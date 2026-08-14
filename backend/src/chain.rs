use alloy::{
    eips::BlockNumberOrTag,
    primitives::{Address, B256},
    providers::{DynProvider, Provider, ProviderBuilder},
    rpc::types::{Filter, Log},
};
use async_trait::async_trait;
use thiserror::Error;
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainBlock {
    pub number: u64,
    pub hash: B256,
    pub parent_hash: B256,
    pub timestamp: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainLog {
    pub block_number: u64,
    pub block_hash: B256,
    pub transaction_hash: B256,
    pub transaction_index: u64,
    pub log_index: u64,
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("RPC {operation} request failed: {message}")]
    Request {
        operation: &'static str,
        message: String,
    },
    #[error("RPC returned no block for requested block {0}")]
    BlockNotFound(u64),
    #[error("RPC log is missing {field}")]
    IncompleteLog { field: &'static str },
}

#[async_trait]
pub trait ChainSource: Send + Sync {
    async fn chain_id(&self) -> Result<u64, RpcError>;
    async fn latest_block_number(&self) -> Result<u64, RpcError>;
    async fn block_by_number(&self, number: u64) -> Result<ChainBlock, RpcError>;
    async fn logs(
        &self,
        from_block: u64,
        to_block: u64,
        address: Address,
        topic0: B256,
    ) -> Result<Vec<ChainLog>, RpcError>;
}

#[derive(Clone)]
pub struct AlloyChainSource {
    provider: DynProvider,
}

impl AlloyChainSource {
    pub fn new(rpc_url: Url) -> Self {
        let provider = ProviderBuilder::new().connect_http(rpc_url).erased();
        Self { provider }
    }
}

#[async_trait]
impl ChainSource for AlloyChainSource {
    async fn chain_id(&self) -> Result<u64, RpcError> {
        self.provider
            .get_chain_id()
            .await
            .map_err(|error| request_error("eth_chainId", error))
    }

    async fn latest_block_number(&self) -> Result<u64, RpcError> {
        self.provider
            .get_block_number()
            .await
            .map_err(|error| request_error("eth_blockNumber", error))
    }

    async fn block_by_number(&self, number: u64) -> Result<ChainBlock, RpcError> {
        let block = self
            .provider
            .get_block_by_number(BlockNumberOrTag::Number(number))
            .await
            .map_err(|error| request_error("eth_getBlockByNumber", error))?
            .ok_or(RpcError::BlockNotFound(number))?;

        Ok(ChainBlock {
            number: block.header.number,
            hash: block.header.hash,
            parent_hash: block.header.parent_hash,
            timestamp: block.header.timestamp,
        })
    }

    async fn logs(
        &self,
        from_block: u64,
        to_block: u64,
        address: Address,
        topic0: B256,
    ) -> Result<Vec<ChainLog>, RpcError> {
        let filter = Filter::new()
            .address(address)
            .event_signature(topic0)
            .from_block(from_block)
            .to_block(to_block);

        self.provider
            .get_logs(&filter)
            .await
            .map_err(|error| request_error("eth_getLogs", error))?
            .into_iter()
            .map(map_log)
            .collect()
    }
}

fn map_log(log: Log) -> Result<ChainLog, RpcError> {
    Ok(ChainLog {
        block_number: log.block_number.ok_or(RpcError::IncompleteLog {
            field: "block_number",
        })?,
        block_hash: log.block_hash.ok_or(RpcError::IncompleteLog {
            field: "block_hash",
        })?,
        transaction_hash: log.transaction_hash.ok_or(RpcError::IncompleteLog {
            field: "transaction_hash",
        })?,
        transaction_index: log.transaction_index.ok_or(RpcError::IncompleteLog {
            field: "transaction_index",
        })?,
        log_index: log
            .log_index
            .ok_or(RpcError::IncompleteLog { field: "log_index" })?,
        address: log.address(),
        topics: log.topics().to_vec(),
        data: log.data().data.to_vec(),
    })
}

fn request_error(operation: &'static str, error: impl std::fmt::Display) -> RpcError {
    RpcError::Request {
        operation,
        message: redact_rpc_urls(&error.to_string()),
    }
}

fn redact_rpc_urls(message: &str) -> String {
    let mut redacted = message.to_owned();
    while let Some(start) = ["http://", "https://"]
        .into_iter()
        .filter_map(|scheme| redacted.find(scheme))
        .min()
    {
        let end = redacted[start..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, ')' | ']' | '}' | '\'' | '"' | ',')
            })
            .map_or(redacted.len(), |offset| start + offset);
        redacted.replace_range(start..end, "<rpc-url>");
    }
    redacted
}

#[cfg(test)]
mod tests {
    use super::redact_rpc_urls;

    #[test]
    fn rpc_error_messages_do_not_expose_urls_or_credentials() {
        let message = "request to https://user:secret@example.test/private-key failed";
        let redacted = redact_rpc_urls(message);

        assert_eq!(redacted, "request to <rpc-url> failed");
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("private-key"));
    }
}
