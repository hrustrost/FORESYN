use alloy::primitives::{Address, B256};
use sqlx::{PgPool, Row};
use thiserror::Error;

use crate::metadata::MarketMetadata;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMarketMetadata {
    pub chain_id: i64,
    pub contract_address: Vec<u8>,
    pub market_id: String,
    pub metadata: MarketMetadata,
    pub metadata_digest: B256,
}

#[derive(Debug, Error)]
pub enum MetadataRepositoryError {
    #[error("database operation failed: {0}")]
    Sql(#[from] sqlx::Error),
    #[error("invalid binary value: {0}")]
    InvalidBinaryValue(String),
    #[error("metadata not found")]
    NotFound,
}

pub struct MetadataRepository {
    pool: PgPool,
}

impl MetadataRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Insert metadata for a market.
    pub async fn insert_metadata(
        &self,
        chain_id: i64,
        contract_address: Address,
        market_id: &str,
        metadata: &MarketMetadata,
        metadata_digest: B256,
    ) -> Result<(), MetadataRepositoryError> {
        let digest_bytes = metadata_digest.as_slice();
        sqlx::query(
            "INSERT INTO market_metadata 
             (chain_id, contract_address, market_id, question, description, resolution_criteria, category, source_url, metadata_digest)
             VALUES ($1, $2, $3::numeric, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (chain_id, contract_address, market_id) DO UPDATE
             SET question = $4, description = $5, resolution_criteria = $6, category = $7, source_url = $8, metadata_digest = $9, updated_at = now()",
        )
        .bind(chain_id)
        .bind(contract_address.as_slice())
        .bind(market_id)
        .bind(&metadata.question)
        .bind(&metadata.description)
        .bind(&metadata.resolution_criteria)
        .bind(&metadata.category)
        .bind(&metadata.source_url)
        .bind(digest_bytes)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Retrieve metadata for a specific market.
    pub async fn get_metadata(
        &self,
        chain_id: i64,
        contract_address: Address,
        market_id: &str,
    ) -> Result<Option<StoredMarketMetadata>, MetadataRepositoryError> {
        let row = sqlx::query(
            "SELECT chain_id, contract_address, market_id::text AS market_id,
                    question, description, resolution_criteria, category, source_url,
                    metadata_digest
             FROM market_metadata
             WHERE chain_id = $1 AND contract_address = $2 AND market_id = $3::numeric",
        )
        .bind(chain_id)
        .bind(contract_address.as_slice())
        .bind(market_id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(stored_metadata_from_row).transpose()
    }

    /// Retrieve metadata for all markets in a chain/contract pair.
    pub async fn get_all_metadata(
        &self,
        chain_id: i64,
        contract_address: Address,
    ) -> Result<Vec<StoredMarketMetadata>, MetadataRepositoryError> {
        let rows = sqlx::query(
            "SELECT chain_id, contract_address, market_id::text AS market_id,
                    question, description, resolution_criteria, category, source_url,
                    metadata_digest
             FROM market_metadata
             WHERE chain_id = $1 AND contract_address = $2
             ORDER BY market_id DESC",
        )
        .bind(chain_id)
        .bind(contract_address.as_slice())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(stored_metadata_from_row).collect()
    }
}

/// `market_id` is `NUMERIC(78, 0)` and is cast to text by every query above,
/// because a 78-digit value has no lossless native integer type to decode into.
fn stored_metadata_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<StoredMarketMetadata, MetadataRepositoryError> {
    let digest_bytes: Vec<u8> = row.try_get("metadata_digest")?;
    let metadata_digest = B256::try_from(digest_bytes.as_slice()).map_err(|_| {
        MetadataRepositoryError::InvalidBinaryValue(format!(
            "metadata_digest is {} bytes, expected 32",
            digest_bytes.len()
        ))
    })?;

    Ok(StoredMarketMetadata {
        chain_id: row.try_get("chain_id")?,
        contract_address: row.try_get("contract_address")?,
        market_id: row.try_get("market_id")?,
        metadata: MarketMetadata {
            question: row.try_get("question")?,
            description: row.try_get("description")?,
            resolution_criteria: row.try_get("resolution_criteria")?,
            category: row.try_get("category")?,
            source_url: row.try_get("source_url")?,
        },
        metadata_digest,
    })
}

#[cfg(test)]
mod tests {
    use sqlx::postgres::PgPoolOptions;

    use super::*;
    use crate::db::{Database, POSTGRES_TEST_LOCK};

    async fn integration_pool() -> Option<PgPool> {
        let database_url = std::env::var("TEST_DATABASE_URL").ok()?;
        Some(
            PgPoolOptions::new()
                .max_connections(2)
                .connect(&database_url)
                .await
                .expect("TEST_DATABASE_URL must point at a reachable database"),
        )
    }

    /// Exercises the repository's SQL end to end. `market_id` is NUMERIC(78, 0),
    /// so this also covers the text cast and a market id beyond u64.
    #[tokio::test]
    async fn metadata_round_trips_through_postgres() {
        let Some(pool) = integration_pool().await else {
            eprintln!("skipping metadata repository test: TEST_DATABASE_URL is not set");
            return;
        };
        let _guard = POSTGRES_TEST_LOCK.lock().await;
        Database::from_pool(pool.clone()).migrate().await.unwrap();

        let chain_id = 71_340_i64;
        let contract = Address::from([0x88_u8; 20]);
        // A 78-digit market id would overflow every native integer type.
        let big_market_id = "340282366920938463463374607431768211455";

        sqlx::query("DELETE FROM indexed_blocks WHERE chain_id = $1")
            .bind(chain_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO indexed_blocks
                (chain_id, block_number, block_hash, parent_hash, block_timestamp)
             VALUES ($1, 100, $2, $3, now())",
        )
        .bind(chain_id)
        .bind(vec![0x40_u8; 32])
        .bind(vec![0x3f_u8; 32])
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO markets
                (chain_id, contract_address, market_id, resolver, creator, deadline,
                 metadata_digest, creation_block_number, creation_transaction_hash)
             VALUES ($1, $2, $3::numeric, $4, $5, 1787356800, $6, 100, $7)",
        )
        .bind(chain_id)
        .bind(contract.as_slice())
        .bind(big_market_id)
        .bind(vec![0x11_u8; 20])
        .bind(vec![0x12_u8; 20])
        .bind(vec![0x13_u8; 32])
        .bind(vec![0x14_u8; 32])
        .execute(&pool)
        .await
        .unwrap();

        let repository = MetadataRepository::new(pool.clone());
        let metadata = MarketMetadata {
            question: "Will ETH be above $4,000 on August 22, 2026?".to_string(),
            description: "A Base Sepolia demonstration prediction market for FORESYN.".to_string(),
            resolution_criteria: "Resolves YES if the ETH/USD reference price is strictly above 4000 USD at the market deadline. Otherwise resolves NO.".to_string(),
            category: "Crypto".to_string(),
            source_url: None,
        };
        let digest = metadata.compute_digest().unwrap();

        repository
            .insert_metadata(chain_id, contract, big_market_id, &metadata, digest)
            .await
            .unwrap();

        let stored = repository
            .get_metadata(chain_id, contract, big_market_id)
            .await
            .unwrap()
            .expect("metadata was just inserted");
        assert_eq!(stored.market_id, big_market_id);
        assert_eq!(stored.metadata, metadata);
        assert_eq!(stored.metadata_digest, digest);
        assert!(
            stored
                .metadata
                .verify_digest(stored.metadata_digest)
                .unwrap()
        );

        // The insert is an upsert, so re-running it must overwrite rather than
        // fail on the primary key.
        let mut revised = metadata.clone();
        revised.source_url = Some("https://example.com/rules".to_string());
        let revised_digest = revised.compute_digest().unwrap();
        assert_ne!(revised_digest, digest, "source_url must affect the digest");

        repository
            .insert_metadata(chain_id, contract, big_market_id, &revised, revised_digest)
            .await
            .unwrap();

        let stored = repository
            .get_metadata(chain_id, contract, big_market_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.metadata, revised);
        assert_eq!(stored.metadata_digest, revised_digest);

        let all = repository
            .get_all_metadata(chain_id, contract)
            .await
            .unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].market_id, big_market_id);

        // Scoping: a different contract on the same chain sees nothing.
        let other = repository
            .get_all_metadata(chain_id, Address::from([0x99_u8; 20]))
            .await
            .unwrap();
        assert!(other.is_empty());

        assert!(
            repository
                .get_metadata(chain_id, contract, "999999")
                .await
                .unwrap()
                .is_none(),
            "an unknown market id must be None, not an error"
        );

        sqlx::query("DELETE FROM indexed_blocks WHERE chain_id = $1")
            .bind(chain_id)
            .execute(&pool)
            .await
            .unwrap();
    }
}
