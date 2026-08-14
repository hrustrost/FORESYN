use alloy::primitives::{Address, B256};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgPoolOptions};
use thiserror::Error;

use crate::{
    chain::{ChainBlock, ChainLog},
    contracts::MarketCreatedProjection,
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

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
pub struct MarketCreatedRecord {
    pub log: ChainLog,
    pub projection: MarketCreatedProjection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RollbackSummary {
    pub ancestor: Checkpoint,
    pub orphaned_blocks: u64,
    pub orphaned_events: u64,
    pub orphaned_market_projections: u64,
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
        ancestor: &Checkpoint,
    ) -> Result<RollbackSummary, DbError> {
        let mut transaction = self.pool.begin().await?;
        let summary = self
            .rollback_chain_transaction(&mut transaction, chain_id, contract_address, ancestor)
            .await?;
        transaction.commit().await?;
        Ok(summary)
    }

    pub(crate) async fn rollback_chain_transaction(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        chain_id: u64,
        contract_address: Address,
        ancestor: &Checkpoint,
    ) -> Result<RollbackSummary, DbError> {
        let chain_id = db_i64("chain_id", chain_id)?;
        let ancestor_number = db_i64("block_number", ancestor.block_number)?;

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
        })
    }

    pub async fn commit_block(
        &self,
        chain_id: u64,
        contract_address: Address,
        block: &ChainBlock,
        events: &[MarketCreatedRecord],
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
        events: &[MarketCreatedRecord],
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
                insert_market_projection(transaction, chain_id, contract_address, block, event)
                    .await?;
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
    event: &MarketCreatedRecord,
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
    event: &MarketCreatedRecord,
) -> Result<(), DbError> {
    let market_id = event.projection.market_id.to_string();
    let deadline = event.projection.deadline.to_string();
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
    .bind(event.projection.resolver.as_slice())
    .bind(event.projection.creator.as_slice())
    .bind(&deadline)
    .bind(event.projection.metadata_digest.as_slice())
    .bind(db_i64("creation_block_number", block.number)?)
    .bind(event.log.transaction_hash.as_slice())
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
        == event.projection.resolver.as_slice()
        && row.try_get::<Vec<u8>, _>("creator")?.as_slice() == event.projection.creator.as_slice()
        && row.try_get::<String, _>("deadline")? == deadline
        && row.try_get::<Vec<u8>, _>("metadata_digest")?.as_slice()
            == event.projection.metadata_digest.as_slice()
        && row.try_get::<i64, _>("creation_block_number")?
            == db_i64("creation_block_number", block.number)?
        && row
            .try_get::<Vec<u8>, _>("creation_transaction_hash")?
            .as_slice()
            == event.log.transaction_hash.as_slice();

    if identical {
        Ok(())
    } else {
        Err(DbError::MarketIdentityConflict)
    }
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
    use alloy::primitives::{Address, B256, U256};
    use sqlx::{PgPool, Row, postgres::PgPoolOptions};

    use super::{Database, MarketCreatedRecord};
    use crate::{
        chain::{ChainBlock, ChainLog},
        contracts::{MarketCreatedProjection, market_created_topic},
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

    fn event(block: &ChainBlock) -> MarketCreatedRecord {
        event_with(block, Address::repeat_byte(0x44), U256::MAX, 0x55, 0x88)
    }

    fn event_with(
        block: &ChainBlock,
        contract: Address,
        market_id: U256,
        transaction_byte: u8,
        metadata_byte: u8,
    ) -> MarketCreatedRecord {
        MarketCreatedRecord {
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
            projection: MarketCreatedProjection {
                market_id,
                resolver: Address::repeat_byte(0x66),
                creator: Address::repeat_byte(0x77),
                deadline: u64::MAX,
                metadata_digest: B256::repeat_byte(metadata_byte),
            },
        }
    }

    #[tokio::test]
    async fn postgres_commit_and_reorg_rollback_are_atomic_idempotent_and_restartable() {
        let Some(pool) = integration_pool().await else {
            eprintln!("skipping PostgreSQL integration test: TEST_DATABASE_URL is not set");
            return;
        };
        let database = Database::from_pool(pool.clone());
        database.migrate().await.unwrap();
        sqlx::query(
            "TRUNCATE markets, indexer_checkpoints, blockchain_events, indexed_blocks CASCADE",
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
        assert_eq!(
            projection.try_get::<String, _>("market_id").unwrap(),
            U256::MAX.to_string()
        );
        assert_eq!(
            projection.try_get::<Vec<u8>, _>("resolver").unwrap(),
            record.projection.resolver.as_slice()
        );
        assert_eq!(
            projection.try_get::<Vec<u8>, _>("creator").unwrap(),
            record.projection.creator.as_slice()
        );
        assert_eq!(
            projection.try_get::<String, _>("deadline").unwrap(),
            u64::MAX.to_string()
        );
        assert_eq!(
            projection.try_get::<Vec<u8>, _>("metadata_digest").unwrap(),
            record.projection.metadata_digest.as_slice()
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
            "TRUNCATE markets, indexer_checkpoints, blockchain_events, indexed_blocks CASCADE",
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
}
