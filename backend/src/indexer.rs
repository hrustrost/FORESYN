use std::collections::BTreeMap;

use alloy::primitives::{Address, B256};
use async_trait::async_trait;
use thiserror::Error;
use tracing::{Instrument, debug, info, info_span, warn};

use crate::{
    chain::{ChainBlock, ChainLog, ChainSource, RpcError},
    config::IndexerConfig,
    contracts::{DecodeError, decode_known_event, market_created_topic, position_taken_topic},
    db::{Checkpoint, Database, DbError, EventRecord, RollbackSummary},
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
        "stored checkpoint block {checkpoint_block} is below configured deployment block {deployment_block}"
    )]
    CheckpointBeforeDeployment {
        checkpoint_block: u64,
        deployment_block: u64,
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
    #[error("stored indexed block {block_number} is missing during common-ancestor search")]
    StoredBlockMissingDuringAncestorSearch { block_number: u64 },
    #[error("reorganization recovery cannot continue because the checkpoint disappeared")]
    RecoveryCheckpointMissing,
    #[error("RPC block {block_number} is missing during common-ancestor search")]
    RpcBlockMissingDuringAncestorSearch { block_number: u64 },
    #[error(
        "no common ancestor exists between checkpoint block {checkpoint_block} and deployment block {deployment_block}"
    )]
    NoCommonAncestor {
        checkpoint_block: u64,
        deployment_block: u64,
    },
    #[error(
        "another reorganization was detected at block {block_number} after one automatic recovery attempt"
    )]
    RecoveryLimitExceeded { block_number: u64 },
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
        "failed to decode known contract event at block {block_number}, transaction {transaction_hash}, log {log_index}: {source}"
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

    async fn indexed_block_hash(
        &self,
        chain_id: u64,
        block_number: u64,
    ) -> Result<Option<B256>, DbError>;

    async fn rollback_to_ancestor(
        &self,
        chain_id: u64,
        contract_address: Address,
        deployment_block: u64,
        ancestor: &Checkpoint,
    ) -> Result<RollbackSummary, DbError>;

    async fn commit_block(
        &self,
        chain_id: u64,
        contract_address: Address,
        block: &ChainBlock,
        events: &[EventRecord],
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

    async fn indexed_block_hash(
        &self,
        chain_id: u64,
        block_number: u64,
    ) -> Result<Option<B256>, DbError> {
        self.indexed_block_hash(chain_id, block_number).await
    }

    async fn rollback_to_ancestor(
        &self,
        chain_id: u64,
        contract_address: Address,
        deployment_block: u64,
        ancestor: &Checkpoint,
    ) -> Result<RollbackSummary, DbError> {
        self.rollback_to_ancestor(chain_id, contract_address, deployment_block, ancestor)
            .await
    }

    async fn commit_block(
        &self,
        chain_id: u64,
        contract_address: Address,
        block: &ChainBlock,
        events: &[EventRecord],
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

        match self.run_catch_up_attempt().await {
            Ok(summary) => Ok(summary),
            Err(error) => {
                let Some(reorg_block) = recoverable_reorg_block(&error) else {
                    return Err(error);
                };
                warn!(
                    block_number = reorg_block,
                    error = %error,
                    "reorganization detected"
                );
                let ancestor = self.recover_from_reorg().await?;
                info!(
                    ancestor_block = ancestor.block_number,
                    ancestor_hash = %ancestor.block_hash,
                    "canonical replay resumed"
                );

                match self.run_catch_up_attempt().await {
                    Err(second_error) if recoverable_reorg_block(&second_error).is_some() => {
                        let block_number = recoverable_reorg_block(&second_error)
                            .expect("reorganization was matched above");
                        Err(IndexerError::RecoveryLimitExceeded { block_number })
                    }
                    result => result,
                }
            }
        }
    }

    async fn run_catch_up_attempt(&self) -> Result<RunSummary, IndexerError> {
        let latest_block = self.source.latest_block_number().await?;
        let safe_head = safe_head(latest_block, self.config.confirmations);
        let checkpoint = self
            .store
            .checkpoint(self.config.chain_id, self.config.contract_address)
            .await?;
        if let Some(checkpoint) = checkpoint.as_ref() {
            self.validate_checkpoint_window(checkpoint)?;
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

    async fn recover_from_reorg(&self) -> Result<Checkpoint, IndexerError> {
        let checkpoint = self
            .store
            .checkpoint(self.config.chain_id, self.config.contract_address)
            .await?
            .ok_or(IndexerError::RecoveryCheckpointMissing)?;
        let ancestor = self.find_common_ancestor(&checkpoint).await?;

        info!(
            ancestor_block = ancestor.block_number,
            ancestor_hash = %ancestor.block_hash,
            "rollback started"
        );
        let rollback = self
            .store
            .rollback_to_ancestor(
                self.config.chain_id,
                self.config.contract_address,
                self.config.deployment_block,
                &ancestor,
            )
            .await?;
        info!(
            ancestor_block = rollback.ancestor.block_number,
            orphaned_blocks = rollback.orphaned_blocks,
            orphaned_events = rollback.orphaned_events,
            orphaned_market_projections = rollback.orphaned_market_projections,
            rebuilt_position_events = rollback.rebuilt_position_events,
            "rollback committed"
        );
        Ok(ancestor)
    }

    async fn find_common_ancestor(
        &self,
        checkpoint: &Checkpoint,
    ) -> Result<Checkpoint, IndexerError> {
        self.validate_checkpoint_window(checkpoint)?;

        info!(
            checkpoint_block = checkpoint.block_number,
            deployment_block = self.config.deployment_block,
            "common ancestor search started"
        );

        for block_number in (self.config.deployment_block..=checkpoint.block_number).rev() {
            let stored_hash = self
                .store
                .indexed_block_hash(self.config.chain_id, block_number)
                .await?
                .ok_or(IndexerError::StoredBlockMissingDuringAncestorSearch { block_number })?;
            let canonical_block = match self.source.block_by_number(block_number).await {
                Ok(block) => block,
                Err(RpcError::BlockNotFound(_)) => {
                    return Err(IndexerError::RpcBlockMissingDuringAncestorSearch { block_number });
                }
                Err(error) => return Err(error.into()),
            };
            if canonical_block.number != block_number {
                return Err(IndexerError::UnexpectedBlockNumber {
                    expected: block_number,
                    actual: canonical_block.number,
                });
            }

            if stored_hash == canonical_block.hash {
                info!(
                    ancestor_block = block_number,
                    ancestor_hash = %stored_hash,
                    "common ancestor found"
                );
                return Ok(Checkpoint {
                    block_number,
                    block_hash: stored_hash,
                });
            }
            debug!(
                block_number,
                stored_hash = %stored_hash,
                canonical_hash = %canonical_block.hash,
                "divergent block comparison"
            );
        }

        Err(IndexerError::NoCommonAncestor {
            checkpoint_block: checkpoint.block_number,
            deployment_block: self.config.deployment_block,
        })
    }

    fn validate_checkpoint_window(&self, checkpoint: &Checkpoint) -> Result<(), IndexerError> {
        if checkpoint.block_number < self.config.deployment_block {
            return Err(IndexerError::CheckpointBeforeDeployment {
                checkpoint_block: checkpoint.block_number,
                deployment_block: self.config.deployment_block,
            });
        }
        Ok(())
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
        let mut logs = self
            .source
            .logs(
                from_block,
                to_block,
                self.config.contract_address,
                market_created_topic(),
            )
            .await?;
        logs.extend(
            self.source
                .logs(
                    from_block,
                    to_block,
                    self.config.contract_address,
                    position_taken_topic(),
                )
                .await?,
        );
        logs.sort_by_key(|log| (log.block_number, log.transaction_index, log.log_index));
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

fn recoverable_reorg_block(error: &IndexerError) -> Option<u64> {
    match error {
        IndexerError::CheckpointReorgDetected { block_number, .. }
        | IndexerError::ReorgDetected { block_number, .. }
        | IndexerError::LogBlockHashMismatch { block_number, .. } => Some(*block_number),
        _ => None,
    }
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
) -> Result<Vec<EventRecord>, IndexerError> {
    let mut records = Vec::new();
    for log in logs {
        if log.address != contract_address {
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
        let event =
            decode_known_event(&log.topics, &log.data).map_err(|source| IndexerError::Decode {
                block_number: log.block_number,
                transaction_hash: log.transaction_hash,
                log_index: log.log_index,
                source,
            })?;
        if let Some(event) = event {
            records.push(EventRecord { log, event });
        }
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
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
        contracts::{
            BinaryOutcome, DecodedEvent, MarketCreated, Outcome, PositionTaken,
            market_created_topic, position_taken_topic,
        },
        db::{Checkpoint, DbError, EventRecord, RollbackSummary},
    };

    type RequestedRange = (u64, u64, Address, B256);

    #[derive(Clone, Debug)]
    struct FakeSource {
        chain_id: u64,
        latest: u64,
        blocks: Arc<Mutex<BTreeMap<u64, VecDeque<ChainBlock>>>>,
        logs: Arc<Mutex<BTreeMap<B256, VecDeque<Vec<ChainLog>>>>>,
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
            let mut blocks = self.blocks.lock().unwrap();
            let responses = blocks
                .get_mut(&number)
                .ok_or(RpcError::BlockNotFound(number))?;
            if responses.len() > 1 {
                Ok(responses.pop_front().expect("response queue is not empty"))
            } else {
                responses
                    .front()
                    .cloned()
                    .ok_or(RpcError::BlockNotFound(number))
            }
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
            let mut responses_by_topic = self.logs.lock().unwrap();
            let Some(responses) = responses_by_topic.get_mut(&topic0) else {
                return Ok(Vec::new());
            };
            let logs = if responses.len() > 1 {
                responses
                    .pop_front()
                    .expect("log response queue is not empty")
            } else {
                responses
                    .front()
                    .cloned()
                    .expect("log response queue is not empty")
            };
            Ok(logs
                .into_iter()
                .filter(|log| {
                    (from_block..=to_block).contains(&log.block_number)
                        && log.address == address
                        && log.topics.first() == Some(&topic0)
                })
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
        indexed_blocks: BTreeMap<u64, B256>,
        commits: Vec<(ChainBlock, Vec<EventRecord>)>,
        rollbacks: Vec<Checkpoint>,
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

        async fn indexed_block_hash(
            &self,
            _chain_id: u64,
            block_number: u64,
        ) -> Result<Option<B256>, DbError> {
            Ok(self
                .state
                .lock()
                .unwrap()
                .indexed_blocks
                .get(&block_number)
                .copied())
        }

        async fn rollback_to_ancestor(
            &self,
            _chain_id: u64,
            _contract_address: Address,
            _deployment_block: u64,
            ancestor: &Checkpoint,
        ) -> Result<RollbackSummary, DbError> {
            let mut state = self.state.lock().unwrap();
            let orphaned_blocks = state
                .indexed_blocks
                .keys()
                .filter(|number| **number > ancestor.block_number)
                .count() as u64;
            let orphaned_events = state
                .commits
                .iter()
                .filter(|(block, _)| block.number > ancestor.block_number)
                .map(|(_, events)| events.len() as u64)
                .sum();
            state
                .indexed_blocks
                .retain(|number, _| *number <= ancestor.block_number);
            state
                .commits
                .retain(|(block, _)| block.number <= ancestor.block_number);
            state.checkpoint = Some(ancestor.clone());
            state.rollbacks.push(ancestor.clone());
            Ok(RollbackSummary {
                ancestor: ancestor.clone(),
                orphaned_blocks,
                orphaned_events,
                orphaned_market_projections: orphaned_events,
                rebuilt_position_events: 0,
            })
        }

        async fn commit_block(
            &self,
            _chain_id: u64,
            _contract_address: Address,
            block: &ChainBlock,
            events: &[EventRecord],
        ) -> Result<(), DbError> {
            let mut state = self.state.lock().unwrap();
            state.checkpoint = Some(Checkpoint {
                block_number: block.number,
                block_hash: block.hash,
            });
            state.indexed_blocks.insert(block.number, block.hash);
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

    fn chain_block(number: u64, hash: u8, parent_hash: u8) -> ChainBlock {
        ChainBlock {
            number,
            hash: B256::repeat_byte(hash),
            parent_hash: B256::repeat_byte(parent_hash),
            timestamp: 1_900_000_000 + number,
        }
    }

    fn seed_store(store: &FakeStore, local_blocks: &[ChainBlock]) {
        let mut state = store.state.lock().unwrap();
        for block in local_blocks {
            state.indexed_blocks.insert(block.number, block.hash);
            state.commits.push((block.clone(), Vec::new()));
        }
        let checkpoint = local_blocks.last().expect("at least one local block");
        state.checkpoint = Some(Checkpoint {
            block_number: checkpoint.number,
            block_hash: checkpoint.hash,
        });
    }

    fn source(latest: u64, blocks: BTreeMap<u64, ChainBlock>, logs: Vec<ChainLog>) -> FakeSource {
        scripted_source(
            latest,
            blocks
                .into_iter()
                .map(|(number, block)| (number, vec![block]))
                .collect(),
            logs,
        )
    }

    fn scripted_source(
        latest: u64,
        blocks: BTreeMap<u64, Vec<ChainBlock>>,
        logs: Vec<ChainLog>,
    ) -> FakeSource {
        scripted_source_with_topic_logs(
            latest,
            blocks,
            BTreeMap::from([
                (market_created_topic(), vec![logs.clone()]),
                (position_taken_topic(), vec![logs]),
            ]),
        )
    }

    fn scripted_source_with_logs(
        latest: u64,
        blocks: BTreeMap<u64, Vec<ChainBlock>>,
        log_responses: Vec<Vec<ChainLog>>,
    ) -> FakeSource {
        scripted_source_with_topic_logs(
            latest,
            blocks,
            BTreeMap::from([(market_created_topic(), log_responses)]),
        )
    }

    fn scripted_source_with_topic_logs(
        latest: u64,
        blocks: BTreeMap<u64, Vec<ChainBlock>>,
        log_responses: BTreeMap<B256, Vec<Vec<ChainLog>>>,
    ) -> FakeSource {
        FakeSource {
            chain_id: 31_337,
            latest,
            blocks: Arc::new(Mutex::new(
                blocks
                    .into_iter()
                    .map(|(number, blocks)| (number, blocks.into()))
                    .collect(),
            )),
            logs: Arc::new(Mutex::new(
                log_responses
                    .into_iter()
                    .map(|(topic, responses)| (topic, responses.into()))
                    .collect(),
            )),
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

    fn position_log(
        block: &ChainBlock,
        address: Address,
        transaction_index: u64,
        log_index: u64,
        outcome: Outcome,
    ) -> ChainLog {
        let event = PositionTaken {
            marketId: U256::from(99),
            user: Address::repeat_byte(0x77),
            outcome,
            amount: U256::from(2),
            userOutcomeStake: U256::from(7),
            yesPool: U256::from(11),
            noPool: U256::from(13),
        };
        let encoded = event.encode_log_data();
        ChainLog {
            block_number: block.number,
            block_hash: block.hash,
            transaction_hash: B256::with_last_byte(transaction_index as u8 + 0x60),
            transaction_index,
            log_index,
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
                .filter(|(_, _, _, topic)| *topic == market_created_topic())
                .map(|(from, to, _, _)| (*from, *to))
                .collect::<Vec<_>>(),
            vec![(10, 11), (12, 13), (14, 14)]
        );
        assert!(
            requested_ranges
                .iter()
                .all(|(_, _, address, topic)| *address == expected_contract
                    && (*topic == market_created_topic() || *topic == position_taken_topic()))
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
    async fn merges_event_queries_in_deterministic_evm_order() {
        let config = config(10, 10);
        let chain_blocks = blocks(10, 10);
        let block = chain_blocks.get(&10).unwrap();
        let mut created = market_log(block, config.contract_address);
        created.transaction_index = 0;
        created.log_index = 8;
        let position = position_log(block, config.contract_address, 1, 2, Outcome::Yes);
        let source = source(10, chain_blocks, vec![position, created]);
        let store = FakeStore::default();
        let state = Arc::clone(&store.state);

        let summary = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap();

        assert_eq!(summary.events_committed, 2);
        let state = state.lock().unwrap();
        let events = &state.commits[0].1;
        assert!(matches!(events[0].event, DecodedEvent::MarketCreated(_)));
        assert!(matches!(
            events[1].event,
            DecodedEvent::PositionTaken(ref projection)
                if projection.outcome == BinaryOutcome::Yes
        ));
        assert_eq!(
            events
                .iter()
                .map(|event| (event.log.transaction_index, event.log.log_index))
                .collect::<Vec<_>>(),
            vec![(0, 8), (1, 2)]
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
    async fn shallow_checkpoint_reorg_recovers_and_replays_without_duplicates() {
        let config = config(100, 10);
        let ancestor = chain_block(100, 0xa0, 0x99);
        let orphaned = chain_block(101, 0xb1, 0xa0);
        let replacement = chain_block(101, 0xc1, 0xa0);
        let log = market_log(&replacement, config.contract_address);
        let source = source(
            101,
            [(100, ancestor.clone()), (101, replacement.clone())].into(),
            vec![log],
        );
        let store = FakeStore::default();
        seed_store(&store, &[ancestor.clone(), orphaned]);
        let state = Arc::clone(&store.state);
        let indexer = Indexer::new(source, store, config);

        let summary = indexer.run_once().await.unwrap();

        assert_eq!(summary.first_block, Some(101));
        assert_eq!(summary.last_block, Some(101));
        assert_eq!((summary.blocks_committed, summary.events_committed), (1, 1));
        {
            let state_after_recovery = state.lock().unwrap();
            assert_eq!(
                state_after_recovery.rollbacks,
                vec![Checkpoint {
                    block_number: 100,
                    block_hash: ancestor.hash,
                }]
            );
            assert_eq!(
                state_after_recovery.checkpoint.as_ref().unwrap().block_hash,
                replacement.hash
            );
            assert_eq!(
                state_after_recovery
                    .indexed_blocks
                    .iter()
                    .map(|(number, hash)| (*number, *hash))
                    .collect::<Vec<_>>(),
                vec![(100, ancestor.hash), (101, replacement.hash)]
            );
            assert_eq!(state_after_recovery.commits.last().unwrap().1.len(), 1);
        }

        let restart = indexer.run_once().await.unwrap();
        assert_eq!(restart.blocks_committed, 0);
        assert_eq!(state.lock().unwrap().commits.len(), 2);
    }

    #[tokio::test]
    async fn deeper_checkpoint_reorg_walks_back_to_first_matching_block() {
        let config = config(100, 10);
        let ancestor = chain_block(100, 0xa0, 0x99);
        let local = [
            ancestor.clone(),
            chain_block(101, 0xb1, 0xa0),
            chain_block(102, 0xb2, 0xb1),
            chain_block(103, 0xb3, 0xb2),
        ];
        let canonical = [
            ancestor.clone(),
            chain_block(101, 0xc1, 0xa0),
            chain_block(102, 0xc2, 0xc1),
            chain_block(103, 0xc3, 0xc2),
        ];
        let source = source(
            103,
            canonical
                .iter()
                .cloned()
                .map(|block| (block.number, block))
                .collect(),
            vec![],
        );
        let store = FakeStore::default();
        seed_store(&store, &local);
        let state = Arc::clone(&store.state);

        let summary = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap();

        assert_eq!(
            (summary.first_block, summary.last_block),
            (Some(101), Some(103))
        );
        assert_eq!(summary.blocks_committed, 3);
        let state = state.lock().unwrap();
        assert_eq!(state.rollbacks[0].block_number, 100);
        assert_eq!(state.indexed_blocks.get(&100), Some(&ancestor.hash));
        assert_eq!(state.indexed_blocks.get(&101), Some(&canonical[1].hash));
        assert_eq!(state.indexed_blocks.get(&102), Some(&canonical[2].hash));
        assert_eq!(state.indexed_blocks.get(&103), Some(&canonical[3].hash));
    }

    #[tokio::test]
    async fn no_common_ancestor_fails_without_destructive_mutation() {
        let config = config(100, 10);
        let local = [chain_block(100, 0xa0, 0x99), chain_block(101, 0xb1, 0xa0)];
        let canonical = [chain_block(100, 0xc0, 0x98), chain_block(101, 0xc1, 0xc0)];
        let source = source(
            101,
            canonical
                .into_iter()
                .map(|block| (block.number, block))
                .collect(),
            vec![],
        );
        let store = FakeStore::default();
        seed_store(&store, &local);
        let state = Arc::clone(&store.state);

        let error = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            IndexerError::NoCommonAncestor {
                checkpoint_block: 101,
                deployment_block: 100,
            }
        ));
        let state = state.lock().unwrap();
        assert!(state.rollbacks.is_empty());
        assert_eq!(state.checkpoint.as_ref().unwrap().block_hash, local[1].hash);
        assert_eq!(state.indexed_blocks.len(), 2);
    }

    #[tokio::test]
    async fn checkpoint_below_deployment_fails_before_scanning_or_mutation() {
        let config = config(100, 10);
        let local = chain_block(50, 0xb0, 0xaf);
        let source = source(50, BTreeMap::new(), vec![]);
        let requested_blocks = Arc::clone(&source.requested_blocks);
        let store = FakeStore::default();
        seed_store(&store, std::slice::from_ref(&local));
        let state = Arc::clone(&store.state);

        let error = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            IndexerError::CheckpointBeforeDeployment {
                checkpoint_block: 50,
                deployment_block: 100,
            }
        ));
        assert!(requested_blocks.lock().unwrap().is_empty());
        let state = state.lock().unwrap();
        assert!(state.rollbacks.is_empty());
        assert_eq!(
            state.checkpoint,
            Some(Checkpoint {
                block_number: 50,
                block_hash: local.hash,
            })
        );
        assert_eq!(state.indexed_blocks, [(50, local.hash)].into());
        assert_eq!(state.commits.len(), 1);
    }

    #[tokio::test]
    async fn missing_stored_block_during_ancestor_search_is_explicit() {
        let config = config(100, 10);
        let local_100 = chain_block(100, 0xa0, 0x99);
        let local_102 = chain_block(102, 0xb2, 0xb1);
        let canonical = [
            local_100.clone(),
            chain_block(101, 0xc1, 0xa0),
            chain_block(102, 0xc2, 0xc1),
        ];
        let source = source(
            102,
            canonical
                .into_iter()
                .map(|block| (block.number, block))
                .collect(),
            vec![],
        );
        let store = FakeStore::default();
        seed_store(&store, &[local_100, local_102]);
        let state = Arc::clone(&store.state);

        let error = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            IndexerError::StoredBlockMissingDuringAncestorSearch { block_number: 101 }
        ));
        assert!(state.lock().unwrap().rollbacks.is_empty());
    }

    #[tokio::test]
    async fn missing_rpc_block_during_ancestor_search_is_explicit() {
        let config = config(100, 10);
        let local = [
            chain_block(100, 0xa0, 0x99),
            chain_block(101, 0xb1, 0xa0),
            chain_block(102, 0xb2, 0xb1),
        ];
        let source = source(102, [(102, chain_block(102, 0xc2, 0xc1))].into(), vec![]);
        let store = FakeStore::default();
        seed_store(&store, &local);
        let state = Arc::clone(&store.state);

        let error = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            IndexerError::RpcBlockMissingDuringAncestorSearch { block_number: 101 }
        ));
        assert!(state.lock().unwrap().rollbacks.is_empty());
    }

    #[tokio::test]
    async fn invalid_rpc_block_number_during_ancestor_search_is_explicit() {
        let config = config(100, 10);
        let local = [
            chain_block(100, 0xa0, 0x99),
            chain_block(101, 0xb1, 0xa0),
            chain_block(102, 0xb2, 0xb1),
        ];
        let invalid = chain_block(999, 0xc1, 0xa0);
        let source = source(
            102,
            [(101, invalid), (102, chain_block(102, 0xc2, 0xc1))].into(),
            vec![],
        );
        let store = FakeStore::default();
        seed_store(&store, &local);
        let state = Arc::clone(&store.state);

        let error = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            IndexerError::UnexpectedBlockNumber {
                expected: 101,
                actual: 999,
            }
        ));
        assert!(state.lock().unwrap().rollbacks.is_empty());
    }

    #[tokio::test]
    async fn repeated_reorg_in_one_run_hits_recovery_limit() {
        let config = config(100, 10);
        let ancestor = chain_block(100, 0xa0, 0x99);
        let inconsistent = chain_block(101, 0xc1, 0xee);
        let source = source(
            101,
            [(100, ancestor.clone()), (101, inconsistent)].into(),
            vec![],
        );
        let store = FakeStore::default();
        seed_store(&store, std::slice::from_ref(&ancestor));
        let state = Arc::clone(&store.state);

        let error = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            IndexerError::RecoveryLimitExceeded { block_number: 101 }
        ));
        assert_eq!(state.lock().unwrap().rollbacks.len(), 1);
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
    async fn parent_mismatch_during_catch_up_recovers_without_operator_restart() {
        let config = config(100, 10);
        let ancestor = chain_block(100, 0xa0, 0x99);
        let observed_before_reorg = chain_block(101, 0xb1, 0xa0);
        let canonical_101 = chain_block(101, 0xc1, 0xa0);
        let canonical_102 = chain_block(102, 0xc2, 0xc1);
        let source = scripted_source(
            102,
            [
                (100, vec![ancestor.clone()]),
                (
                    101,
                    vec![observed_before_reorg.clone(), canonical_101.clone()],
                ),
                (102, vec![canonical_102.clone()]),
            ]
            .into(),
            vec![],
        );
        let store = FakeStore::default();
        seed_store(&store, std::slice::from_ref(&ancestor));
        let state = Arc::clone(&store.state);

        let summary = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap();

        assert_eq!(
            (summary.first_block, summary.last_block),
            (Some(101), Some(102))
        );
        assert_eq!(summary.blocks_committed, 2);
        let state = state.lock().unwrap();
        assert_eq!(state.rollbacks[0].block_number, 100);
        assert_eq!(state.indexed_blocks.get(&100), Some(&ancestor.hash));
        assert_eq!(state.indexed_blocks.get(&101), Some(&canonical_101.hash));
        assert_eq!(state.indexed_blocks.get(&102), Some(&canonical_102.hash));
        assert!(
            state
                .commits
                .iter()
                .all(|(block, _)| block.hash != observed_before_reorg.hash)
        );
    }

    #[tokio::test]
    async fn log_block_hash_mismatch_recovers_with_canonical_logs_on_retry() {
        let config = config(100, 10);
        let ancestor = chain_block(100, 0xa0, 0x99);
        let orphaned_101 = chain_block(101, 0xb1, 0xa0);
        let canonical_101 = chain_block(101, 0xc1, 0xa0);
        let orphaned_log = market_log(&orphaned_101, config.contract_address);
        let canonical_log = market_log(&canonical_101, config.contract_address);
        let source = scripted_source_with_logs(
            101,
            [
                (100, vec![ancestor.clone()]),
                (101, vec![canonical_101.clone()]),
            ]
            .into(),
            vec![vec![orphaned_log], vec![canonical_log]],
        );
        let requested_ranges = Arc::clone(&source.requested_ranges);
        let store = FakeStore::default();
        seed_store(&store, std::slice::from_ref(&ancestor));
        let state = Arc::clone(&store.state);
        let indexer = Indexer::new(source, store, config);

        let summary = indexer.run_once().await.unwrap();

        assert_eq!((summary.blocks_committed, summary.events_committed), (1, 1));
        assert_eq!(requested_ranges.lock().unwrap().len(), 4);
        {
            let state = state.lock().unwrap();
            assert_eq!(state.rollbacks.len(), 1);
            assert_eq!(state.rollbacks[0].block_number, 100);
            assert_eq!(state.commits.len(), 2);
            assert_eq!(state.commits[1].0.hash, canonical_101.hash);
            assert_eq!(state.commits[1].1.len(), 1);
            assert_eq!(state.commits[1].1[0].log.block_hash, canonical_101.hash);
            assert_eq!(
                state.commits.iter().flat_map(|(_, events)| events).count(),
                1
            );
        }

        let restart = indexer.run_once().await.unwrap();
        assert_eq!((restart.blocks_committed, restart.events_committed), (0, 0));
        assert_eq!(state.lock().unwrap().commits.len(), 2);
    }

    #[tokio::test]
    async fn repeated_log_block_hash_mismatch_hits_recovery_limit() {
        let config = config(100, 10);
        let ancestor = chain_block(100, 0xa0, 0x99);
        let orphaned_101 = chain_block(101, 0xb1, 0xa0);
        let canonical_101 = chain_block(101, 0xc1, 0xa0);
        let orphaned_log = market_log(&orphaned_101, config.contract_address);
        let source = scripted_source_with_logs(
            101,
            [(100, vec![ancestor.clone()]), (101, vec![canonical_101])].into(),
            vec![vec![orphaned_log.clone()], vec![orphaned_log]],
        );
        let requested_ranges = Arc::clone(&source.requested_ranges);
        let store = FakeStore::default();
        seed_store(&store, std::slice::from_ref(&ancestor));
        let state = Arc::clone(&store.state);

        let error = Indexer::new(source, store, config)
            .run_once()
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            IndexerError::RecoveryLimitExceeded { block_number: 101 }
        ));
        assert_eq!(requested_ranges.lock().unwrap().len(), 4);
        let state = state.lock().unwrap();
        assert_eq!(state.rollbacks.len(), 1);
        assert_eq!(state.commits.len(), 1);
        assert!(state.commits[0].1.is_empty());
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
