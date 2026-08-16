use alloy::primitives::Address;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use thiserror::Error;

use crate::metadata::MarketMetadata;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketReadModel {
    pub market_id: String,
    pub resolver: String,
    pub creator: String,
    pub deadline: String,
    pub metadata_digest: String,
    pub creation_block_number: String,
    pub yes_pool: String,
    pub no_pool: String,
    pub total_pool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MarketMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_verified: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PositionReadModel {
    pub user_address: String,
    pub yes_stake: String,
    pub no_stake: String,
    pub total_stake: String,
    pub updated_block_number: String,
}

#[derive(Debug, Error)]
pub enum ReadError {
    #[error("PostgreSQL read failed: {0}")]
    Sql(#[from] sqlx::Error),
    #[error("stored {field} is not a valid {expected}-byte EVM value")]
    InvalidBinaryValue {
        field: &'static str,
        expected: usize,
    },
    #[error("stored {field} is outside its non-negative PostgreSQL range")]
    InvalidBlockNumber { field: &'static str },
}

#[async_trait]
pub trait MarketReader: Send + Sync {
    async fn list_markets(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<MarketReadModel>, ReadError>;

    async fn market(&self, market_id: &str) -> Result<Option<MarketReadModel>, ReadError>;

    async fn positions(&self, market_id: &str) -> Result<Vec<PositionReadModel>, ReadError>;
}

#[derive(Clone)]
pub struct PostgresMarketReader {
    pool: PgPool,
    chain_id: i64,
    contract_address: Address,
}

impl PostgresMarketReader {
    pub async fn connect(
        database_url: &str,
        chain_id: u64,
        contract_address: Address,
    ) -> Result<Self, ReadError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        Self::from_pool(pool, chain_id, contract_address)
    }

    pub fn from_pool(
        pool: PgPool,
        chain_id: u64,
        contract_address: Address,
    ) -> Result<Self, ReadError> {
        let chain_id = i64::try_from(chain_id)
            .map_err(|_| ReadError::InvalidBlockNumber { field: "chain_id" })?;
        Ok(Self {
            pool,
            chain_id,
            contract_address,
        })
    }
}

#[async_trait]
impl MarketReader for PostgresMarketReader {
    async fn list_markets(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<MarketReadModel>, ReadError> {
        let rows = sqlx::query(
            "SELECT
                m.market_id::text AS market_id,
                m.resolver,
                m.creator,
                m.deadline::text AS deadline,
                m.metadata_digest,
                m.creation_block_number,
                COALESCE(s.yes_pool, 0)::text AS yes_pool,
                COALESCE(s.no_pool, 0)::text AS no_pool,
                (COALESCE(s.yes_pool, 0) + COALESCE(s.no_pool, 0))::text AS total_pool,
                mm.question,
                mm.description,
                mm.resolution_criteria,
                mm.category,
                mm.source_url
             FROM markets m
             LEFT JOIN market_states s
               ON s.chain_id = m.chain_id
              AND s.contract_address = m.contract_address
              AND s.market_id = m.market_id
             LEFT JOIN market_metadata mm
               ON mm.chain_id = m.chain_id
              AND mm.contract_address = m.contract_address
              AND mm.market_id = m.market_id
             WHERE m.chain_id = $1 AND m.contract_address = $2
             ORDER BY m.market_id DESC
             LIMIT $3 OFFSET $4",
        )
        .bind(self.chain_id)
        .bind(self.contract_address.as_slice())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(market_from_row).collect()
    }

    async fn market(&self, market_id: &str) -> Result<Option<MarketReadModel>, ReadError> {
        let row = sqlx::query(
            "SELECT
                m.market_id::text AS market_id,
                m.resolver,
                m.creator,
                m.deadline::text AS deadline,
                m.metadata_digest,
                m.creation_block_number,
                COALESCE(s.yes_pool, 0)::text AS yes_pool,
                COALESCE(s.no_pool, 0)::text AS no_pool,
                (COALESCE(s.yes_pool, 0) + COALESCE(s.no_pool, 0))::text AS total_pool,
                mm.question,
                mm.description,
                mm.resolution_criteria,
                mm.category,
                mm.source_url
             FROM markets m
             LEFT JOIN market_states s
               ON s.chain_id = m.chain_id
              AND s.contract_address = m.contract_address
              AND s.market_id = m.market_id
             LEFT JOIN market_metadata mm
               ON mm.chain_id = m.chain_id
              AND mm.contract_address = m.contract_address
              AND mm.market_id = m.market_id
             WHERE m.chain_id = $1
               AND m.contract_address = $2
               AND m.market_id = $3::numeric",
        )
        .bind(self.chain_id)
        .bind(self.contract_address.as_slice())
        .bind(market_id)
        .fetch_optional(&self.pool)
        .await?;

        row.as_ref().map(market_from_row).transpose()
    }

    async fn positions(&self, market_id: &str) -> Result<Vec<PositionReadModel>, ReadError> {
        let rows = sqlx::query(
            "SELECT
                user_address,
                yes_stake::text AS yes_stake,
                no_stake::text AS no_stake,
                (yes_stake + no_stake)::text AS total_stake,
                updated_block_number
             FROM market_positions
             WHERE chain_id = $1
               AND contract_address = $2
               AND market_id = $3::numeric
             ORDER BY user_address ASC",
        )
        .bind(self.chain_id)
        .bind(self.contract_address.as_slice())
        .bind(market_id)
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(position_from_row).collect()
    }
}

fn market_from_row(row: &sqlx::postgres::PgRow) -> Result<MarketReadModel, ReadError> {
    let on_chain_digest = row.try_get::<Vec<u8>, _>("metadata_digest")?;
    let metadata_digest_hex = prefixed_hex("metadata_digest", &on_chain_digest, 32)?;

    // The metadata columns come from a LEFT JOIN, so they are all NULL for a
    // market with no off-chain metadata (Market #1). Such a market is reported
    // with no metadata at all rather than as unverified metadata.
    let (metadata, metadata_verified) = match (
        row.try_get::<Option<String>, _>("question")?,
        row.try_get::<Option<String>, _>("description")?,
        row.try_get::<Option<String>, _>("resolution_criteria")?,
        row.try_get::<Option<String>, _>("category")?,
    ) {
        (Some(question), Some(description), Some(resolution_criteria), Some(category)) => {
            let metadata = MarketMetadata {
                question,
                description,
                resolution_criteria,
                category,
                source_url: row.try_get("source_url")?,
            };

            // Recompute the digest from the stored text and compare it to the
            // value the indexer read from the chain. Metadata that fails this
            // check is still returned, but flagged, so a mismatch is visible
            // rather than silently rendered as authentic.
            let verified = metadata
                .compute_digest()
                .is_ok_and(|computed| computed.as_slice() == on_chain_digest.as_slice());

            (Some(metadata), Some(verified))
        }
        _ => (None, None),
    };

    Ok(MarketReadModel {
        market_id: row.try_get("market_id")?,
        resolver: prefixed_hex("resolver", &row.try_get::<Vec<u8>, _>("resolver")?, 20)?,
        creator: prefixed_hex("creator", &row.try_get::<Vec<u8>, _>("creator")?, 20)?,
        deadline: row.try_get("deadline")?,
        metadata_digest: metadata_digest_hex,
        creation_block_number: non_negative_i64_string(
            "creation_block_number",
            row.try_get("creation_block_number")?,
        )?,
        yes_pool: row.try_get("yes_pool")?,
        no_pool: row.try_get("no_pool")?,
        total_pool: row.try_get("total_pool")?,
        metadata,
        metadata_verified,
    })
}

fn position_from_row(row: &sqlx::postgres::PgRow) -> Result<PositionReadModel, ReadError> {
    Ok(PositionReadModel {
        user_address: prefixed_hex(
            "user_address",
            &row.try_get::<Vec<u8>, _>("user_address")?,
            20,
        )?,
        yes_stake: row.try_get("yes_stake")?,
        no_stake: row.try_get("no_stake")?,
        total_stake: row.try_get("total_stake")?,
        updated_block_number: non_negative_i64_string(
            "updated_block_number",
            row.try_get("updated_block_number")?,
        )?,
    })
}

fn non_negative_i64_string(field: &'static str, value: i64) -> Result<String, ReadError> {
    if value < 0 {
        Err(ReadError::InvalidBlockNumber { field })
    } else {
        Ok(value.to_string())
    }
}

fn prefixed_hex(
    field: &'static str,
    bytes: &[u8],
    expected_length: usize,
) -> Result<String, ReadError> {
    if bytes.len() != expected_length {
        return Err(ReadError::InvalidBinaryValue {
            field,
            expected: expected_length,
        });
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(2 + bytes.len() * 2);
    encoded.push_str("0x");
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::{ReadError, prefixed_hex};

    #[test]
    fn binary_values_use_lowercase_prefixed_fixed_width_hex() {
        assert_eq!(
            prefixed_hex("address", &[0xab; 20], 20).unwrap(),
            "0xabababababababababababababababababababab"
        );
        assert!(matches!(
            prefixed_hex("digest", &[0; 31], 32),
            Err(ReadError::InvalidBinaryValue {
                field: "digest",
                expected: 32
            })
        ));
    }
}
