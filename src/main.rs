mod cli;
mod config;
mod pihole;

use anyhow::Result;
use cli::Cli;
use tracing_subscriber::EnvFilter;

fn setup_logging() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();
}

#[tokio::main]
async fn main() -> Result<()> {
    setup_logging();

    Cli::parse_args().await?;

    Ok(())
}
