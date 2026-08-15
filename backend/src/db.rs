use alloy::primitives::{Address, B256};
use chrono::{DateTime, Utc};
use sqlx::{
    PgPool, Postgres, Row, Transaction,
    postgres::{PgPoolOptions, PgRow},
};
use thiserror::Error;

use crate::{
    chain::{ChainBlock, ChainLog},
    contracts::{
        BinaryOutcome, DecodeError, DecodedEvent, MarketCreatedProjection, PositionTakenProjection,
        decode_position_taken, position_taken_topic,
    },
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

// PostgreSQL integration tests intentionally mutate shared projection tables.
// The lock lives at module scope so tests in API and database modules serialize
// when they use the same disposable TEST_DATABASE_URL under Rust's parallel runner.
#[cfg(test)]
pub(crate) static POSTGRES_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub block_number: u64,
    pub block_hash: B256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventRecord {
    pub log: ChainLog,
    pub event: DecodedEvent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RollbackSummary {
    pub ancestor: Checkpoint,
    pub orphaned_blocks: u64,
    pub orphaned_events: u64,
    pub orphaned_market_projections: u64,
    pub rebuilt_position_events: u64,
}

#[derive(Debug, Error)]
pub enum DbError {
    #[error("database operation failed: {0}")]
    Sql(#[from] sqlx::Error),
    #[error("database migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("block {block_number} timestamp {timestamp} is outside PostgreSQL range")]
    TimestampOutOfRange { block_number: u64, timestamp: u64 },
    #[error("{field} value {value} exceeds the PostgreSQL integer representation")]
    IntegerOutOfRange { field: &'static str, value: u64 },
    #[error("stored {field} has an invalid byte length")]
    CorruptData { field: &'static str },
    #[error("block {block_number} already exists with a different canonical hash")]
    CanonicalBlockConflict { block_number: u64 },
    #[error("event identity already exists with different log contents")]
    EventIdentityConflict,
    #[error("market identity already exists with different projected contents")]
    MarketIdentityConflict,
    #[error("checkpoint update would move backward from block {stored} to {attempted}")]
    CheckpointRegression { stored: u64, attempted: u64 },
    #[error("rollback ancestor block {block_number} is missing from indexed_blocks")]
    RollbackAncestorMissing { block_number: u64 },
    #[error(
        "rollback ancestor block {block_number} has stored hash {stored_hash}, expected {expected_hash}"
    )]
    RollbackAncestorHashMismatch {
        block_number: u64,
        expected_hash: B256,
        stored_hash: B256,
    },
    #[error(
        "PositionTaken historical coverage is not proven for chain {chain_id}, contract {contract_address}; run the indexer with --full-reindex"
    )]
    PositionFullReindexRequired {
        chain_id: u64,
        contract_address: Address,
    },
    #[error(
        "retained PositionTaken event at block {block_number}, transaction {transaction_hash}, log {log_index} cannot be decoded: {source}"
    )]
    RetainedPositionDecode {
        block_number: u64,
        transaction_hash: B256,
        log_index: u64,
        #[source]
        source: DecodeError,
    },
}

impl Database {
    pub async fn connect(database_url: &str) -> Result<Self, DbError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn migrate(&self) -> Result<(), DbError> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    /// Establishes that this contract has PositionTaken coverage from deployment.
    /// Existing pre-milestone state has no such proof and must be explicitly rebuilt.
    pub async fn ensure_position_coverage(
        &self,
        chain_id: u64,
        contract_address: Address,
        deployment_block: u64,
    ) -> Result<(), DbError> {
        let chain_id_db = db_i64("chain_id", chain_id)?;
        let deployment_block_db = db_i64("deployment_block", deployment_block)?;
        let mut transaction = self.pool.begin().await?;
        lock_chain_transaction(&mut transaction, chain_id_db).await?;

        let recorded_from: Option<i64> = sqlx::query_scalar(
            "SELECT position_taken_from_block
             FROM indexer_contract_coverage
             WHERE chain_id = $1 AND contract_address = $2
             FOR UPDATE",
        )
        .bind(chain_id_db)
        .bind(contract_address.as_slice())
        .fetch_optional(&mut *transaction)
        .await?;

        if recorded_from == Some(deployment_block_db) {
            transaction.commit().await?;
            return Ok(());
        }

        let has_existing_state: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM indexed_blocks WHERE chain_id = $1
             ) OR EXISTS (
                 SELECT 1 FROM blockchain_events
                 WHERE chain_id = $1 AND contract_address = $2
             ) OR EXISTS (
                 SELECT 1 FROM markets
                 WHERE chain_id = $1 AND contract_address = $2
             ) OR EXISTS (
                 SELECT 1 FROM indexer_checkpoints
                 WHERE chain_id = $1 AND contract_address = $2
             )",
        )
        .bind(chain_id_db)
        .bind(contract_address.as_slice())
        .fetch_one(&mut *transaction)
        .await?;

        if recorded_from.is_some() || has_existing_state {
            return Err(DbError::PositionFullReindexRequired {
                chain_id,
                contract_address,
            });
        }

        sqlx::query(
            "INSERT INTO indexer_contract_coverage
                (chain_id, contract_address, position_taken_from_block)
             VALUES ($1, $2, $3)",
        )
        .bind(chain_id_db)
        .bind(contract_address.as_slice())
        .bind(deployment_block_db)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Explicit destructive prototype reindex. Because indexed_blocks is keyed by
    /// chain rather than contract, the documented model permits one indexed
    /// Foresyn contract per chain and this clears that chain's index state.
    pub async fn full_reindex(
        &self,
        chain_id: u64,
        contract_address: Address,
        deployment_block: u64,
    ) -> Result<(), DbError> {
        let chain_id_db = db_i64("chain_id", chain_id)?;
        let deployment_block_db = db_i64("deployment_block", deployment_block)?;
        let mut transaction = self.pool.begin().await?;
        lock_chain_transaction(&mut transaction, chain_id_db).await?;

        sqlx::query(
            "DELETE FROM market_positions
             WHERE chain_id = $1 AND contract_address = $2",
        )
        .bind(chain_id_db)
        .bind(contract_address.as_slice())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "DELETE FROM market_states
             WHERE chain_id = $1 AND contract_address = $2",
        )
        .bind(chain_id_db)
        .bind(contract_address.as_slice())
        .execute(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM indexed_blocks WHERE chain_id = $1")
            .bind(chain_id_db)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM indexer_contract_coverage WHERE chain_id = $1")
            .bind(chain_id_db)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT INTO indexer_contract_coverage
                (chain_id, contract_address, position_taken_from_block)
             VALUES ($1, $2, $3)",
        )
        .bind(chain_id_db)
        .bind(contract_address.as_slice())
        .bind(deployment_block_db)
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;
        Ok(())
    }

    pub async fn checkpoint(
        &self,
        chain_id: u64,
        contract_address: Address,
    ) -> Result<Option<Checkpoint>, DbError> {
        let row = sqlx::query(
            "SELECT last_block_number, last_block_hash
             FROM indexer_checkpoints
             WHERE chain_id = $1 AND contract_address = $2",
        )
        .bind(db_i64("chain_id", chain_id)?)
        .bind(contract_address.as_slice())
        .fetch_optional(&self.pool)
        .await?;

        row.map(|row| {
            let number: i64 = row.try_get("last_block_number")?;
            let hash: Vec<u8> = row.try_get("last_block_hash")?;
            Ok(Checkpoint {
                block_number: u64::try_from(number).map_err(|_| DbError::CorruptData {
                    field: "last_block_number",
                })?,
                block_hash: fixed_bytes("last_block_hash", &hash)?,
            })
        })
        .transpose()
    }

    pub async fn indexed_block_hash(
        &self,
        chain_id: u64,
        block_number: u64,
    ) -> Result<Option<B256>, DbError> {
        let hash: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT block_hash
             FROM indexed_blocks
             WHERE chain_id = $1 AND block_number = $2",
        )
        .bind(db_i64("chain_id", chain_id)?)
        .bind(db_i64("block_number", block_number)?)
        .fetch_optional(&self.pool)
        .await?;

        hash.map(|hash| fixed_bytes("block_hash", &hash))
            .transpose()
    }

    pub async fn rollback_to_ancestor(
        &self,
        chain_id: u64,
        contract_address: Address,
        deployment_block: u64,
        ancestor: &Checkpoint,
    ) -> Result<RollbackSummary, DbError> {
        let mut transaction = self.pool.begin().await?;
        let summary = self
            .rollback_chain_transaction(
                &mut transaction,
                chain_id,
                contract_address,
                deployment_block,
                ancestor,
            )
            .await?;
        transaction.commit().await?;
        Ok(summary)
    }

    pub(crate) async fn rollback_chain_transaction(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        chain_id: u64,
        contract_address: Address,
        deployment_block: u64,
        ancestor: &Checkpoint,
    ) -> Result<RollbackSummary, DbError> {
        let chain_id = db_i64("chain_id", chain_id)?;
        let ancestor_number = db_i64("block_number", ancestor.block_number)?;
        let deployment_block = db_i64("deployment_block", deployment_block)?;

        lock_chain_transaction(transaction, chain_id).await?;

        // Serialize a rewind against checkpoint writers for this chain. The current
        // schema is chain-scoped at the block layer, so every same-chain checkpoint
        // participates in this destructive transaction.
        sqlx::query(
            "SELECT contract_address
             FROM indexer_checkpoints
             WHERE chain_id = $1
             FOR UPDATE",
        )
        .bind(chain_id)
        .fetch_all(&mut **transaction)
        .await?;

        let stored_hash: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT block_hash
             FROM indexed_blocks
             WHERE chain_id = $1 AND block_number = $2
             FOR UPDATE",
        )
        .bind(chain_id)
        .bind(ancestor_number)
        .fetch_optional(&mut **transaction)
        .await?;
        let Some(stored_hash) = stored_hash else {
            return Err(DbError::RollbackAncestorMissing {
                block_number: ancestor.block_number,
            });
        };
        let stored_hash = fixed_bytes("block_hash", &stored_hash)?;
        if stored_hash != ancestor.block_hash {
            return Err(DbError::RollbackAncestorHashMismatch {
                block_number: ancestor.block_number,
                expected_hash: ancestor.block_hash,
                stored_hash,
            });
        }

        let orphan_counts = sqlx::query(
            "SELECT
                (SELECT count(*) FROM blockchain_events
                 WHERE chain_id = $1 AND block_number > $2) AS orphaned_events,
                (SELECT count(*) FROM markets
                 WHERE chain_id = $1 AND creation_block_number > $2)
                    AS orphaned_market_projections",
        )
        .bind(chain_id)
        .bind(ancestor_number)
        .fetch_one(&mut **transaction)
        .await?;
        let orphaned_events =
            db_count("orphaned_events", orphan_counts.try_get("orphaned_events")?)?;
        let orphaned_market_projections = db_count(
            "orphaned_market_projections",
            orphan_counts.try_get("orphaned_market_projections")?,
        )?;

        let delete_result = sqlx::query(
            "DELETE FROM indexed_blocks
             WHERE chain_id = $1 AND block_number > $2",
        )
        .bind(chain_id)
        .bind(ancestor_number)
        .execute(&mut **transaction)
        .await?;

        // Mutable projections cannot be rolled back by row provenance alone: an
        // orphaned update may overwrite state accumulated before the ancestor.
        sqlx::query(
            "DELETE FROM market_positions
             WHERE chain_id = $1 AND contract_address = $2",
        )
        .bind(chain_id)
        .bind(contract_address.as_slice())
        .execute(&mut **transaction)
        .await?;
        sqlx::query(
            "DELETE FROM market_states
             WHERE chain_id = $1 AND contract_address = $2",
        )
        .bind(chain_id)
        .bind(contract_address.as_slice())
        .execute(&mut **transaction)
        .await?;

        let retained_events = sqlx::query(
            "SELECT block_number, transaction_hash, log_index, topics, data
             FROM blockchain_events
             WHERE chain_id = $1
               AND contract_address = $2
               AND block_number BETWEEN $3 AND $4
               AND topics[1] = $5
             ORDER BY block_number, transaction_index, log_index",
        )
        .bind(chain_id)
        .bind(contract_address.as_slice())
        .bind(deployment_block)
        .bind(ancestor_number)
        .bind(position_taken_topic().as_slice())
        .fetch_all(&mut **transaction)
        .await?;

        for row in &retained_events {
            replay_retained_position(transaction, chain_id, contract_address, row).await?;
        }

        // Deleting the old checkpoint through the block FK and recreating it here
        // makes the rewind durable in the same transaction as the cascade cleanup.
        sqlx::query(
            "INSERT INTO indexer_checkpoints
                (chain_id, contract_address, last_block_number, last_block_hash)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (chain_id, contract_address) DO UPDATE SET
                last_block_number = EXCLUDED.last_block_number,
                last_block_hash = EXCLUDED.last_block_hash,
                updated_at = now()",
        )
        .bind(chain_id)
        .bind(contract_address.as_slice())
        .bind(ancestor_number)
        .bind(ancestor.block_hash.as_slice())
        .execute(&mut **transaction)
        .await?;

        Ok(RollbackSummary {
            ancestor: ancestor.clone(),
            orphaned_blocks: delete_result.rows_affected(),
            orphaned_events,
            orphaned_market_projections,
            rebuilt_position_events: retained_events.len() as u64,
        })
    }

    pub async fn commit_block(
        &self,
        chain_id: u64,
        contract_address: Address,
        block: &ChainBlock,
        events: &[EventRecord],
    ) -> Result<(), DbError> {
        let mut transaction = self.pool.begin().await?;
        self.persist_block_transaction(&mut transaction, chain_id, contract_address, block, events)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub(crate) async fn persist_block_transaction(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        chain_id: u64,
        contract_address: Address,
        block: &ChainBlock,
        events: &[EventRecord],
    ) -> Result<(), DbError> {
        let chain_id = db_i64("chain_id", chain_id)?;
        let block_number = db_i64("block_number", block.number)?;
        let block_timestamp =
            DateTime::<Utc>::from_timestamp(db_i64("block_timestamp", block.timestamp)?, 0).ok_or(
                DbError::TimestampOutOfRange {
                    block_number: block.number,
                    timestamp: block.timestamp,
                },
            )?;

        lock_chain_transaction(transaction, chain_id).await?;

        sqlx::query(
            "INSERT INTO indexed_blocks
                (chain_id, block_number, block_hash, parent_hash, block_timestamp)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (chain_id, block_number) DO NOTHING",
        )
        .bind(chain_id)
        .bind(block_number)
        .bind(block.hash.as_slice())
        .bind(block.parent_hash.as_slice())
        .bind(block_timestamp)
        .execute(&mut **transaction)
        .await?;

        let stored_hash: Vec<u8> = sqlx::query_scalar(
            "SELECT block_hash FROM indexed_blocks
             WHERE chain_id = $1 AND block_number = $2",
        )
        .bind(chain_id)
        .bind(block_number)
        .fetch_one(&mut **transaction)
        .await?;
        if stored_hash.as_slice() != block.hash.as_slice() {
            return Err(DbError::CanonicalBlockConflict {
                block_number: block.number,
            });
        }

        for event in events {
            let inserted = insert_raw_event(transaction, chain_id, block, event).await?;
            if inserted {
                match &event.event {
                    DecodedEvent::MarketCreated(projection) => {
                        insert_market_projection(
                            transaction,
                            chain_id,
                            contract_address,
                            block,
                            &event.log,
                            projection,
                        )
                        .await?;
                    }
                    DecodedEvent::PositionTaken(projection) => {
                        apply_position_projection(
                            transaction,
                            chain_id,
                            contract_address,
                            block.number,
                            projection,
                        )
                        .await?;
                    }
                }
            }
        }

        let checkpoint_result = sqlx::query(
            "INSERT INTO indexer_checkpoints
                (chain_id, contract_address, last_block_number, last_block_hash)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (chain_id, contract_address) DO UPDATE SET
                last_block_number = EXCLUDED.last_block_number,
                last_block_hash = EXCLUDED.last_block_hash,
                updated_at = now()
             WHERE indexer_checkpoints.last_block_number <= EXCLUDED.last_block_number",
        )
        .bind(chain_id)
        .bind(contract_address.as_slice())
        .bind(block_number)
        .bind(block.hash.as_slice())
        .execute(&mut **transaction)
        .await?;

        if checkpoint_result.rows_affected() == 0 {
            let stored: i64 = sqlx::query_scalar(
                "SELECT last_block_number FROM indexer_checkpoints
                 WHERE chain_id = $1 AND contract_address = $2",
            )
            .bind(chain_id)
            .bind(contract_address.as_slice())
            .fetch_one(&mut **transaction)
            .await?;
            return Err(DbError::CheckpointRegression {
                stored: u64::try_from(stored).map_err(|_| DbError::CorruptData {
                    field: "last_block_number",
                })?,
                attempted: block.number,
            });
        }

        Ok(())
    }
}

async fn insert_raw_event(
    transaction: &mut Transaction<'_, Postgres>,
    chain_id: i64,
    block: &ChainBlock,
    event: &EventRecord,
) -> Result<bool, DbError> {
    let topics: Vec<Vec<u8>> = event
        .log
        .topics
        .iter()
        .map(|topic| topic.as_slice().to_vec())
        .collect();
    let transaction_index = db_i32("transaction_index", event.log.transaction_index)?;
    let log_index = db_i32("log_index", event.log.log_index)?;

    let result = sqlx::query(
        "INSERT INTO blockchain_events
            (chain_id, block_number, block_hash, transaction_hash, transaction_index,
             log_index, contract_address, topics, data)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (chain_id, transaction_hash, log_index) DO NOTHING",
    )
    .bind(chain_id)
    .bind(db_i64("block_number", block.number)?)
    .bind(block.hash.as_slice())
    .bind(event.log.transaction_hash.as_slice())
    .bind(transaction_index)
    .bind(log_index)
    .bind(event.log.address.as_slice())
    .bind(&topics)
    .bind(&event.log.data)
    .execute(&mut **transaction)
    .await?;

    if result.rows_affected() == 1 {
        return Ok(true);
    }

    let row = sqlx::query(
        "SELECT block_number, block_hash, transaction_index, contract_address, topics, data
         FROM blockchain_events
         WHERE chain_id = $1 AND transaction_hash = $2 AND log_index = $3",
    )
    .bind(chain_id)
    .bind(event.log.transaction_hash.as_slice())
    .bind(log_index)
    .fetch_one(&mut **transaction)
    .await?;

    let identical = row.try_get::<i64, _>("block_number")? == db_i64("block_number", block.number)?
        && row.try_get::<Vec<u8>, _>("block_hash")?.as_slice() == block.hash.as_slice()
        && row.try_get::<i32, _>("transaction_index")? == transaction_index
        && row.try_get::<Vec<u8>, _>("contract_address")?.as_slice()
            == event.log.address.as_slice()
        && row.try_get::<Vec<Vec<u8>>, _>("topics")? == topics
        && row.try_get::<Vec<u8>, _>("data")? == event.log.data;

    if identical {
        Ok(false)
    } else {
        Err(DbError::EventIdentityConflict)
    }
}

async fn lock_chain_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    chain_id: i64,
) -> Result<(), DbError> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(chain_id)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn insert_market_projection(
    transaction: &mut Transaction<'_, Postgres>,
    chain_id: i64,
    contract_address: Address,
    block: &ChainBlock,
    log: &ChainLog,
    projection: &MarketCreatedProjection,
) -> Result<(), DbError> {
    let market_id = projection.market_id.to_string();
    let deadline = projection.deadline.to_string();
    let result = sqlx::query(
        "INSERT INTO markets
            (chain_id, contract_address, market_id, resolver, creator, deadline,
             metadata_digest, creation_block_number, creation_transaction_hash)
         VALUES ($1, $2, $3::numeric, $4, $5, $6::numeric, $7, $8, $9)
         ON CONFLICT (chain_id, contract_address, market_id) DO NOTHING",
    )
    .bind(chain_id)
    .bind(contract_address.as_slice())
    .bind(&market_id)
    .bind(projection.resolver.as_slice())
    .bind(projection.creator.as_slice())
    .bind(&deadline)
    .bind(projection.metadata_digest.as_slice())
    .bind(db_i64("creation_block_number", block.number)?)
    .bind(log.transaction_hash.as_slice())
    .execute(&mut **transaction)
    .await?;

    if result.rows_affected() == 1 {
        return Ok(());
    }

    let row = sqlx::query(
        "SELECT resolver, creator, deadline::text, metadata_digest,
                creation_block_number, creation_transaction_hash
         FROM markets
         WHERE chain_id = $1 AND contract_address = $2 AND market_id = $3::numeric",
    )
    .bind(chain_id)
    .bind(contract_address.as_slice())
    .bind(&market_id)
    .fetch_one(&mut **transaction)
    .await?;

    let identical = row.try_get::<Vec<u8>, _>("resolver")?.as_slice()
        == projection.resolver.as_slice()
        && row.try_get::<Vec<u8>, _>("creator")?.as_slice() == projection.creator.as_slice()
        && row.try_get::<String, _>("deadline")? == deadline
        && row.try_get::<Vec<u8>, _>("metadata_digest")?.as_slice()
            == projection.metadata_digest.as_slice()
        && row.try_get::<i64, _>("creation_block_number")?
            == db_i64("creation_block_number", block.number)?
        && row
            .try_get::<Vec<u8>, _>("creation_transaction_hash")?
            .as_slice()
            == log.transaction_hash.as_slice();

    if identical {
        Ok(())
    } else {
        Err(DbError::MarketIdentityConflict)
    }
}

async fn apply_position_projection(
    transaction: &mut Transaction<'_, Postgres>,
    chain_id: i64,
    contract_address: Address,
    block_number: u64,
    projection: &PositionTakenProjection,
) -> Result<(), DbError> {
    let market_id = projection.market_id.to_string();
    let yes_pool = projection.yes_pool.to_string();
    let no_pool = projection.no_pool.to_string();
    let user_outcome_stake = projection.user_outcome_stake.to_string();
    let block_number = db_i64("updated_block_number", block_number)?;

    sqlx::query(
        "INSERT INTO market_states
            (chain_id, contract_address, market_id, yes_pool, no_pool, updated_block_number)
         VALUES ($1, $2, $3::numeric, $4::numeric, $5::numeric, $6)
         ON CONFLICT (chain_id, contract_address, market_id) DO UPDATE SET
            yes_pool = EXCLUDED.yes_pool,
            no_pool = EXCLUDED.no_pool,
            updated_block_number = EXCLUDED.updated_block_number",
    )
    .bind(chain_id)
    .bind(contract_address.as_slice())
    .bind(&market_id)
    .bind(&yes_pool)
    .bind(&no_pool)
    .bind(block_number)
    .execute(&mut **transaction)
    .await?;

    match projection.outcome {
        BinaryOutcome::Yes => {
            sqlx::query(
                "INSERT INTO market_positions
                    (chain_id, contract_address, market_id, user_address,
                     yes_stake, no_stake, updated_block_number)
                 VALUES ($1, $2, $3::numeric, $4, $5::numeric, 0, $6)
                 ON CONFLICT (chain_id, contract_address, market_id, user_address)
                 DO UPDATE SET
                    yes_stake = EXCLUDED.yes_stake,
                    updated_block_number = EXCLUDED.updated_block_number",
            )
            .bind(chain_id)
            .bind(contract_address.as_slice())
            .bind(&market_id)
            .bind(projection.user.as_slice())
            .bind(&user_outcome_stake)
            .bind(block_number)
            .execute(&mut **transaction)
            .await?;
        }
        BinaryOutcome::No => {
            sqlx::query(
                "INSERT INTO market_positions
                    (chain_id, contract_address, market_id, user_address,
                     yes_stake, no_stake, updated_block_number)
                 VALUES ($1, $2, $3::numeric, $4, 0, $5::numeric, $6)
                 ON CONFLICT (chain_id, contract_address, market_id, user_address)
                 DO UPDATE SET
                    no_stake = EXCLUDED.no_stake,
                    updated_block_number = EXCLUDED.updated_block_number",
            )
            .bind(chain_id)
            .bind(contract_address.as_slice())
            .bind(&market_id)
            .bind(projection.user.as_slice())
            .bind(&user_outcome_stake)
            .bind(block_number)
            .execute(&mut **transaction)
            .await?;
        }
    }

    Ok(())
}

async fn replay_retained_position(
    transaction: &mut Transaction<'_, Postgres>,
    chain_id: i64,
    contract_address: Address,
    row: &PgRow,
) -> Result<(), DbError> {
    let block_number_db: i64 = row.try_get("block_number")?;
    let block_number = u64::try_from(block_number_db).map_err(|_| DbError::CorruptData {
        field: "block_number",
    })?;
    let transaction_hash = fixed_bytes(
        "transaction_hash",
        &row.try_get::<Vec<u8>, _>("transaction_hash")?,
    )?;
    let log_index_db: i32 = row.try_get("log_index")?;
    let log_index =
        u64::try_from(log_index_db).map_err(|_| DbError::CorruptData { field: "log_index" })?;
    let stored_topics: Vec<Vec<u8>> = row.try_get("topics")?;
    let topics = stored_topics
        .iter()
        .map(|topic| fixed_bytes("topic", topic))
        .collect::<Result<Vec<_>, _>>()?;
    let data: Vec<u8> = row.try_get("data")?;
    let projection = decode_position_taken(&topics, &data).map_err(|source| {
        DbError::RetainedPositionDecode {
            block_number,
            transaction_hash,
            log_index,
            source,
        }
    })?;

    apply_position_projection(
        transaction,
        chain_id,
        contract_address,
        block_number,
        &projection,
    )
    .await
}

fn db_i64(field: &'static str, value: u64) -> Result<i64, DbError> {
    i64::try_from(value).map_err(|_| DbError::IntegerOutOfRange { field, value })
}

fn db_i32(field: &'static str, value: u64) -> Result<i32, DbError> {
    i32::try_from(value).map_err(|_| DbError::IntegerOutOfRange { field, value })
}

fn db_count(field: &'static str, value: i64) -> Result<u64, DbError> {
    u64::try_from(value).map_err(|_| DbError::CorruptData { field })
}

fn fixed_bytes(field: &'static str, value: &[u8]) -> Result<B256, DbError> {
    B256::try_from(value).map_err(|_| DbError::CorruptData { field })
}

#[cfg(test)]
mod tests {
    use alloy::{
        primitives::{Address, B256, U256},
        sol_types::SolEvent,
    };
    use sqlx::{PgPool, Row, postgres::PgPoolOptions};

    use super::{Database, EventRecord};
    use crate::{
        chain::{ChainBlock, ChainLog},
        contracts::{
            BinaryOutcome, DecodedEvent, MarketCreatedProjection, Outcome, PositionTaken,
            decode_position_taken, market_created_topic,
        },
    };

    async fn integration_pool() -> Option<PgPool> {
        let database_url = std::env::var("TEST_DATABASE_URL").ok()?;
        Some(
            PgPoolOptions::new()
                .max_connections(2)
                .connect(&database_url)
                .await
                .unwrap(),
        )
    }

    fn block(number: u64, hash_byte: u8, parent_byte: u8) -> ChainBlock {
        ChainBlock {
            number,
            hash: B256::repeat_byte(hash_byte),
            parent_hash: B256::repeat_byte(parent_byte),
            timestamp: 1_900_000_000 + number,
        }
    }

    fn event(block: &ChainBlock) -> EventRecord {
        event_with(block, Address::repeat_byte(0x44), U256::MAX, 0x55, 0x88)
    }

    fn event_with(
        block: &ChainBlock,
        contract: Address,
        market_id: U256,
        transaction_byte: u8,
        metadata_byte: u8,
    ) -> EventRecord {
        EventRecord {
            log: ChainLog {
                block_number: block.number,
                block_hash: block.hash,
                transaction_hash: B256::repeat_byte(transaction_byte),
                transaction_index: 3,
                log_index: 7,
                address: contract,
                topics: vec![market_created_topic(), B256::ZERO, B256::ZERO],
                data: vec![0; 96],
            },
            event: DecodedEvent::MarketCreated(MarketCreatedProjection {
                market_id,
                resolver: Address::repeat_byte(0x66),
                creator: Address::repeat_byte(0x77),
                deadline: u64::MAX,
                metadata_digest: B256::repeat_byte(metadata_byte),
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn position_event(
        block: &ChainBlock,
        contract: Address,
        market_id: U256,
        user: Address,
        outcome: BinaryOutcome,
        amount: U256,
        user_outcome_stake: U256,
        yes_pool: U256,
        no_pool: U256,
        transaction_byte: u8,
        transaction_index: u64,
        log_index: u64,
    ) -> EventRecord {
        let abi_outcome = match outcome {
            BinaryOutcome::Yes => Outcome::Yes,
            BinaryOutcome::No => Outcome::No,
        };
        let abi_event = PositionTaken {
            marketId: market_id,
            user,
            outcome: abi_outcome,
            amount,
            userOutcomeStake: user_outcome_stake,
            yesPool: yes_pool,
            noPool: no_pool,
        };
        let encoded = abi_event.encode_log_data();
        let projection = decode_position_taken(encoded.topics(), &encoded.data).unwrap();
        EventRecord {
            log: ChainLog {
                block_number: block.number,
                block_hash: block.hash,
                transaction_hash: B256::repeat_byte(transaction_byte),
                transaction_index,
                log_index,
                address: contract,
                topics: encoded.topics().to_vec(),
                data: encoded.data.to_vec(),
            },
            event: DecodedEvent::PositionTaken(projection),
        }
    }

    #[tokio::test]
    async fn postgres_commit_and_reorg_rollback_are_atomic_idempotent_and_restartable() {
        let Some(pool) = integration_pool().await else {
            eprintln!("skipping PostgreSQL integration test: TEST_DATABASE_URL is not set");
            return;
        };
        let _guard = super::POSTGRES_TEST_LOCK.lock().await;
        let database = Database::from_pool(pool.clone());
        database.migrate().await.unwrap();
        sqlx::query(
            "TRUNCATE indexer_contract_coverage, markets, indexer_checkpoints,
                      blockchain_events, indexed_blocks CASCADE",
        )
        .execute(&pool)
        .await
        .unwrap();

        let chain_id = 31_337;
        let contract = Address::repeat_byte(0x44);
        let first_block = block(100, 0x10, 0x09);
        let record = event(&first_block);

        database
            .commit_block(
                chain_id,
                contract,
                &first_block,
                std::slice::from_ref(&record),
            )
            .await
            .unwrap();
        database
            .commit_block(
                chain_id,
                contract,
                &first_block,
                std::slice::from_ref(&record),
            )
            .await
            .unwrap();

        let raw_count: i64 = sqlx::query_scalar("SELECT count(*) FROM blockchain_events")
            .fetch_one(&pool)
            .await
            .unwrap();
        let market_count: i64 = sqlx::query_scalar("SELECT count(*) FROM markets")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((raw_count, market_count), (1, 1));

        let projection = sqlx::query(
            "SELECT market_id::text, resolver, creator, deadline::text, metadata_digest,
                    creation_block_number, creation_transaction_hash
             FROM markets",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let DecodedEvent::MarketCreated(record_projection) = &record.event else {
            panic!("test record must be MarketCreated");
        };
        assert_eq!(
            projection.try_get::<String, _>("market_id").unwrap(),
            U256::MAX.to_string()
        );
        assert_eq!(
            projection.try_get::<Vec<u8>, _>("resolver").unwrap(),
            record_projection.resolver.as_slice()
        );
        assert_eq!(
            projection.try_get::<Vec<u8>, _>("creator").unwrap(),
            record_projection.creator.as_slice()
        );
        assert_eq!(
            projection.try_get::<String, _>("deadline").unwrap(),
            u64::MAX.to_string()
        );
        assert_eq!(
            projection.try_get::<Vec<u8>, _>("metadata_digest").unwrap(),
            record_projection.metadata_digest.as_slice()
        );
        assert_eq!(
            projection
                .try_get::<i64, _>("creation_block_number")
                .unwrap(),
            100
        );
        assert_eq!(
            projection
                .try_get::<Vec<u8>, _>("creation_transaction_hash")
                .unwrap(),
            record.log.transaction_hash.as_slice()
        );

        let checkpoint = database
            .checkpoint(chain_id, contract)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint.block_number, 100);
        assert_eq!(checkpoint.block_hash, first_block.hash);

        let second_block = block(101, 0x11, 0x10);
        let mut transaction = pool.begin().await.unwrap();
        database
            .persist_block_transaction(&mut transaction, chain_id, contract, &second_block, &[])
            .await
            .unwrap();
        transaction.rollback().await.unwrap();

        let checkpoint_after_rollback = database
            .checkpoint(chain_id, contract)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint_after_rollback.block_number, 100);
        let second_block_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM indexed_blocks WHERE block_number = 101")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(second_block_count, 0);

        sqlx::query(
            "TRUNCATE indexer_contract_coverage, markets, indexer_checkpoints,
                      blockchain_events, indexed_blocks CASCADE",
        )
        .execute(&pool)
        .await
        .unwrap();

        let other_chain_id = 31_338;
        let other_contract = Address::repeat_byte(0xaa);
        let ancestor = block(100, 0xa0, 0x99);
        let orphaned_101 = block(101, 0xb1, 0xa0);
        let orphaned_102 = block(102, 0xb2, 0xb1);
        let ancestor_event = event_with(&ancestor, contract, U256::from(1), 0x11, 0x21);
        let orphaned_event_101 = event_with(&orphaned_101, contract, U256::from(2), 0x12, 0x22);
        let orphaned_event_102 = event_with(&orphaned_102, contract, U256::from(3), 0x13, 0x23);

        database
            .commit_block(
                chain_id,
                contract,
                &ancestor,
                std::slice::from_ref(&ancestor_event),
            )
            .await
            .unwrap();
        database
            .commit_block(
                chain_id,
                contract,
                &orphaned_101,
                std::slice::from_ref(&orphaned_event_101),
            )
            .await
            .unwrap();
        database
            .commit_block(
                chain_id,
                contract,
                &orphaned_102,
                std::slice::from_ref(&orphaned_event_102),
            )
            .await
            .unwrap();

        let other_100 = block(100, 0xd0, 0xcf);
        let other_101 = block(101, 0xd1, 0xd0);
        let other_102 = block(102, 0xd2, 0xd1);
        let other_event = event_with(&other_102, other_contract, U256::from(90), 0x91, 0x92);
        database
            .commit_block(other_chain_id, other_contract, &other_100, &[])
            .await
            .unwrap();
        database
            .commit_block(other_chain_id, other_contract, &other_101, &[])
            .await
            .unwrap();
        database
            .commit_block(
                other_chain_id,
                other_contract,
                &other_102,
                std::slice::from_ref(&other_event),
            )
            .await
            .unwrap();

        let rollback = database
            .rollback_to_ancestor(
                chain_id,
                contract,
                100,
                &super::Checkpoint {
                    block_number: ancestor.number,
                    block_hash: ancestor.hash,
                },
            )
            .await
            .unwrap();
        assert_eq!(rollback.orphaned_blocks, 2);
        assert_eq!(rollback.orphaned_events, 2);
        assert_eq!(rollback.orphaned_market_projections, 2);

        let remaining_blocks: i64 =
            sqlx::query_scalar("SELECT count(*) FROM indexed_blocks WHERE chain_id = $1")
                .bind(i64::try_from(chain_id).unwrap())
                .fetch_one(&pool)
                .await
                .unwrap();
        let remaining_events: i64 =
            sqlx::query_scalar("SELECT count(*) FROM blockchain_events WHERE chain_id = $1")
                .bind(i64::try_from(chain_id).unwrap())
                .fetch_one(&pool)
                .await
                .unwrap();
        let remaining_markets: i64 =
            sqlx::query_scalar("SELECT count(*) FROM markets WHERE chain_id = $1")
                .bind(i64::try_from(chain_id).unwrap())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (remaining_blocks, remaining_events, remaining_markets),
            (1, 1, 1)
        );
        assert_eq!(
            database.indexed_block_hash(chain_id, 100).await.unwrap(),
            Some(ancestor.hash)
        );
        assert_eq!(
            database.indexed_block_hash(chain_id, 101).await.unwrap(),
            None
        );
        assert_eq!(
            database
                .checkpoint(chain_id, contract)
                .await
                .unwrap()
                .unwrap(),
            super::Checkpoint {
                block_number: 100,
                block_hash: ancestor.hash,
            }
        );

        // A stop after rollback commit is restartable from the durable ancestor.
        let restarted_database = Database::from_pool(pool.clone());
        assert_eq!(
            restarted_database
                .checkpoint(chain_id, contract)
                .await
                .unwrap()
                .unwrap()
                .block_number,
            100
        );

        let canonical_101 = block(101, 0xc1, 0xa0);
        let canonical_102 = block(102, 0xc2, 0xc1);
        let replacement_event_101 = event_with(&canonical_101, contract, U256::from(2), 0x31, 0x41);
        let replacement_event_102 = event_with(&canonical_102, contract, U256::from(4), 0x32, 0x42);
        for _ in 0..2 {
            restarted_database
                .commit_block(
                    chain_id,
                    contract,
                    &canonical_101,
                    std::slice::from_ref(&replacement_event_101),
                )
                .await
                .unwrap();
        }
        for _ in 0..2 {
            restarted_database
                .commit_block(
                    chain_id,
                    contract,
                    &canonical_102,
                    std::slice::from_ref(&replacement_event_102),
                )
                .await
                .unwrap();
        }

        let replay_counts: (i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT count(*) FROM blockchain_events WHERE chain_id = $1),
                (SELECT count(*) FROM markets WHERE chain_id = $1)",
        )
        .bind(i64::try_from(chain_id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(replay_counts, (3, 3));
        let replacement_digest: Vec<u8> = sqlx::query_scalar(
            "SELECT metadata_digest FROM markets
             WHERE chain_id = $1 AND contract_address = $2 AND market_id = 2",
        )
        .bind(i64::try_from(chain_id).unwrap())
        .bind(contract.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(replacement_digest, B256::repeat_byte(0x41).as_slice());
        let orphaned_market_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM markets WHERE chain_id = $1 AND market_id = 3",
        )
        .bind(i64::try_from(chain_id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(orphaned_market_count, 0);

        let canonical_103 = block(103, 0xc3, 0xc2);
        let event_103 = event_with(&canonical_103, contract, U256::from(5), 0x33, 0x43);
        restarted_database
            .commit_block(
                chain_id,
                contract,
                &canonical_103,
                std::slice::from_ref(&event_103),
            )
            .await
            .unwrap();
        let counts_before_failed_rollback: (i64, i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT count(*) FROM indexed_blocks WHERE chain_id = $1),
                (SELECT count(*) FROM blockchain_events WHERE chain_id = $1),
                (SELECT count(*) FROM markets WHERE chain_id = $1)",
        )
        .bind(i64::try_from(chain_id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();

        let mut failed_transaction = pool.begin().await.unwrap();
        restarted_database
            .rollback_chain_transaction(
                &mut failed_transaction,
                chain_id,
                contract,
                100,
                &super::Checkpoint {
                    block_number: ancestor.number,
                    block_hash: ancestor.hash,
                },
            )
            .await
            .unwrap();
        assert!(
            sqlx::query("SELECT 1 / 0")
                .execute(&mut *failed_transaction)
                .await
                .is_err()
        );
        failed_transaction.rollback().await.unwrap();

        let counts_after_failed_rollback: (i64, i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT count(*) FROM indexed_blocks WHERE chain_id = $1),
                (SELECT count(*) FROM blockchain_events WHERE chain_id = $1),
                (SELECT count(*) FROM markets WHERE chain_id = $1)",
        )
        .bind(i64::try_from(chain_id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(counts_after_failed_rollback, counts_before_failed_rollback);
        assert_eq!(
            restarted_database
                .checkpoint(chain_id, contract)
                .await
                .unwrap()
                .unwrap()
                .block_number,
            103
        );

        let other_chain_counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT count(*) FROM indexed_blocks WHERE chain_id = $1),
                (SELECT count(*) FROM blockchain_events WHERE chain_id = $1),
                (SELECT count(*) FROM markets WHERE chain_id = $1)",
        )
        .bind(i64::try_from(other_chain_id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(other_chain_counts, (3, 1, 1));
        assert_eq!(
            restarted_database
                .checkpoint(other_chain_id, other_contract)
                .await
                .unwrap()
                .unwrap()
                .block_number,
            102
        );
    }

    #[tokio::test]
    async fn postgres_position_projections_reindex_and_reorg_rebuild_are_deterministic() {
        let Some(pool) = integration_pool().await else {
            eprintln!("skipping PostgreSQL integration test: TEST_DATABASE_URL is not set");
            return;
        };
        let _guard = super::POSTGRES_TEST_LOCK.lock().await;
        let database = Database::from_pool(pool.clone());
        database.migrate().await.unwrap();
        sqlx::query(
            "TRUNCATE indexer_contract_coverage, markets, indexer_checkpoints,
                      blockchain_events, indexed_blocks CASCADE",
        )
        .execute(&pool)
        .await
        .unwrap();

        let chain_id = 31_337;
        let contract = Address::repeat_byte(0x44);
        let other_chain_id = 31_338;
        let other_contract = Address::repeat_byte(0xaa);
        let alice = Address::repeat_byte(0xa1);
        let bob = Address::repeat_byte(0xb2);
        let carol = Address::repeat_byte(0xc3);
        let dave = Address::repeat_byte(0xd4);

        // Seed another chain first; neither explicit full reindex nor later rollback
        // for chain_id may alter it.
        let other_200 = block(200, 0xe0, 0xdf);
        let other_market = event_with(&other_200, other_contract, U256::from(7), 0xe1, 0xe2);
        database
            .commit_block(
                other_chain_id,
                other_contract,
                &other_200,
                std::slice::from_ref(&other_market),
            )
            .await
            .unwrap();
        let other_201 = block(201, 0xe3, 0xe0);
        let other_position = position_event(
            &other_201,
            other_contract,
            U256::from(7),
            alice,
            BinaryOutcome::Yes,
            U256::from(8),
            U256::from(8),
            U256::from(8),
            U256::ZERO,
            0xe4,
            0,
            0,
        );
        database
            .commit_block(
                other_chain_id,
                other_contract,
                &other_201,
                std::slice::from_ref(&other_position),
            )
            .await
            .unwrap();

        // A checkpoint created by the MarketCreated-only milestone has no durable
        // proof of PositionTaken history and normal startup must leave it intact.
        let legacy_100 = block(100, 0x10, 0x0f);
        let legacy_market = event_with(&legacy_100, contract, U256::from(1), 0x11, 0x12);
        database
            .commit_block(
                chain_id,
                contract,
                &legacy_100,
                std::slice::from_ref(&legacy_market),
            )
            .await
            .unwrap();
        let error = database
            .ensure_position_coverage(chain_id, contract, 100)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            super::DbError::PositionFullReindexRequired {
                chain_id: 31_337,
                contract_address
            } if contract_address == contract
        ));
        assert_eq!(
            database
                .checkpoint(chain_id, contract)
                .await
                .unwrap()
                .unwrap()
                .block_number,
            100
        );

        database
            .full_reindex(chain_id, contract, 100)
            .await
            .unwrap();
        assert!(
            database
                .checkpoint(chain_id, contract)
                .await
                .unwrap()
                .is_none()
        );
        database
            .ensure_position_coverage(chain_id, contract, 100)
            .await
            .unwrap();
        let other_checkpoint = database
            .checkpoint(other_chain_id, other_contract)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(other_checkpoint.block_number, 201);

        let canonical_100 = block(100, 0xa0, 0x9f);
        let market = event_with(&canonical_100, contract, U256::from(1), 0x20, 0x21);
        database
            .commit_block(
                chain_id,
                contract,
                &canonical_100,
                std::slice::from_ref(&market),
            )
            .await
            .unwrap();

        let canonical_101 = block(101, 0xa1, 0xa0);
        let alice_yes_2 = position_event(
            &canonical_101,
            contract,
            U256::from(1),
            alice,
            BinaryOutcome::Yes,
            U256::from(2),
            U256::from(2),
            U256::from(2),
            U256::ZERO,
            0x31,
            0,
            0,
        );
        database
            .commit_block(
                chain_id,
                contract,
                &canonical_101,
                std::slice::from_ref(&alice_yes_2),
            )
            .await
            .unwrap();
        // Raw identity is the idempotency gate; the duplicate cannot reapply state.
        database
            .commit_block(
                chain_id,
                contract,
                &canonical_101,
                std::slice::from_ref(&alice_yes_2),
            )
            .await
            .unwrap();

        let canonical_102 = block(102, 0xa2, 0xa1);
        let bob_no_3 = position_event(
            &canonical_102,
            contract,
            U256::from(1),
            bob,
            BinaryOutcome::No,
            U256::from(3),
            U256::from(3),
            U256::from(2),
            U256::from(3),
            0x32,
            0,
            0,
        );
        database
            .commit_block(
                chain_id,
                contract,
                &canonical_102,
                std::slice::from_ref(&bob_no_3),
            )
            .await
            .unwrap();

        let canonical_103 = block(103, 0xa3, 0xa2);
        let alice_yes_3 = position_event(
            &canonical_103,
            contract,
            U256::from(1),
            alice,
            BinaryOutcome::Yes,
            U256::from(1),
            U256::from(3),
            U256::from(3),
            U256::from(3),
            0x33,
            0,
            0,
        );
        database
            .commit_block(
                chain_id,
                contract,
                &canonical_103,
                std::slice::from_ref(&alice_yes_3),
            )
            .await
            .unwrap();

        let state_at_103: (String, String, i64) = sqlx::query_as(
            "SELECT yes_pool::text, no_pool::text, updated_block_number
             FROM market_states
             WHERE chain_id = $1 AND contract_address = $2 AND market_id = 1",
        )
        .bind(i64::try_from(chain_id).unwrap())
        .bind(contract.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state_at_103, ("3".into(), "3".into(), 103));
        let positions_at_103: Vec<(Vec<u8>, String, String)> = sqlx::query_as(
            "SELECT user_address, yes_stake::text, no_stake::text
             FROM market_positions
             WHERE chain_id = $1 AND contract_address = $2 AND market_id = 1
             ORDER BY user_address",
        )
        .bind(i64::try_from(chain_id).unwrap())
        .bind(contract.as_slice())
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            positions_at_103,
            vec![
                (alice.as_slice().to_vec(), "3".into(), "0".into()),
                (bob.as_slice().to_vec(), "0".into(), "3".into()),
            ]
        );

        // Same-user NO updates preserve Alice's YES side, and repeated emitted
        // userOutcomeStake values replace rather than accumulate local state.
        let orphaned_104 = block(104, 0xb4, 0xa3);
        let alice_no_7 = position_event(
            &orphaned_104,
            contract,
            U256::from(1),
            alice,
            BinaryOutcome::No,
            U256::from(7),
            U256::from(7),
            U256::from(3),
            U256::from(10),
            0x34,
            0,
            0,
        );
        database
            .commit_block(
                chain_id,
                contract,
                &orphaned_104,
                std::slice::from_ref(&alice_no_7),
            )
            .await
            .unwrap();
        let orphaned_105 = block(105, 0xb5, 0xb4);
        let alice_no_9 = position_event(
            &orphaned_105,
            contract,
            U256::from(1),
            alice,
            BinaryOutcome::No,
            U256::from(2),
            U256::from(9),
            U256::from(3),
            U256::from(12),
            0x35,
            0,
            0,
        );
        database
            .commit_block(
                chain_id,
                contract,
                &orphaned_105,
                std::slice::from_ref(&alice_no_9),
            )
            .await
            .unwrap();
        let alice_both: (String, String) = sqlx::query_as(
            "SELECT yes_stake::text, no_stake::text FROM market_positions
             WHERE chain_id = $1 AND contract_address = $2
               AND market_id = 1 AND user_address = $3",
        )
        .bind(i64::try_from(chain_id).unwrap())
        .bind(contract.as_slice())
        .bind(alice.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(alice_both, ("3".into(), "9".into()));

        // A uint256 maximum round-trips exactly, and the pool is SET to the
        // emitted value (blindly adding amount=1 would produce 4 instead).
        let orphaned_106 = block(106, 0xb6, 0xb5);
        let dave_max = position_event(
            &orphaned_106,
            contract,
            U256::from(1),
            dave,
            BinaryOutcome::Yes,
            U256::from(1),
            U256::MAX,
            U256::MAX,
            U256::from(12),
            0x36,
            0,
            0,
        );
        database
            .commit_block(
                chain_id,
                contract,
                &orphaned_106,
                std::slice::from_ref(&dave_max),
            )
            .await
            .unwrap();
        database
            .commit_block(
                chain_id,
                contract,
                &orphaned_106,
                std::slice::from_ref(&dave_max),
            )
            .await
            .unwrap();
        let exact_max: (String, String, i64) = sqlx::query_as(
            "SELECT s.yes_pool::text, p.yes_stake::text,
                    (SELECT count(*) FROM blockchain_events
                     WHERE chain_id = $1 AND transaction_hash = $4 AND log_index = 0)
             FROM market_states s
             JOIN market_positions p USING (chain_id, contract_address, market_id)
             WHERE s.chain_id = $1 AND s.contract_address = $2
               AND s.market_id = 1 AND p.user_address = $3",
        )
        .bind(i64::try_from(chain_id).unwrap())
        .bind(contract.as_slice())
        .bind(dave.as_slice())
        .bind(dave_max.log.transaction_hash.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(exact_max, (U256::MAX.to_string(), U256::MAX.to_string(), 1));

        // Rebuild through 103 restores Alice's pre-reorg YES stake and removes
        // the orphan-only Dave row.
        let rollback_103 = database
            .rollback_to_ancestor(
                chain_id,
                contract,
                100,
                &super::Checkpoint {
                    block_number: 103,
                    block_hash: canonical_103.hash,
                },
            )
            .await
            .unwrap();
        assert_eq!(rollback_103.rebuilt_position_events, 3);
        let rebuilt_alice: (String, String) = sqlx::query_as(
            "SELECT yes_stake::text, no_stake::text FROM market_positions
             WHERE chain_id = $1 AND contract_address = $2
               AND market_id = 1 AND user_address = $3",
        )
        .bind(i64::try_from(chain_id).unwrap())
        .bind(contract.as_slice())
        .bind(alice.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rebuilt_alice, ("3".into(), "0".into()));
        let dave_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM market_positions
             WHERE chain_id = $1 AND contract_address = $2 AND user_address = $3",
        )
        .bind(i64::try_from(chain_id).unwrap())
        .bind(contract.as_slice())
        .bind(dave.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(dave_count, 0);

        // The interview example diverges after MarketCreated at block 100.
        database
            .rollback_to_ancestor(
                chain_id,
                contract,
                100,
                &super::Checkpoint {
                    block_number: 100,
                    block_hash: canonical_100.hash,
                },
            )
            .await
            .unwrap();
        let replacement_101 = block(101, 0xc1, 0xa0);
        let alice_yes_4 = position_event(
            &replacement_101,
            contract,
            U256::from(1),
            alice,
            BinaryOutcome::Yes,
            U256::from(4),
            U256::from(4),
            U256::from(4),
            U256::ZERO,
            0x41,
            0,
            0,
        );
        database
            .commit_block(
                chain_id,
                contract,
                &replacement_101,
                std::slice::from_ref(&alice_yes_4),
            )
            .await
            .unwrap();
        let replacement_102 = block(102, 0xc2, 0xc1);
        let carol_no_5 = position_event(
            &replacement_102,
            contract,
            U256::from(1),
            carol,
            BinaryOutcome::No,
            U256::from(5),
            U256::from(5),
            U256::from(4),
            U256::from(5),
            0x42,
            0,
            0,
        );
        database
            .commit_block(
                chain_id,
                contract,
                &replacement_102,
                std::slice::from_ref(&carol_no_5),
            )
            .await
            .unwrap();

        let replacement_state: (String, String) = sqlx::query_as(
            "SELECT yes_pool::text, no_pool::text FROM market_states
             WHERE chain_id = $1 AND contract_address = $2 AND market_id = 1",
        )
        .bind(i64::try_from(chain_id).unwrap())
        .bind(contract.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(replacement_state, ("4".into(), "5".into()));
        let replacement_positions: Vec<(Vec<u8>, String, String)> = sqlx::query_as(
            "SELECT user_address, yes_stake::text, no_stake::text
             FROM market_positions
             WHERE chain_id = $1 AND contract_address = $2 AND market_id = 1
             ORDER BY user_address",
        )
        .bind(i64::try_from(chain_id).unwrap())
        .bind(contract.as_slice())
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            replacement_positions,
            vec![
                (alice.as_slice().to_vec(), "4".into(), "0".into()),
                (carol.as_slice().to_vec(), "0".into(), "5".into()),
            ]
        );
        assert!(
            !replacement_positions
                .iter()
                .any(|row| row.0 == bob.as_slice())
        );

        // A failure after destructive SQL has run but before COMMIT preserves the
        // replacement projection and checkpoint exactly.
        let before_failure: (String, String, i64, i64) = sqlx::query_as(
            "SELECT s.yes_pool::text, s.no_pool::text,
                    (SELECT count(*) FROM market_positions
                     WHERE chain_id = $1 AND contract_address = $2),
                    (SELECT last_block_number FROM indexer_checkpoints
                     WHERE chain_id = $1 AND contract_address = $2)
             FROM market_states s
             WHERE s.chain_id = $1 AND s.contract_address = $2 AND s.market_id = 1",
        )
        .bind(i64::try_from(chain_id).unwrap())
        .bind(contract.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
        let mut failed_transaction = pool.begin().await.unwrap();
        database
            .rollback_chain_transaction(
                &mut failed_transaction,
                chain_id,
                contract,
                100,
                &super::Checkpoint {
                    block_number: 100,
                    block_hash: canonical_100.hash,
                },
            )
            .await
            .unwrap();
        assert!(
            sqlx::query("SELECT 1 / 0")
                .execute(&mut *failed_transaction)
                .await
                .is_err()
        );
        failed_transaction.rollback().await.unwrap();
        let after_failure: (String, String, i64, i64) = sqlx::query_as(
            "SELECT s.yes_pool::text, s.no_pool::text,
                    (SELECT count(*) FROM market_positions
                     WHERE chain_id = $1 AND contract_address = $2),
                    (SELECT last_block_number FROM indexer_checkpoints
                     WHERE chain_id = $1 AND contract_address = $2)
             FROM market_states s
             WHERE s.chain_id = $1 AND s.contract_address = $2 AND s.market_id = 1",
        )
        .bind(i64::try_from(chain_id).unwrap())
        .bind(contract.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(after_failure, before_failure);

        let other_state: (String, String, String, String, i64) = sqlx::query_as(
            "SELECT s.yes_pool::text, s.no_pool::text,
                    p.yes_stake::text, p.no_stake::text,
                    c.last_block_number
             FROM market_states s
             JOIN market_positions p USING (chain_id, contract_address, market_id)
             JOIN indexer_checkpoints c USING (chain_id, contract_address)
             WHERE s.chain_id = $1 AND s.contract_address = $2 AND s.market_id = 7",
        )
        .bind(i64::try_from(other_chain_id).unwrap())
        .bind(other_contract.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            other_state,
            ("8".into(), "0".into(), "8".into(), "0".into(), 201)
        );
    }
}
