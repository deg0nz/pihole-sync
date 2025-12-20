use anyhow::Result;
use pihole_sync::cli::Cli;
use tracing::info;
use tracing_subscriber::EnvFilter;

fn setup_logging() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .try_init();
}

#[tokio::main]
async fn main() -> Result<()> {
    setup_logging();

    info!("Starting pihole-sync v{}", env!("CARGO_PKG_VERSION"));

    Cli::parse_args().await?;

    Ok(())
}
