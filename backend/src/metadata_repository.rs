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
            "SELECT chain_id, contract_address, market_id, question, description, resolution_criteria, category, source_url, metadata_digest
             FROM market_metadata
             WHERE chain_id = $1 AND contract_address = $2 AND market_id = $3::numeric",
        )
        .bind(chain_id)
        .bind(contract_address.as_slice())
        .bind(market_id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => {
                let digest_bytes: Vec<u8> = row.get("metadata_digest");
                let digest =
                    B256::from_slice(&digest_bytes[..]).clone();

                let metadata = MarketMetadata {
                    question: row.get("question"),
                    description: row.get("description"),
                    resolution_criteria: row.get("resolution_criteria"),
                    category: row.get("category"),
                    source_url: row.get("source_url"),
                };

                Ok(Some(StoredMarketMetadata {
                    chain_id: row.get("chain_id"),
                    contract_address: row.get("contract_address"),
                    market_id: row.get::<String, _>("market_id"),
                    metadata,
                    metadata_digest: digest,
                }))
            }
            None => Ok(None),
        }
    }

    /// Retrieve metadata for all markets in a chain/contract pair.
    pub async fn get_all_metadata(
        &self,
        chain_id: i64,
        contract_address: Address,
    ) -> Result<Vec<StoredMarketMetadata>, MetadataRepositoryError> {
        let rows = sqlx::query(
            "SELECT chain_id, contract_address, market_id, question, description, resolution_criteria, category, source_url, metadata_digest
             FROM market_metadata
             WHERE chain_id = $1 AND contract_address = $2
             ORDER BY market_id DESC",
        )
        .bind(chain_id)
        .bind(contract_address.as_slice())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                let digest_bytes: Vec<u8> = row.get("metadata_digest");
                let digest = B256::from_slice(&digest_bytes[..]).clone();

                let metadata = MarketMetadata {
                    question: row.get("question"),
                    description: row.get("description"),
                    resolution_criteria: row.get("resolution_criteria"),
                    category: row.get("category"),
                    source_url: row.get("source_url"),
                };

                Ok(StoredMarketMetadata {
                    chain_id: row.get("chain_id"),
                    contract_address: row.get("contract_address"),
                    market_id: row.get::<String, _>("market_id"),
                    metadata,
                    metadata_digest: digest,
                })
            })
            .collect()
    }
}
