mod config;
mod deaddrop;
mod prekey;
mod protocol;
mod ratelimit;
mod router;
mod server;
mod transport;

use std::path::PathBuf;

use clap::Parser;
use thiserror::Error;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[derive(Debug, Error)]
enum RelayError {
    #[error("{0}")]
    Config(#[from] config::ConfigError),

    #[error("{0}")]
    Server(#[from] server::ServerError),
}

#[derive(Parser)]
#[command(name = "shatters-relay")]
struct Args {
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), RelayError> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let args = Args::parse();

    let config_path = args.config.canonicalize().unwrap_or(args.config);
    let config: config::RelayConfig = config::RelayConfig::load(&config_path)?;

    init_logging(&config.logging);

    tracing::info!(
        config = %config_path.display(),
        "starting shatters relay"
    );

    server::run(config).await?;

    Ok(())
}

fn init_logging(logging: &config::LoggingConfig) {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&logging.level));

    match logging.format.as_str() {
        "json" => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt::layer().json().flatten_event(true))
                .init();
        }
        _ => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt::layer().pretty())
                .init();
        }
    }
}
