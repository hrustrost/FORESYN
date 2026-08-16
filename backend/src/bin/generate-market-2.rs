//! Prints everything needed to create Base Sepolia Market #2, without ever
//! touching the network. This binary only computes and prints: it holds no key,
//! opens no RPC connection, and broadcasts nothing.
//!
//! Usage:
//!   cargo run --bin generate-market-2 -- <resolver-address>

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::Address;
use foresyn_backend::metadata::MarketMetadata;

/// Base Sepolia.
const CHAIN_ID: u64 = 84532;

/// The already-deployed FORESYN market contract.
const CONTRACT_ADDRESS: &str = "0xa60d5Da44F32FeC40f26945a323fa4d98790312a";

/// 2026-08-22T00:00:00Z. The contract rejects a deadline at or before
/// `block.timestamp`, so this must stay in the future at broadcast time.
const DEADLINE: u64 = 1_787_356_800;

fn main() -> ExitCode {
    let resolver = match std::env::args().nth(1) {
        Some(arg) => match arg.parse::<Address>() {
            Ok(address) if !address.is_zero() => address,
            Ok(_) => {
                eprintln!(
                    "error: resolver must not be the zero address; \
                     createMarket reverts with InvalidResolver"
                );
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("error: could not parse resolver address: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            eprintln!("usage: cargo run --bin generate-market-2 -- <resolver-address>");
            eprintln!();
            eprintln!(
                "The resolver is the account allowed to settle this market. \
                 The contract rejects the zero address, so there is no safe default."
            );
            return ExitCode::FAILURE;
        }
    };

    let contract: Address = CONTRACT_ADDRESS.parse().expect("contract address is valid");

    let metadata = MarketMetadata {
        question: "Will ETH be above $4,000 on August 22, 2026?".to_string(),
        description: "A Base Sepolia demonstration prediction market for FORESYN.".to_string(),
        resolution_criteria: "Resolves YES if the ETH/USD reference price is strictly above 4000 USD at the market deadline. Otherwise resolves NO.".to_string(),
        category: "Crypto".to_string(),
        source_url: None,
    };

    // Go through the same code path the API uses to verify stored metadata, so
    // the printed digest cannot drift from what the backend will later check.
    let canonical = metadata
        .to_canonical_json()
        .expect("canonical serialization is infallible for these values");
    let digest = metadata
        .compute_digest()
        .expect("digest computation is infallible for these values");

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_secs();

    println!("FORESYN Market #2 — create-market parameters (nothing is broadcast)");
    println!();

    println!("Canonical metadata JSON");
    println!("-----------------------");
    println!("{}", String::from_utf8_lossy(&canonical));
    println!();

    println!("Metadata digest (keccak256 of the bytes above)");
    println!("----------------------------------------------");
    println!("{digest:?}");
    println!();

    println!("Parameters");
    println!("----------");
    println!("  chain id        {CHAIN_ID} (Base Sepolia)");
    println!("  contract        {contract}");
    println!("  deadline        {DEADLINE}  (2026-08-22T00:00:00Z)");
    println!("  resolver        {resolver}");
    println!("  market id       assigned on-chain by createMarket; not chosen by us");
    println!();

    if DEADLINE <= now {
        println!("  WARNING: the deadline is now in the past.");
        println!("  createMarket would revert with DeadlineNotInFuture. Pick a later deadline.");
        println!();
    }

    println!("Parameter meanings");
    println!("------------------");
    println!("  deadline  uint64 Unix seconds. Betting closes at this time and the");
    println!("            market cannot be resolved before it. Must be > block.timestamp.");
    println!("  resolver  The only account that may settle the outcome. This is a");
    println!("            trusted role: there is no oracle, and nothing about ETH/USD");
    println!("            is checked on-chain. A human submits the result.");
    println!("  digest    bytes32 commitment to the canonical metadata above. The");
    println!("            contract stores it verbatim and never interprets it.");
    println!();

    println!("Command (requires the contract owner's key; createMarket is onlyOwner)");
    println!("---------------------------------------------------------------------");
    println!("  cast send {contract} \\");
    println!("    'createMarket(uint64,address,bytes32)' \\");
    println!("    {DEADLINE} \\");
    println!("    {resolver} \\");
    println!("    {digest:?} \\");
    println!("    --rpc-url \"$BASE_SEPOLIA_RPC_URL\" \\");
    println!("    --private-key \"$DEPLOYER_PRIVATE_KEY\"");
    println!();
    println!("  Dry-run first (simulates, sends nothing):");
    println!("  cast call {contract} \\");
    println!("    'createMarket(uint64,address,bytes32)' \\");
    println!("    {DEADLINE} {resolver} {digest:?} \\");
    println!("    --rpc-url \"$BASE_SEPOLIA_RPC_URL\" --from <owner-address>");
    println!();

    println!("Then record the metadata off-chain, substituting the market id the");
    println!("MarketCreated event actually assigned:");
    println!("-------------------------------------------------------------------");
    println!("  INSERT INTO market_metadata (");
    println!("      chain_id, contract_address, market_id,");
    println!("      question, description, resolution_criteria, category, source_url,");
    println!("      metadata_digest");
    println!("  ) VALUES (");
    println!("      {CHAIN_ID},");
    println!("      '\\x{:x}',", contract);
    println!("      <market-id-from-event>,");
    println!("      {},", sql_quote(&metadata.question));
    println!("      {},", sql_quote(&metadata.description));
    println!("      {},", sql_quote(&metadata.resolution_criteria));
    println!("      {},", sql_quote(&metadata.category));
    println!("      NULL,");
    println!("      '\\x{:x}'", digest);
    println!("  );");
    println!();

    println!("Note: the API recomputes this digest from the stored row and compares");
    println!("it to the indexed on-chain value. If the text above is edited even by");
    println!("one character, the market renders as unverified rather than verified.");

    ExitCode::SUCCESS
}

/// Renders a Postgres single-quoted string literal.
fn sql_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
