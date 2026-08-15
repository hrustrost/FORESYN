use std::{collections::HashMap, env, fmt, net::SocketAddr};

use alloy::primitives::Address;
use thiserror::Error;
use url::Url;

const MAX_BATCH_SIZE: u64 = 10_000;
const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8080";
const DEFAULT_CORS_ORIGIN: &str = "http://localhost:5173";

#[derive(Clone, PartialEq, Eq)]
pub struct ApiConfig {
    pub database_url: String,
    pub chain_id: u64,
    pub contract_address: Address,
    pub bind_address: SocketAddr,
    pub cors_origin: String,
}

impl fmt::Debug for ApiConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApiConfig")
            .field("database_url", &"<redacted>")
            .field("chain_id", &self.chain_id)
            .field("contract_address", &self.contract_address)
            .field("bind_address", &self.bind_address)
            .field("cors_origin", &self.cors_origin)
            .finish()
    }
}

impl ApiConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_values(env::vars().collect())
    }

    fn from_values(values: HashMap<String, String>) -> Result<Self, ConfigError> {
        let database_url = database_url(&values)?;
        let chain_id = chain_id(&values)?;
        let contract_address = contract_address(&values)?;
        let bind_address_raw = values
            .get("FORESYN_BIND_ADDRESS")
            .map(String::as_str)
            .unwrap_or(DEFAULT_BIND_ADDRESS);
        let bind_address =
            bind_address_raw
                .parse()
                .map_err(|_| ConfigError::InvalidSocketAddress {
                    name: "FORESYN_BIND_ADDRESS",
                })?;
        let cors_origin = values
            .get("FORESYN_CORS_ORIGIN")
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| DEFAULT_CORS_ORIGIN.to_owned());
        let cors_url = parse_url("FORESYN_CORS_ORIGIN", &cors_origin)?;
        if !matches!(cors_url.scheme(), "http" | "https") {
            return Err(ConfigError::UnsupportedScheme {
                name: "FORESYN_CORS_ORIGIN",
                scheme: cors_url.scheme().to_owned(),
            });
        }

        Ok(Self {
            database_url,
            chain_id,
            contract_address,
            bind_address,
            cors_origin,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct IndexerConfig {
    pub database_url: String,
    pub rpc_url: Url,
    pub chain_id: u64,
    pub contract_address: Address,
    pub deployment_block: u64,
    pub confirmations: u64,
    pub batch_size: u64,
}

impl fmt::Debug for IndexerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IndexerConfig")
            .field("database_url", &"<redacted>")
            .field("rpc_url", &"<redacted>")
            .field("chain_id", &self.chain_id)
            .field("contract_address", &self.contract_address)
            .field("deployment_block", &self.deployment_block)
            .field("confirmations", &self.confirmations)
            .field("batch_size", &self.batch_size)
            .finish()
    }
}

impl IndexerConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_values(env::vars().collect())
    }

    fn from_values(values: HashMap<String, String>) -> Result<Self, ConfigError> {
        let database_url = database_url(&values)?;

        let rpc_url_raw = required(&values, "EVM_RPC_URL")?;
        let rpc_url = parse_url("EVM_RPC_URL", &rpc_url_raw)?;
        if !matches!(rpc_url.scheme(), "http" | "https") {
            return Err(ConfigError::UnsupportedScheme {
                name: "EVM_RPC_URL",
                scheme: rpc_url.scheme().to_owned(),
            });
        }

        let chain_id = chain_id(&values)?;
        let contract_address = contract_address(&values)?;

        let deployment_block = parse_u64(&values, "FORESYN_DEPLOYMENT_BLOCK")?;
        ensure_db_bigint("FORESYN_DEPLOYMENT_BLOCK", deployment_block)?;

        let confirmations = parse_u64(&values, "INDEXER_CONFIRMATIONS")?;
        ensure_db_bigint("INDEXER_CONFIRMATIONS", confirmations)?;

        let batch_size = parse_u64(&values, "INDEXER_BATCH_SIZE")?;
        if !(1..=MAX_BATCH_SIZE).contains(&batch_size) {
            return Err(ConfigError::OutOfRange {
                name: "INDEXER_BATCH_SIZE",
                value: batch_size,
                range: "1..=10000",
            });
        }

        Ok(Self {
            database_url,
            rpc_url,
            chain_id,
            contract_address,
            deployment_block,
            confirmations,
            batch_size,
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    Missing(&'static str),
    #[error("environment variable {name} must be an unsigned integer")]
    InvalidInteger { name: &'static str },
    #[error("environment variable {name} must be a valid URL")]
    InvalidUrl { name: &'static str },
    #[error("environment variable {name} uses unsupported URL scheme {scheme}")]
    UnsupportedScheme { name: &'static str, scheme: String },
    #[error("environment variable {name} must be a 20-byte EVM address")]
    InvalidAddress { name: &'static str },
    #[error("environment variable {name} must be a valid socket address")]
    InvalidSocketAddress { name: &'static str },
    #[error("environment variable {name} must not be the zero address")]
    ZeroAddress { name: &'static str },
    #[error("environment variable {name} must be greater than zero")]
    MustBePositive { name: &'static str },
    #[error("environment variable {name} value {value} must be in range {range}")]
    OutOfRange {
        name: &'static str,
        value: u64,
        range: &'static str,
    },
}

fn database_url(values: &HashMap<String, String>) -> Result<String, ConfigError> {
    let database_url = required(values, "DATABASE_URL")?;
    let parsed = parse_url("DATABASE_URL", &database_url)?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql") {
        return Err(ConfigError::UnsupportedScheme {
            name: "DATABASE_URL",
            scheme: parsed.scheme().to_owned(),
        });
    }
    Ok(database_url)
}

fn chain_id(values: &HashMap<String, String>) -> Result<u64, ConfigError> {
    let chain_id = parse_u64(values, "EVM_CHAIN_ID")?;
    ensure_positive("EVM_CHAIN_ID", chain_id)?;
    ensure_db_bigint("EVM_CHAIN_ID", chain_id)?;
    Ok(chain_id)
}

fn contract_address(values: &HashMap<String, String>) -> Result<Address, ConfigError> {
    let raw = required(values, "FORESYN_CONTRACT_ADDRESS")?;
    let address = raw.parse().map_err(|_| ConfigError::InvalidAddress {
        name: "FORESYN_CONTRACT_ADDRESS",
    })?;
    if address == Address::ZERO {
        return Err(ConfigError::ZeroAddress {
            name: "FORESYN_CONTRACT_ADDRESS",
        });
    }
    Ok(address)
}

fn required(values: &HashMap<String, String>, name: &'static str) -> Result<String, ConfigError> {
    values
        .get(name)
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .ok_or(ConfigError::Missing(name))
}

fn parse_url(name: &'static str, value: &str) -> Result<Url, ConfigError> {
    Url::parse(value).map_err(|_| ConfigError::InvalidUrl { name })
}

fn parse_u64(values: &HashMap<String, String>, name: &'static str) -> Result<u64, ConfigError> {
    required(values, name)?
        .parse()
        .map_err(|_| ConfigError::InvalidInteger { name })
}

fn ensure_positive(name: &'static str, value: u64) -> Result<(), ConfigError> {
    if value == 0 {
        Err(ConfigError::MustBePositive { name })
    } else {
        Ok(())
    }
}

fn ensure_db_bigint(name: &'static str, value: u64) -> Result<(), ConfigError> {
    if value > i64::MAX as u64 {
        Err(ConfigError::OutOfRange {
            name,
            value,
            range: "0..=9223372036854775807",
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{ApiConfig, ConfigError, IndexerConfig};

    fn valid_values() -> HashMap<String, String> {
        [
            (
                "DATABASE_URL",
                "postgres://foresyn:secret@localhost/foresyn",
            ),
            ("EVM_RPC_URL", "http://127.0.0.1:8545"),
            ("EVM_CHAIN_ID", "31337"),
            (
                "FORESYN_CONTRACT_ADDRESS",
                "0x1111111111111111111111111111111111111111",
            ),
            ("FORESYN_DEPLOYMENT_BLOCK", "42"),
            ("INDEXER_CONFIRMATIONS", "6"),
            ("INDEXER_BATCH_SIZE", "500"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
    }

    #[test]
    fn parses_valid_configuration() {
        let config = IndexerConfig::from_values(valid_values()).unwrap();

        assert_eq!(config.chain_id, 31_337);
        assert_eq!(config.deployment_block, 42);
        assert_eq!(config.confirmations, 6);
        assert_eq!(config.batch_size, 500);
    }

    #[test]
    fn api_config_does_not_require_rpc_or_indexer_settings() {
        let values = [
            (
                "DATABASE_URL",
                "postgres://foresyn:secret@localhost/foresyn",
            ),
            ("EVM_CHAIN_ID", "31337"),
            (
                "FORESYN_CONTRACT_ADDRESS",
                "0x1111111111111111111111111111111111111111",
            ),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();

        let config = ApiConfig::from_values(values).unwrap();

        assert_eq!(config.chain_id, 31_337);
        assert_eq!(config.bind_address.to_string(), "127.0.0.1:8080");
        assert_eq!(config.cors_origin, "http://localhost:5173");
    }

    #[test]
    fn rejects_missing_required_value() {
        let mut values = valid_values();
        values.remove("FORESYN_DEPLOYMENT_BLOCK");

        assert_eq!(
            IndexerConfig::from_values(values).unwrap_err(),
            ConfigError::Missing("FORESYN_DEPLOYMENT_BLOCK")
        );
    }

    #[test]
    fn rejects_invalid_chain_id_address_and_batch_size() {
        let mut values = valid_values();
        values.insert("EVM_CHAIN_ID".to_owned(), "0".to_owned());
        assert_eq!(
            IndexerConfig::from_values(values).unwrap_err(),
            ConfigError::MustBePositive {
                name: "EVM_CHAIN_ID"
            }
        );

        let mut values = valid_values();
        values.insert(
            "FORESYN_CONTRACT_ADDRESS".to_owned(),
            "not-an-address".to_owned(),
        );
        assert_eq!(
            IndexerConfig::from_values(values).unwrap_err(),
            ConfigError::InvalidAddress {
                name: "FORESYN_CONTRACT_ADDRESS"
            }
        );

        let mut values = valid_values();
        values.insert(
            "FORESYN_CONTRACT_ADDRESS".to_owned(),
            "0x0000000000000000000000000000000000000000".to_owned(),
        );
        assert_eq!(
            IndexerConfig::from_values(values).unwrap_err(),
            ConfigError::ZeroAddress {
                name: "FORESYN_CONTRACT_ADDRESS"
            }
        );

        let mut values = valid_values();
        values.insert("INDEXER_BATCH_SIZE".to_owned(), "0".to_owned());
        assert!(matches!(
            IndexerConfig::from_values(values),
            Err(ConfigError::OutOfRange {
                name: "INDEXER_BATCH_SIZE",
                ..
            })
        ));
    }

    #[test]
    fn rejects_unsupported_rpc_transport() {
        let mut values = valid_values();
        values.insert("EVM_RPC_URL".to_owned(), "ws://localhost:8545".to_owned());

        assert!(matches!(
            IndexerConfig::from_values(values),
            Err(ConfigError::UnsupportedScheme {
                name: "EVM_RPC_URL",
                ..
            })
        ));
    }

    #[test]
    fn rejects_values_that_cannot_be_persisted() {
        let mut values = valid_values();
        values.insert("INDEXER_CONFIRMATIONS".to_owned(), u64::MAX.to_string());
        assert!(matches!(
            IndexerConfig::from_values(values),
            Err(ConfigError::OutOfRange {
                name: "INDEXER_CONFIRMATIONS",
                ..
            })
        ));

        let mut values = valid_values();
        values.insert("DATABASE_URL".to_owned(), "mysql://localhost/db".to_owned());
        assert!(matches!(
            IndexerConfig::from_values(values),
            Err(ConfigError::UnsupportedScheme {
                name: "DATABASE_URL",
                ..
            })
        ));
    }
}
