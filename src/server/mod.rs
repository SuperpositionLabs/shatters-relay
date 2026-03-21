mod connection;

use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::sync::Semaphore;

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
        config.server.allow_self_signed_tls,
    )?;

    let itimeout = Duration::from_secs(config.server.max_idle_timeout_secs);
    let endpoint = transport::quic::build_endpoint(
        config.server.listen_addr.parse()?,
        certs,
        key,
        itimeout,
    )?;

    let router = Router::with_limits(
        config.limits.max_subscriptions_per_conn,
        config.limits.max_total_channels,
    );

    let store = DeadDropStore::new(config.deaddrop);
    store.spawn_cleanup();

    let prekeys = PreKeyStore::new(config.prekey);
    prekeys.spawn_cleanup();

    let conn_semaphore = Arc::new(Semaphore::new(config.limits.max_connections));

    tracing::info!(
        addr = %config.server.listen_addr,
        max_connections = config.limits.max_connections,
        "relay listening"
    );

    accept_loop(
        endpoint,
        router,
        store,
        prekeys,
        config.limits,
        config.server.max_auth_timeout_secs,
        conn_semaphore,
    )
    .await;

    Ok(())
}

async fn accept_loop(
    endpoint:        quinn::Endpoint,
    router:          Arc<Router>,
    store:           Arc<DeadDropStore>,
    prekeys:         Arc<PreKeyStore>,
    limits:          LimitsConfig,
    auth_timeout:    u64,
    conn_semaphore:  Arc<Semaphore>,
) {
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let incoming = match incoming {
                    Some(i) => i,
                    None    => break,
                };

                let permit = match conn_semaphore.clone().try_acquire_owned() {
                    Ok(p) => p,
                    Err(_) => {
                        tracing::warn!(
                            remote = %incoming.remote_address(),
                            "connection limit reached, refusing"
                        );
                        incoming.refuse();
                        continue;
                    }
                };

                let remote  = incoming.remote_address();
                let router  = Arc::clone(&router);
                let store   = Arc::clone(&store);
                let prekeys = Arc::clone(&prekeys);
                let limits  = limits.clone();

                tokio::spawn(async move {
                    let _permit = permit;
                    match incoming.await {
                        Ok(conn) => {
                            tracing::info!(remote = %remote, "connection established");

                            if let Err(e) = connection::handle(
                                conn, router, store, prekeys, limits, auth_timeout,
                            ).await {
                                tracing::error!(remote = %remote, error = %e, "connection error");
                            }
                        }
                        Err(e) => {
                            tracing::error!(remote = %remote, error = %e, "connection failed");
                        }
                    }
                });
            }
            _ = &mut shutdown => {
                tracing::info!("shutdown signal received, draining connections");
                break;
            }
        }
    }

    endpoint.close(quinn::VarInt::from_u32(0), b"shutdown");
    tracing::info!("waiting for connections to close");
    endpoint.wait_idle().await;
    tracing::info!("relay shut down");
}
