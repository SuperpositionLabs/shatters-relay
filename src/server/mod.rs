mod connection;

use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

use crate::config::{LimitsConfig, RelayConfig};
use crate::deaddrop::DeadDropStore;
use crate::prekey::PreKeyStore;
use crate::router::Router;
use crate::transport;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("invalid listen address: {0}")]
    InvalidListenAddr(#[from] std::net::AddrParseError),

    #[error("tls error: {0}")]
    Tls(#[from] transport::tls::TlsError),

    #[error("quic error: {0}")]
    Quic(#[from] transport::quic::QuicError),
}

pub async fn run(config: RelayConfig) -> Result<(), ServerError> {
    let (certs, key) = transport::tls::load_credentials(
        &config.server.tls_cert_path,
        &config.server.tls_key_path,
    )?;

    let itimeout = Duration::from_secs(config.server.max_idle_timeout_secs);
    let endpoint = transport::quic::build_endpoint(
        config.server.listen_addr.parse()?,
        certs,
        key,
        itimeout,
    )?;

    let router = Router::with_limits(config.limits.max_subscriptions_per_conn);

    let store = DeadDropStore::new(config.deaddrop);
    store.spawn_cleanup();

    let prekeys = PreKeyStore::new(config.prekey);
    prekeys.spawn_cleanup();

    tracing::info!(addr = %config.server.listen_addr, "relay listening");

    accept_loop(endpoint, router, store, prekeys, config.limits).await;

    Ok(())
}

async fn accept_loop(
    endpoint: quinn::Endpoint,
    router:   Arc<Router>,
    store:    Arc<DeadDropStore>,
    prekeys:  Arc<PreKeyStore>,
    limits:   LimitsConfig,
) {
    while let Some(incoming) = endpoint.accept().await {
        let remote = incoming.remote_address();
        let router  = Arc::clone(&router);
        let store   = Arc::clone(&store);
        let prekeys = Arc::clone(&prekeys);
        let limits  = limits.clone();

        tokio::spawn(async move {
            match incoming.await {
                Ok(conn) => {
                    tracing::info!(remote = %remote, "connection established");

                    if let Err(e) = connection::handle(conn, router, store, prekeys, limits).await {
                        tracing::error!(remote = %remote, error = %e, "connection error");
                    }
                }
                Err(e) => {
                    tracing::error!(remote = %remote, error = %e, "connection failed");
                }
            }
        });
    }
}
