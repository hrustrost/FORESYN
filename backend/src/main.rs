use foresyn_backend::{
    api::{ApiState, router},
    config::ApiConfig,
    read_repository::PostgresMarketReader,
};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let config = ApiConfig::from_env()?;
    let reader = PostgresMarketReader::connect(
        &config.database_url,
        config.chain_id,
        config.contract_address,
    )
    .await?;
    let application = router(
        ApiState::new(std::sync::Arc::new(reader)),
        &config.cors_origin,
    )?;
    let listener = TcpListener::bind(config.bind_address).await?;

    info!(
        address = %config.bind_address,
        chain_id = config.chain_id,
        contract_address = %config.contract_address,
        "backend listening"
    );

    axum::serve(listener, application)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("foresyn_backend=info"));

    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to install shutdown signal handler");
    }
}
