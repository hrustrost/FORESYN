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
        let contract = Address::repeat_byte(0x44);
        MarketCreatedRecord {
            log: ChainLog {
                block_number: block.number,
                block_hash: block.hash,
                transaction_hash: B256::repeat_byte(0x55),
                transaction_index: 3,
                log_index: 7,
                address: contract,
                topics: vec![market_created_topic(), B256::ZERO, B256::ZERO],
                data: vec![0; 96],
            },
            projection: MarketCreatedProjection {
                market_id: U256::MAX,
                resolver: Address::repeat_byte(0x66),
                creator: Address::repeat_byte(0x77),
                deadline: u64::MAX,
                metadata_digest: B256::repeat_byte(0x88),
            },
        }
    }

    #[tokio::test]
    async fn postgres_commit_is_atomic_idempotent_and_restartable() {
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
    }
}
