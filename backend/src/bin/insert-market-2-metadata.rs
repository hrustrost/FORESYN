//! Records the off-chain metadata for Base Sepolia Market #2 in PostgreSQL.
//!
//! This binary touches the database only. It opens no RPC connection, holds no
//! key, and broadcasts nothing; the market must already exist on-chain and be
//! indexed before it runs.
//!
//! Usage:
//!   DATABASE_URL=postgres://... cargo run --bin insert-market-2-metadata

use std::process::ExitCode;

use alloy::primitives::{Address, B256};
use foresyn_backend::{
    metadata::MarketMetadata,
    metadata_repository::MetadataRepository,
    read_repository::{MarketReader, PostgresMarketReader},
};
use sqlx::postgres::PgPoolOptions;

/// Base Sepolia.
const CHAIN_ID: u64 = 84_532;

/// The already-deployed FORESYN market contract.
const CONTRACT_ADDRESS: &str = "0xa60d5Da44F32FeC40f26945a323fa4d98790312a";

/// The market this metadata describes, as assigned on-chain.
const MARKET_ID: &str = "2";

/// The digest committed on-chain when Market #2 was created. The metadata below
/// must hash to exactly this, or the row would be stored unverifiable.
const EXPECTED_DIGEST: &str = "0xca3422b0b137aacb93c63a5cd26a8edccaaa30785067afb912c72ddda02a1659";

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let database_url =
        std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is not set".to_string())?;

    let contract: Address = CONTRACT_ADDRESS
        .parse()
        .map_err(|e| format!("contract address is not a valid address: {e}"))?;
    let expected_digest: B256 = EXPECTED_DIGEST
        .parse()
        .map_err(|e| format!("expected digest is not a valid 32-byte value: {e}"))?;

    let metadata = market_2_metadata();

    // Recompute rather than trust the constant. The digest is the only thing
    // binding this text to the chain, so it is checked before anything is
    // written, and again against the indexed on-chain value below.
    let digest = metadata
        .compute_digest()
        .map_err(|e| format!("could not compute metadata digest: {e}"))?;
    if digest != expected_digest {
        return Err(format!(
            "metadata does not hash to the expected digest.\n  \
             expected: {expected_digest:?}\n  \
             computed: {digest:?}\n  \
             The metadata text has drifted; refusing to insert."
        ));
    }
    println!("Digest matches the expected value: {digest:?}");

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .map_err(|e| format!("could not connect to the database: {e}"))?;

    let chain_id = i64::try_from(CHAIN_ID).map_err(|_| "chain id is out of range".to_string())?;
    let repository = MetadataRepository::new(pool.clone());
    let reader = PostgresMarketReader::from_pool(pool, CHAIN_ID, contract)
        .map_err(|e| format!("could not build the market reader: {e}"))?;

    // The metadata table has a foreign key onto markets, so the market must be
    // indexed first. Checking here turns an opaque constraint violation into a
    // clear statement about what is missing.
    let market = reader
        .market(MARKET_ID)
        .await
        .map_err(|e| format!("could not read market {MARKET_ID}: {e}"))?
        .ok_or_else(|| {
            format!(
                "market {MARKET_ID} is not indexed for chain {CHAIN_ID} at {contract}.\n  \
                 Create it on-chain and let the indexer catch up before inserting metadata."
            )
        })?;

    // Guard against writing metadata that could never verify: compare against
    // the digest the indexer actually read from the chain, not just the
    // constant above.
    let on_chain_digest = market.metadata_digest.to_lowercase();
    if on_chain_digest != format!("{digest:?}") {
        return Err(format!(
            "the indexed on-chain digest for market {MARKET_ID} does not match this metadata.\n  \
             on-chain: {on_chain_digest}\n  \
             metadata: {digest:?}\n  \
             This metadata belongs to a different market; refusing to insert."
        ));
    }

    match repository
        .get_metadata(chain_id, contract, MARKET_ID)
        .await
        .map_err(|e| format!("could not read existing metadata: {e}"))?
    {
        // Already recorded and identical: nothing to do, so re-running this
        // command is safe.
        Some(existing) if existing.metadata == metadata && existing.metadata_digest == digest => {
            println!("Metadata is already present and identical; nothing to insert.");
        }
        // Different metadata is already recorded. Overwriting it silently could
        // destroy a correct row, so this stops instead.
        Some(existing) => {
            return Err(format!(
                "market {MARKET_ID} already has different metadata stored.\n  \
                 stored question: {}\n  \
                 stored digest:   {:?}\n  \
                 new question:    {}\n  \
                 new digest:      {digest:?}\n  \
                 Refusing to overwrite. Remove the existing row deliberately if it is wrong.",
                existing.metadata.question, existing.metadata_digest, metadata.question,
            ));
        }
        None => {
            repository
                .insert_metadata(chain_id, contract, MARKET_ID, &metadata, digest)
                .await
                .map_err(|e| format!("could not insert metadata: {e}"))?;
            println!("Inserted metadata for market {MARKET_ID}.");
        }
    }

    report(&repository, &reader, chain_id, contract).await
}

/// Reads the row back through the same path the API uses, so the printed
/// verification result is the one clients will actually see.
async fn report(
    repository: &MetadataRepository,
    reader: &PostgresMarketReader,
    chain_id: i64,
    contract: Address,
) -> Result<(), String> {
    let stored = repository
        .get_metadata(chain_id, contract, MARKET_ID)
        .await
        .map_err(|e| format!("could not read metadata back: {e}"))?
        .ok_or_else(|| format!("metadata for market {MARKET_ID} vanished after insert"))?;

    let market = reader
        .market(MARKET_ID)
        .await
        .map_err(|e| format!("could not read market {MARKET_ID} back: {e}"))?
        .ok_or_else(|| format!("market {MARKET_ID} vanished after insert"))?;

    let verified = market.metadata_verified.unwrap_or(false);

    println!();
    println!("Stored row");
    println!("----------");
    println!("  chain_id          {}", stored.chain_id);
    println!("  contract_address  0x{}", hex(&stored.contract_address));
    println!("  market_id         {}", stored.market_id);
    println!("  question          {}", stored.metadata.question);
    println!("  metadata_digest   {:?}", stored.metadata_digest);
    println!(
        "  verified          {verified}{}",
        if verified {
            ""
        } else {
            "  <- the API will not present this metadata as authentic"
        }
    );

    if !verified {
        return Err(
            "the stored metadata does not verify against the indexed on-chain digest".to_string(),
        );
    }

    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The exact metadata committed on-chain for Market #2.
fn market_2_metadata() -> MarketMetadata {
    MarketMetadata {
        question: "Will ETH be above $4,000 on August 22, 2026?".to_string(),
        description: "A Base Sepolia demonstration prediction market for FORESYN.".to_string(),
        resolution_criteria: "Resolves YES if the ETH/USD reference price is strictly above 4000 USD at the market deadline. Otherwise resolves NO.".to_string(),
        category: "Crypto".to_string(),
        source_url: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The binary must never insert metadata that disagrees with the digest it
    /// claims to be committing.
    #[test]
    fn metadata_hashes_to_the_expected_digest() {
        let digest = market_2_metadata().compute_digest().unwrap();
        assert_eq!(format!("{digest:?}"), EXPECTED_DIGEST);
    }

    #[test]
    fn constants_parse() {
        assert!(CONTRACT_ADDRESS.parse::<Address>().is_ok());
        assert!(EXPECTED_DIGEST.parse::<B256>().is_ok());
    }
}
