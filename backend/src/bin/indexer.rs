use std::future::Future;

use foresyn_backend::{
    chain::{AlloyChainSource, ChainSource},
    config::IndexerConfig,
    db::Database,
    indexer::{Indexer, IndexerError},
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();
    let full_reindex = parse_full_reindex_flag()?;

    let config = IndexerConfig::from_env()?;
    let source = AlloyChainSource::new(config.rpc_url.clone());
    let configured_chain_id = config.chain_id;
    let summary = run_after_chain_preflight(source, configured_chain_id, |source| async move {
        let database = Database::connect(&config.database_url).await?;
        database.migrate().await?;
        if full_reindex {
            warn!(
                chain_id = config.chain_id,
                contract_address = %config.contract_address,
                deployment_block = config.deployment_block,
                "explicit full reindex requested; clearing configured chain index state"
            );
            database
                .full_reindex(
                    config.chain_id,
                    config.contract_address,
                    config.deployment_block,
                )
                .await?;
        } else {
            database
                .ensure_position_coverage(
                    config.chain_id,
                    config.contract_address,
                    config.deployment_block,
                )
                .await?;
        }

        Indexer::new(source, database, config).run_once().await
    })
    .await?;

    info!(
        latest_block = summary.latest_block,
        safe_head = summary.safe_head,
        first_block = summary.first_block,
        last_block = summary.last_block,
        blocks_committed = summary.blocks_committed,
        events_committed = summary.events_committed,
        "indexer run finished"
    );

    Ok(())
}

async fn run_after_chain_preflight<S, F, Fut, T>(
    source: S,
    configured_chain_id: u64,
    after_validation: F,
) -> Result<T, IndexerError>
where
    S: ChainSource,
    F: FnOnce(S) -> Fut,
    Fut: Future<Output = Result<T, IndexerError>>,
{
    let actual_chain_id = source.chain_id().await?;
    if actual_chain_id != configured_chain_id {
        return Err(IndexerError::ChainIdMismatch {
            configured: configured_chain_id,
            actual: actual_chain_id,
        });
    }

    after_validation(source).await
}

fn parse_full_reindex_flag() -> Result<bool, Box<dyn std::error::Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(false),
        [argument] if argument == "--full-reindex" => Ok(true),
        _ => Err("usage: indexer [--full-reindex]".into()),
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("foresyn_backend=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use alloy::primitives::{Address, B256};
    use async_trait::async_trait;
    use foresyn_backend::{
        chain::{ChainBlock, ChainLog, ChainSource, RpcError},
        indexer::IndexerError,
    };

    use super::run_after_chain_preflight;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeDatabaseState {
        indexed_blocks: usize,
        raw_events: usize,
        markets: usize,
        market_states: usize,
        market_positions: usize,
        checkpoint_block: Option<u64>,
        coverage_markers: usize,
    }

    impl FakeDatabaseState {
        const fn seeded() -> Self {
            Self {
                indexed_blocks: 4,
                raw_events: 7,
                markets: 2,
                market_states: 2,
                market_positions: 3,
                checkpoint_block: Some(103),
                coverage_markers: 0,
            }
        }
    }

    #[derive(Clone, Debug)]
    struct WrongChainSource {
        chain_id: u64,
    }

    #[async_trait]
    impl ChainSource for WrongChainSource {
        async fn chain_id(&self) -> Result<u64, RpcError> {
            Ok(self.chain_id)
        }

        async fn latest_block_number(&self) -> Result<u64, RpcError> {
            unreachable!("startup must stop after the chain-id mismatch")
        }

        async fn block_by_number(&self, _number: u64) -> Result<ChainBlock, RpcError> {
            unreachable!("startup must stop after the chain-id mismatch")
        }

        async fn logs(
            &self,
            _from_block: u64,
            _to_block: u64,
            _address: Address,
            _topic0: B256,
        ) -> Result<Vec<ChainLog>, RpcError> {
            unreachable!("startup must stop after the chain-id mismatch")
        }
    }

    #[tokio::test]
    async fn full_reindex_wrong_rpc_chain_never_connects_or_mutates_database() {
        let state = Arc::new(Mutex::new(FakeDatabaseState::seeded()));
        let before = state.lock().unwrap().clone();
        let database_connected = Arc::new(AtomicBool::new(false));
        let action_state = Arc::clone(&state);
        let action_connected = Arc::clone(&database_connected);

        let error = run_after_chain_preflight(
            WrongChainSource { chain_id: 2 },
            1,
            move |_source| async move {
                action_connected.store(true, Ordering::SeqCst);
                let mut state = action_state.lock().unwrap();
                state.indexed_blocks = 0;
                state.raw_events = 0;
                state.markets = 0;
                state.market_states = 0;
                state.market_positions = 0;
                state.checkpoint_block = None;
                state.coverage_markers = 1;
                Ok::<(), IndexerError>(())
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            IndexerError::ChainIdMismatch {
                configured: 1,
                actual: 2
            }
        ));
        assert!(!database_connected.load(Ordering::SeqCst));
        assert_eq!(*state.lock().unwrap(), before);
        assert_eq!(state.lock().unwrap().coverage_markers, 0);
    }

    #[tokio::test]
    async fn normal_startup_wrong_rpc_chain_never_writes_coverage_or_index_state() {
        let state = Arc::new(Mutex::new(FakeDatabaseState::seeded()));
        let before = state.lock().unwrap().clone();
        let database_connected = Arc::new(AtomicBool::new(false));
        let action_state = Arc::clone(&state);
        let action_connected = Arc::clone(&database_connected);

        let error = run_after_chain_preflight(
            WrongChainSource { chain_id: 31_338 },
            31_337,
            move |_source| async move {
                action_connected.store(true, Ordering::SeqCst);
                let mut state = action_state.lock().unwrap();
                state.coverage_markers = 1;
                state.checkpoint_block = Some(104);
                Ok::<(), IndexerError>(())
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            IndexerError::ChainIdMismatch {
                configured: 31_337,
                actual: 31_338
            }
        ));
        assert!(!database_connected.load(Ordering::SeqCst));
        assert_eq!(*state.lock().unwrap(), before);
        assert_eq!(state.lock().unwrap().coverage_markers, 0);
    }
}
