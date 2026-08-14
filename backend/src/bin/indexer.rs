use foresyn_backend::{
    chain::AlloyChainSource, config::IndexerConfig, db::Database, indexer::Indexer,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let config = IndexerConfig::from_env()?;
    let database = Database::connect(&config.database_url).await?;
    database.migrate().await?;

    let source = AlloyChainSource::new(config.rpc_url.clone());
    let summary = Indexer::new(source, database, config).run_once().await?;

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

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("foresyn_backend=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
