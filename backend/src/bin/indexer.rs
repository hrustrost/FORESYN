use std::{future::Future, time::Duration};

use async_trait::async_trait;
use foresyn_backend::{
    chain::{AlloyChainSource, ChainSource},
    config::IndexerConfig,
    db::Database,
    indexer::{BlockStore, Indexer, IndexerError, RunSummary},
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
struct RunOptions {
    full_reindex: bool,
    watch: bool,
    poll_interval: Duration,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            full_reindex: false,
            watch: false,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();
    let options = parse_options()?;

    let config = IndexerConfig::from_env()?;
    let source = AlloyChainSource::new(config.rpc_url.clone());
    let configured_chain_id = config.chain_id;
    run_after_chain_preflight(source, configured_chain_id, |source| async move {
        let database = Database::connect(&config.database_url).await?;
        database.migrate().await?;
        if options.full_reindex {
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

        let indexer = Indexer::new(source, database, config);
        run_indexer(&indexer, &options, shutdown_signal()).await
    })
    .await?;

    Ok(())
}

#[async_trait]
trait CatchUpRunner: Send + Sync {
    async fn run_once(&self) -> Result<RunSummary, IndexerError>;
}

#[async_trait]
impl<S, D> CatchUpRunner for Indexer<S, D>
where
    S: ChainSource,
    D: BlockStore,
{
    async fn run_once(&self) -> Result<RunSummary, IndexerError> {
        Indexer::run_once(self).await
    }
}

async fn run_indexer<R, F>(
    runner: &R,
    options: &RunOptions,
    shutdown: F,
) -> Result<RunSummary, IndexerError>
where
    R: CatchUpRunner,
    F: Future<Output = ()>,
{
    tokio::pin!(shutdown);

    loop {
        let summary = runner.run_once().await?;
        log_summary(&summary, options.watch);

        if !options.watch {
            return Ok(summary);
        }

        tokio::select! {
            biased;
            () = &mut shutdown => {
                info!("indexer watch shutdown requested");
                return Ok(summary);
            }
            () = tokio::time::sleep(options.poll_interval) => {}
        }
    }
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(error = %error, "failed to install Ctrl+C handler; stopping watch mode");
    }
}

fn log_summary(summary: &RunSummary, watch: bool) {
    info!(
        latest_block = summary.latest_block,
        safe_head = summary.safe_head,
        first_block = summary.first_block,
        last_block = summary.last_block,
        blocks_committed = summary.blocks_committed,
        events_committed = summary.events_committed,
        watch,
        "indexer run finished"
    );
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

fn parse_options() -> Result<RunOptions, Box<dyn std::error::Error>> {
    parse_options_from(std::env::args().skip(1))
}

fn parse_options_from<I, S>(arguments: I) -> Result<RunOptions, Box<dyn std::error::Error>>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut options = RunOptions::default();
    let mut arguments = arguments.into_iter().map(Into::into);
    let mut custom_poll_interval = false;

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--full-reindex" if !options.full_reindex => options.full_reindex = true,
            "--watch" if !options.watch => options.watch = true,
            "--poll-interval-ms" if !custom_poll_interval => {
                let value = arguments.next().ok_or_else(cli_usage_error)?;
                let milliseconds = value.parse::<u64>().map_err(|_| cli_usage_error())?;
                if milliseconds == 0 {
                    return Err(cli_usage_error());
                }
                options.poll_interval = Duration::from_millis(milliseconds);
                custom_poll_interval = true;
            }
            _ => return Err(cli_usage_error()),
        }
    }

    if custom_poll_interval && !options.watch {
        return Err(cli_usage_error());
    }

    Ok(options)
}

fn cli_usage_error() -> Box<dyn std::error::Error> {
    "usage: indexer [--full-reindex] [--watch [--poll-interval-ms <positive integer>]]".into()
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("foresyn_backend=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[cfg(test)]
mod tests {
    use std::{
        future::pending,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use alloy::primitives::{Address, B256};
    use async_trait::async_trait;
    use foresyn_backend::{
        chain::{ChainBlock, ChainLog, ChainSource, RpcError},
        indexer::{IndexerError, RunSummary},
    };
    use tokio::sync::oneshot;

    use super::{
        CatchUpRunner, RunOptions, parse_options_from, run_after_chain_preflight, run_indexer,
    };

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
    struct FixedChainSource {
        chain_id: u64,
    }

    #[async_trait]
    impl ChainSource for FixedChainSource {
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

    #[derive(Clone, Default)]
    struct FakeRunner {
        runs: Arc<AtomicUsize>,
        stop_after: Option<usize>,
        stop_sender: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    }

    impl FakeRunner {
        fn stopping_after(runs: usize, sender: oneshot::Sender<()>) -> Self {
            Self {
                runs: Arc::new(AtomicUsize::new(0)),
                stop_after: Some(runs),
                stop_sender: Arc::new(Mutex::new(Some(sender))),
            }
        }

        fn signal_stop(&self) {
            if let Some(sender) = self.stop_sender.lock().unwrap().take() {
                let _ = sender.send(());
            }
        }
    }

    #[async_trait]
    impl CatchUpRunner for FakeRunner {
        async fn run_once(&self) -> Result<RunSummary, IndexerError> {
            let run = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
            if self.stop_after == Some(run) {
                self.signal_stop();
            }
            Ok(RunSummary {
                latest_block: run as u64,
                ..RunSummary::default()
            })
        }
    }

    #[tokio::test]
    async fn one_shot_is_the_default_and_runs_exactly_once() {
        let options = parse_options_from(std::iter::empty::<String>()).unwrap();
        assert_eq!(options, RunOptions::default());

        let runner = FakeRunner::default();
        let summary = run_indexer(&runner, &options, pending()).await.unwrap();

        assert_eq!(runner.runs.load(Ordering::SeqCst), 1);
        assert_eq!(summary.latest_block, 1);
    }

    #[tokio::test]
    async fn watch_repeats_the_same_normal_catch_up_runner_until_shutdown() {
        let (stop_sender, stop_receiver) = oneshot::channel();
        let runner = FakeRunner::stopping_after(3, stop_sender);
        let options = RunOptions {
            watch: true,
            poll_interval: Duration::ZERO,
            ..RunOptions::default()
        };

        let summary = run_indexer(&runner, &options, async {
            let _ = stop_receiver.await;
        })
        .await
        .unwrap();

        assert_eq!(runner.runs.load(Ordering::SeqCst), 3);
        assert_eq!(summary.latest_block, 3);
    }

    #[tokio::test]
    async fn full_reindex_startup_action_is_not_repeated_by_watch_iterations() {
        let startup_actions = Arc::new(AtomicUsize::new(0));
        let startup_actions_in_run = Arc::clone(&startup_actions);
        let (stop_sender, stop_receiver) = oneshot::channel();
        let runner = FakeRunner::stopping_after(3, stop_sender);
        let runs = Arc::clone(&runner.runs);
        let options = RunOptions {
            full_reindex: true,
            watch: true,
            poll_interval: Duration::ZERO,
        };

        run_after_chain_preflight(
            FixedChainSource { chain_id: 31_337 },
            31_337,
            move |_| async move {
                if options.full_reindex {
                    startup_actions_in_run.fetch_add(1, Ordering::SeqCst);
                }
                run_indexer(&runner, &options, async {
                    let _ = stop_receiver.await;
                })
                .await
            },
        )
        .await
        .unwrap();

        assert_eq!(startup_actions.load(Ordering::SeqCst), 1);
        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn watch_cli_accepts_an_explicit_positive_poll_interval() {
        let options =
            parse_options_from(["--full-reindex", "--watch", "--poll-interval-ms", "250"]).unwrap();
        assert!(options.full_reindex);
        assert!(options.watch);
        assert_eq!(options.poll_interval, Duration::from_millis(250));

        assert!(parse_options_from(["--poll-interval-ms", "250"]).is_err());
        assert!(parse_options_from(["--watch", "--poll-interval-ms", "0"]).is_err());
    }

    #[tokio::test]
    async fn full_reindex_wrong_rpc_chain_never_connects_or_mutates_database() {
        let state = Arc::new(Mutex::new(FakeDatabaseState::seeded()));
        let before = state.lock().unwrap().clone();
        let database_connected = Arc::new(AtomicBool::new(false));
        let action_state = Arc::clone(&state);
        let action_connected = Arc::clone(&database_connected);

        let error = run_after_chain_preflight(
            FixedChainSource { chain_id: 2 },
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
            FixedChainSource { chain_id: 31_338 },
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
