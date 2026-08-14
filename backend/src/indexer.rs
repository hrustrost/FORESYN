use std::collections::BTreeMap;

use alloy::primitives::{Address, B256};
use async_trait::async_trait;
use thiserror::Error;
use tracing::{Instrument, debug, info, info_span};

use crate::{
    chain::{ChainBlock, ChainLog, ChainSource, RpcError},
    config::IndexerConfig,
    contracts::{DecodeError, decode_market_created, market_created_topic},
    db::{Checkpoint, Database, DbError, MarketCreatedRecord},
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunSummary {
    pub latest_block: u64,
    pub safe_head: Option<u64>,
    pub first_block: Option<u64>,
    pub last_block: Option<u64>,
    pub blocks_committed: u64,
    pub events_committed: u64,
}

#[derive(Debug, Error)]
pub enum IndexerError {
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    Database(#[from] DbError),
    #[error("configured chain id {configured} does not match RPC chain id {actual}")]
    ChainIdMismatch { configured: u64, actual: u64 },
    #[error("checkpoint block number cannot be incremented")]
    CheckpointOverflow,
    #[error(
        "stored checkpoint block {checkpoint_block} is above current RPC chain head {latest_block}"
    )]
    CheckpointAboveChainHead {
        checkpoint_block: u64,
        latest_block: u64,
    },
    #[error(
        "stored checkpoint block {checkpoint_block} is missing from the current RPC chain at head {latest_block}"
    )]
    CheckpointBlockMissing {
        checkpoint_block: u64,
        latest_block: u64,
    },
    #[error(
        "reorganization detected at stored checkpoint block {block_number}: expected hash {expected_hash}, canonical RPC hash {actual_hash}"
    )]
    CheckpointReorgDetected {
        block_number: u64,
        expected_hash: B256,
        actual_hash: B256,
    },
    #[error(
        "reorganization detected at block {block_number}: expected parent {expected_parent}, actual parent {actual_parent}"
    )]
    ReorgDetected {
        block_number: u64,
        expected_parent: B256,
        actual_parent: B256,
    },
    #[error("RPC returned block {actual} while block {expected} was requested")]
    UnexpectedBlockNumber { expected: u64, actual: u64 },
    #[error(
        "log at block {block_number}, transaction {transaction_hash}, index {log_index} references non-canonical block hash {actual_hash}; expected {expected_hash}"
    )]
    LogBlockHashMismatch {
        block_number: u64,
        transaction_hash: B256,
        log_index: u64,
        expected_hash: B256,
        actual_hash: B256,
    },
    #[error(
        "RPC returned log for block {block_number} outside requested range {from_block}..={to_block}"
    )]
    LogOutsideRange {
        block_number: u64,
        from_block: u64,
        to_block: u64,
    },
    #[error(
        "failed to decode MarketCreated at block {block_number}, transaction {transaction_hash}, log {log_index}: {source}"
    )]
    Decode {
        block_number: u64,
        transaction_hash: B256,
        log_index: u64,
        #[source]
        source: DecodeError,
    },
}

#[async_trait]
pub trait BlockStore: Send + Sync {
    async fn checkpoint(
        &self,
        chain_id: u64,
        contract_address: Address,
    ) -> Result<Option<Checkpoint>, DbError>;

    async fn commit_block(
        &self,
        chain_id: u64,
        contract_address: Address,
        block: &ChainBlock,
        events: &[MarketCreatedRecord],
    ) -> Result<(), DbError>;
}

#[async_trait]
impl BlockStore for Database {
    async fn checkpoint(
        &self,
        chain_id: u64,
        contract_address: Address,
    ) -> Result<Option<Checkpoint>, DbError> {
        self.checkpoint(chain_id, contract_address).await
    }

    async fn commit_block(
        &self,
        chain_id: u64,
        contract_address: Address,
        block: &ChainBlock,
        events: &[MarketCreatedRecord],
    ) -> Result<(), DbError> {
        self.commit_block(chain_id, contract_address, block, events)
            .await
    }
}

pub struct Indexer<S, D> {
    source: S,
    store: D,
    config: IndexerConfig,
}

impl<S, D> Indexer<S, D>
where
    S: ChainSource,
    D: BlockStore,
{
    pub const fn new(source: S, store: D, config: IndexerConfig) -> Self {
        Self {
            source,
            store,
            config,
        }
    }

    pub async fn run_once(&self) -> Result<RunSummary, IndexerError> {
        let span = info_span!(
            "indexer_run",
            chain_id = self.config.chain_id,
            contract_address = %self.config.contract_address,
            deployment_block = self.config.deployment_block,
            confirmations = self.config.confirmations,
            batch_size = self.config.batch_size,
        );
        self.run_once_inner().instrument(span).await
    }

    async fn run_once_inner(&self) -> Result<RunSummary, IndexerError> {
        let actual_chain_id = self.source.chain_id().await?;
        if actual_chain_id != self.config.chain_id {
            return Err(IndexerError::ChainIdMismatch {
                configured: self.config.chain_id,
                actual: actual_chain_id,
            });
        }

        let latest_block = self.source.latest_block_number().await?;
        let safe_head = safe_head(latest_block, self.config.confirmations);
        let checkpoint = self
            .store
            .checkpoint(self.config.chain_id, self.config.contract_address)
            .await?;
        if let Some(checkpoint) = checkpoint.as_ref() {
            self.verify_checkpoint(checkpoint, latest_block).await?;
        }
        let mut next_block = match checkpoint.as_ref() {
            Some(checkpoint) => checkpoint
                .block_number
                .checked_add(1)
                .ok_or(IndexerError::CheckpointOverflow)?,
            None => self.config.deployment_block,
        };

        let mut summary = RunSummary {
            latest_block,
            safe_head,
            ..RunSummary::default()
        };
        let Some(safe_head) = safe_head else {
            info!(
                latest_block,
                "chain head has not reached the confirmation depth"
            );
            return Ok(summary);
        };
        if next_block > safe_head {
            info!(latest_block, safe_head, next_block, "indexer is caught up");
            return Ok(summary);
        }

        let mut expected_parent = checkpoint.map(|checkpoint| checkpoint.block_hash);
        summary.first_block = Some(next_block);

        while next_block <= safe_head {
            let batch_end = next_block
                .saturating_add(self.config.batch_size - 1)
                .min(safe_head);
            let batch_span = info_span!(
                "indexer_batch",
                from_block = next_block,
                to_block = batch_end
            );
            let batch_result = self
                .process_batch(next_block, batch_end, &mut expected_parent, &mut summary)
                .instrument(batch_span)
                .await;
            batch_result?;
            next_block = batch_end
                .checked_add(1)
                .ok_or(IndexerError::CheckpointOverflow)?;
        }

        info!(
            blocks_committed = summary.blocks_committed,
            events_committed = summary.events_committed,
            last_block = summary.last_block,
            "indexer catch-up completed"
        );
        Ok(summary)
    }

    async fn verify_checkpoint(
        &self,
        checkpoint: &Checkpoint,
        latest_block: u64,
    ) -> Result<(), IndexerError> {
        if checkpoint.block_number > latest_block {
            return Err(IndexerError::CheckpointAboveChainHead {
                checkpoint_block: checkpoint.block_number,
                latest_block,
            });
        }

        let canonical_block = match self.source.block_by_number(checkpoint.block_number).await {
            Ok(block) => block,
            Err(RpcError::BlockNotFound(_)) => {
                return Err(IndexerError::CheckpointBlockMissing {
                    checkpoint_block: checkpoint.block_number,
                    latest_block,
                });
            }
            Err(error) => return Err(error.into()),
        };
        if canonical_block.number != checkpoint.block_number {
            return Err(IndexerError::UnexpectedBlockNumber {
                expected: checkpoint.block_number,
                actual: canonical_block.number,
            });
        }
        if canonical_block.hash != checkpoint.block_hash {
            return Err(IndexerError::CheckpointReorgDetected {
                block_number: checkpoint.block_number,
                expected_hash: checkpoint.block_hash,
                actual_hash: canonical_block.hash,
            });
        }

        debug!(
            checkpoint_block = checkpoint.block_number,
            checkpoint_hash = %checkpoint.block_hash,
            "stored checkpoint remains canonical"
        );
        Ok(())
    }

    async fn process_batch(
        &self,
        from_block: u64,
        to_block: u64,
        expected_parent: &mut Option<B256>,
        summary: &mut RunSummary,
    ) -> Result<(), IndexerError> {
        let logs = self
            .source
            .logs(
                from_block,
                to_block,
                self.config.contract_address,
                market_created_topic(),
            )
            .await?;
        let mut logs_by_block = group_logs(logs, from_block, to_block)?;

        for block_number in from_block..=to_block {
            let block = self.source.block_by_number(block_number).await?;
            if block.number != block_number {
                return Err(IndexerError::UnexpectedBlockNumber {
                    expected: block_number,
                    actual: block.number,
                });
            }
            ensure_parent(&block, *expected_parent)?;

            let mut block_logs = logs_by_block.remove(&block_number).unwrap_or_default();
            block_logs.sort_by_key(|log| (log.transaction_index, log.log_index));
            let records = decode_block_logs(self.config.contract_address, &block, block_logs)?;

            let block_span = info_span!(
                "indexer_block_commit",
                block_number,
                block_hash = %block.hash,
                event_count = records.len()
            );
            self.store
                .commit_block(
                    self.config.chain_id,
                    self.config.contract_address,
                    &block,
                    &records,
                )
                .instrument(block_span)
                .await?;

            debug!(
                block_number,
                events = records.len(),
                "canonical block committed"
            );
            *expected_parent = Some(block.hash);
            summary.blocks_committed += 1;
            summary.events_committed += records.len() as u64;
            summary.last_block = Some(block_number);
        }

        Ok(())
    }
}

pub const fn safe_head(latest_block: u64, confirmations: u64) -> Option<u64> {
    latest_block.checked_sub(confirmations)
}

fn group_logs(
    logs: Vec<ChainLog>,
    from_block: u64,
    to_block: u64,
) -> Result<BTreeMap<u64, Vec<ChainLog>>, IndexerError> {
    let mut grouped = BTreeMap::new();
    for log in logs {
        if !(from_block..=to_block).contains(&log.block_number) {
            return Err(IndexerError::LogOutsideRange {
                block_number: log.block_number,
                from_block,
                to_block,
            });
        }
        grouped
            .entry(log.block_number)
            .or_insert_with(Vec::new)
            .push(log);
    }
    Ok(grouped)
}

fn ensure_parent(block: &ChainBlock, expected_parent: Option<B256>) -> Result<(), IndexerError> {
    if let Some(expected_parent) = expected_parent
        && block.parent_hash != expected_parent
    {
        return Err(IndexerError::ReorgDetected {
            block_number: block.number,
            expected_parent,
            actual_parent: block.parent_hash,
        });
    }
    Ok(())
}

fn decode_block_logs(
    contract_address: Address,
    block: &ChainBlock,
    logs: Vec<ChainLog>,
) -> Result<Vec<MarketCreatedRecord>, IndexerError> {
    let mut records = Vec::new();
    for log in logs {
        if log.address != contract_address || log.topics.first() != Some(&market_created_topic()) {
            continue;
        }
        if log.block_hash != block.hash {
            return Err(IndexerError::LogBlockHashMismatch {
                block_number: log.block_number,
                transaction_hash: log.transaction_hash,
                log_index: log.log_index,
                expected_hash: block.hash,
                actual_hash: log.block_hash,
            });
        }
        let projection = decode_market_created(&log.topics, &log.data).map_err(|source| {
            IndexerError::Decode {
                block_number: log.block_number,
                transaction_hash: log.transaction_hash,
                log_index: log.log_index,
                source,
            }
        })?;
        records.push(MarketCreatedRecord { log, projection });
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };

    use alloy::{
        primitives::{Address, B256, U256},
        sol_types::SolEvent,
    };
    use async_trait::async_trait;
    use url::Url;

    use super::{BlockStore, Indexer, IndexerError, safe_head};
    use crate::{
        chain::{ChainBlock, ChainLog, ChainSource, RpcError},
        config::IndexerConfig,
        contracts::{MarketCreated, market_created_topic},
        db::{Checkpoint, DbError, MarketCreatedRecord},
    };

    type RequestedRange = (u64, u64, Address, B256);

    #[derive(Clone, Debug)]
    struct FakeSource {
        chain_id: u64,
        latest: u64,
        blocks: Arc<BTreeMap<u64, ChainBlock>>,
        logs: Arc<Vec<ChainLog>>,
        requested_ranges: Arc<Mutex<Vec<RequestedRange>>>,
        requested_blocks: Arc<Mutex<Vec<u64>>>,
    }

    #[async_trait]
    impl ChainSource for FakeSource {
        async fn chain_id(&self) -> Result<u64, RpcError> {
            Ok(self.chain_id)
        }

        async fn latest_block_number(&self) -> Result<u64, RpcError> {
            Ok(self.latest)
        }

        async fn block_by_number(&self, number: u64) -> Result<ChainBlock, RpcError> {
            self.requested_blocks.lock().unwrap().push(number);
            self.blocks
                .get(&number)
                .cloned()
                .ok_or(RpcError::BlockNotFound(number))
        }

        async fn logs(
            &self,
            from_block: u64,
            to_block: u64,
            address: Address,
            topic0: B256,
        ) -> Result<Vec<ChainLog>, RpcError> {
            self.requested_ranges
                .lock()
                .unwrap()
                .push((from_block, to_block, address, topic0));
            Ok(self
                .logs
                .iter()
                .filter(|log| (from_block..=to_block).contains(&log.block_number))
                .cloned()
                .collect())
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakeStore {
        state: Arc<Mutex<FakeStoreState>>,
    }

    #[derive(Debug, Default)]
    struct FakeStoreState {
        checkpoint: Option<Checkpoint>,
        commits: Vec<(ChainBlock, Vec<MarketCreatedRecord>)>,
    }

    #[async_trait]
    impl BlockStore for FakeStore {
        async fn checkpoint(
            &self,
            _chain_id: u64,
            _contract_address: Address,
        ) -> Result<Option<Checkpoint>, DbError> {
            Ok(self.state.lock().unwrap().checkpoint.clone())
        }

        async fn commit_block(
            &self,
            _chain_id: u64,
            _contract_address: Address,
            block: &ChainBlock,
            events: &[MarketCreatedRecord],
        ) -> Result<(), DbError> {
            let mut state = self.state.lock().unwrap();
            state.checkpoint = Some(Checkpoint {
                block_number: block.number,
                block_hash: block.hash,
            });
            state.commits.push((block.clone(), events.to_vec()));
            Ok(())
        }
    }

    fn config(deployment_block: u64, batch_size: u64) -> IndexerConfig {
        IndexerConfig {
            database_url: "postgres://unused".to_owned(),
            rpc_url: Url::parse("http://127.0.0.1:8545").unwrap(),
            chain_id: 31_337,
            contract_address: Address::repeat_byte(0x44),
            deployment_block,
            confirmations: 0,
            batch_size,
        }
    }

    fn blocks(from: u64, to: u64) -> BTreeMap<u64, ChainBlock> {
        (from..=to)
            .map(|number| {
                let hash = B256::with_last_byte(number as u8);
                let parent_hash = B256::with_last_byte(number.saturating_sub(1) as u8);
                (
                    number,
                    ChainBlock {
                        number,
                        hash,
                        parent_hash,
                        timestamp: 1_900_000_000 + number,
                    },
                )
            })
            .collect()
    }

    fn source(latest: u64, blocks: BTreeMap<u64, ChainBlock>, logs: Vec<ChainLog>) -> FakeSource {
        FakeSource {
            chain_id: 31_337,
            latest,
            blocks: Arc::new(blocks),
            logs: Arc::new(logs),
            requested_ranges: Arc::default(),
            requested_blocks: Arc::default(),
        }
    }

    fn market_log(block: &ChainBlock, address: Address) -> ChainLog {
        let event = MarketCreated {
            marketId: U256::from(99),
            resolver: Address::repeat_byte(0x11),
            creator: Address::repeat_byte(0x22),
            deadline: 1_900_000_999,
            metadataDigest: B256::repeat_byte(0x33),
        };
        let encoded = event.encode_log_data();
        ChainLog {
            block_number: block.number,
            block_hash: block.hash,
            transaction_hash: B256::repeat_byte(0x55),
            transaction_index: 2,
            log_index: 3,
            address,
            topics: encoded.topics().to_vec(),
            data: encoded.data.to_vec(),
        }
    }

    #[test]
    fn safe_head_handles_underflow() {
        assert_eq!(safe_head(100, 6), Some(94));
        assert_eq!(safe_head(5, 6), None);
        assert_eq!(safe_head(0, 0), Some(0));
    }

    #[tokio::test]
    async fn indexes_ordered_bounded_ranges_and_advances_empty_blocks() {
        let config = config(10, 2);
        let blocks = blocks(10, 14);
        let log = market_log(blocks.get(&12).unwrap(), config.contract_address);
        let source = source(14, blocks, vec![log]);
        let requested_ranges = Arc::clone(&source.requested_ranges);
        let store = FakeStore::default();
        let state = Arc::clone(&store.state);
        let expected_contract = config.contract_address;

        let summary = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap();

        assert_eq!(summary.blocks_committed, 5);
        assert_eq!(summary.events_committed, 1);
        assert_eq!(summary.first_block, Some(10));
        assert_eq!(summary.last_block, Some(14));
        let requested_ranges = requested_ranges.lock().unwrap();
        assert_eq!(
            requested_ranges
                .iter()
                .map(|(from, to, _, _)| (*from, *to))
                .collect::<Vec<_>>(),
            vec![(10, 11), (12, 13), (14, 14)]
        );
        assert!(
            requested_ranges
                .iter()
                .all(|(_, _, address, topic)| *address == expected_contract
                    && *topic == market_created_topic())
        );
        assert_eq!(
            state
                .lock()
                .unwrap()
                .commits
                .iter()
                .map(|(block, events)| (block.number, events.len()))
                .collect::<Vec<_>>(),
            vec![(10, 0), (11, 0), (12, 1), (13, 0), (14, 0)]
        );
    }

    #[tokio::test]
    async fn canonical_checkpoint_allows_normal_restart() {
        let config = config(10, 10);
        let source = source(14, blocks(12, 14), vec![]);
        let requested_blocks = Arc::clone(&source.requested_blocks);
        let store = FakeStore::default();
        store.state.lock().unwrap().checkpoint = Some(Checkpoint {
            block_number: 12,
            block_hash: B256::with_last_byte(12),
        });

        let summary = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap();

        assert_eq!(summary.first_block, Some(13));
        assert_eq!(*requested_blocks.lock().unwrap(), vec![12, 13, 14]);
    }

    #[tokio::test]
    async fn changed_checkpoint_hash_is_detected_while_caught_up() {
        let config = config(10, 10);
        let mut chain_blocks = blocks(12, 12);
        chain_blocks.get_mut(&12).unwrap().hash = B256::repeat_byte(0xee);
        let source = source(12, chain_blocks, vec![]);
        let requested_blocks = Arc::clone(&source.requested_blocks);
        let requested_ranges = Arc::clone(&source.requested_ranges);
        let store = FakeStore::default();
        store.state.lock().unwrap().checkpoint = Some(Checkpoint {
            block_number: 12,
            block_hash: B256::with_last_byte(12),
        });
        let state = Arc::clone(&store.state);

        let error = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            IndexerError::CheckpointReorgDetected {
                block_number: 12,
                expected_hash,
                actual_hash,
            } if expected_hash == B256::with_last_byte(12)
                && actual_hash == B256::repeat_byte(0xee)
        ));
        assert_eq!(*requested_blocks.lock().unwrap(), vec![12]);
        assert!(requested_ranges.lock().unwrap().is_empty());
        assert!(state.lock().unwrap().commits.is_empty());
    }

    #[tokio::test]
    async fn changed_checkpoint_hash_is_detected_before_newer_blocks() {
        let config = config(10, 10);
        let mut chain_blocks = blocks(12, 14);
        chain_blocks.get_mut(&12).unwrap().hash = B256::repeat_byte(0xee);
        let source = source(14, chain_blocks, vec![]);
        let requested_blocks = Arc::clone(&source.requested_blocks);
        let requested_ranges = Arc::clone(&source.requested_ranges);
        let store = FakeStore::default();
        store.state.lock().unwrap().checkpoint = Some(Checkpoint {
            block_number: 12,
            block_hash: B256::with_last_byte(12),
        });
        let state = Arc::clone(&store.state);

        let error = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            IndexerError::CheckpointReorgDetected {
                block_number: 12,
                ..
            }
        ));
        assert_eq!(*requested_blocks.lock().unwrap(), vec![12]);
        assert!(requested_ranges.lock().unwrap().is_empty());
        assert!(state.lock().unwrap().commits.is_empty());
    }

    #[tokio::test]
    async fn unavailable_checkpoint_block_is_reported_explicitly() {
        let config = config(10, 10);
        let missing_source = source(14, blocks(13, 14), vec![]);
        let store = FakeStore::default();
        store.state.lock().unwrap().checkpoint = Some(Checkpoint {
            block_number: 12,
            block_hash: B256::with_last_byte(12),
        });

        let error = Indexer::new(missing_source, store, config.clone())
            .run_once()
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            IndexerError::CheckpointBlockMissing {
                checkpoint_block: 12,
                latest_block: 14,
            }
        ));

        let source = source(14, BTreeMap::new(), vec![]);
        let requested_blocks = Arc::clone(&source.requested_blocks);
        let store = FakeStore::default();
        store.state.lock().unwrap().checkpoint = Some(Checkpoint {
            block_number: 15,
            block_hash: B256::with_last_byte(15),
        });

        let error = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            IndexerError::CheckpointAboveChainHead {
                checkpoint_block: 15,
                latest_block: 14,
            }
        ));
        assert!(requested_blocks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn parent_mismatch_stops_before_committing_block() {
        let config = config(10, 10);
        let mut chain_blocks = blocks(10, 11);
        chain_blocks.get_mut(&11).unwrap().parent_hash = B256::repeat_byte(0xee);
        let source = source(11, chain_blocks, vec![]);
        let store = FakeStore::default();
        store.state.lock().unwrap().checkpoint = Some(Checkpoint {
            block_number: 10,
            block_hash: B256::with_last_byte(10),
        });
        let state = Arc::clone(&store.state);

        let error = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            IndexerError::ReorgDetected {
                block_number: 11,
                ..
            }
        ));
        assert!(state.lock().unwrap().commits.is_empty());
    }

    #[tokio::test]
    async fn ignores_wrong_contract_log() {
        let config = config(10, 10);
        let chain_blocks = blocks(10, 10);
        let log = market_log(chain_blocks.get(&10).unwrap(), Address::repeat_byte(0xaa));
        let source = source(10, chain_blocks, vec![log]);
        let store = FakeStore::default();
        let state = Arc::clone(&store.state);

        Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap();

        assert_eq!(state.lock().unwrap().commits[0].1.len(), 0);
    }

    #[tokio::test]
    async fn malformed_matching_log_does_not_commit_block() {
        let config = config(10, 10);
        let chain_blocks = blocks(10, 10);
        let mut log = market_log(chain_blocks.get(&10).unwrap(), config.contract_address);
        log.data.truncate(31);
        let source = source(10, chain_blocks, vec![log]);
        let store = FakeStore::default();
        let state = Arc::clone(&store.state);

        let error = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap_err();

        assert!(matches!(error, IndexerError::Decode { .. }));
        assert!(state.lock().unwrap().commits.is_empty());
    }
}
