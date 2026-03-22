mod connection;

use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
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
        config.limits.slow_consumer_drop_threshold,
    );

    let store = DeadDropStore::new(config.deaddrop);
    store.spawn_cleanup();

    let prekeys = PreKeyStore::new(config.prekey);
    prekeys.spawn_cleanup();

    let conn_semaphore = Arc::new(Semaphore::new(config.limits.max_connections));
    let ip_counts: Arc<DashMap<IpAddr, AtomicUsize>> = Arc::new(DashMap::new());

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
        config.shutdown.drain_timeout_secs,
        ip_counts,
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
    drain_timeout:   u64,
    ip_counts:       Arc<DashMap<IpAddr, AtomicUsize>>,
) {
    let max_per_ip = limits.max_connections_per_ip;
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
                        tracing::warn!("connection limit reached, refusing");
                        incoming.refuse();
                        continue;
                    }
                };

                let remote  = incoming.remote_address();
                let ip = remote.ip();

                let current = ip_counts
                    .entry(ip)
                    .or_insert_with(|| AtomicUsize::new(0))
                    .fetch_add(1, Ordering::Relaxed);
                if current >= max_per_ip {
                    ip_counts.get(&ip).map(|c| c.fetch_sub(1, Ordering::Relaxed));
                    tracing::warn!("per-ip connection limit reached, refusing");
                    incoming.refuse();
                    continue;
                }

                let router  = Arc::clone(&router);
                let store   = Arc::clone(&store);
                let prekeys = Arc::clone(&prekeys);
                let limits  = limits.clone();
                let ip_counts_inner = Arc::clone(&ip_counts);

                tokio::spawn(async move {
                    let _permit = permit;
                    match incoming.await {
                        Ok(conn) => {
                            tracing::info!("connection established");

                            if let Err(e) = connection::handle(
                                conn, router, store, prekeys, limits, auth_timeout,
                            ).await {
                                tracing::error!(error = %e, "connection error");
                            }
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "connection failed");
                        }
                    }
                    ip_counts_inner.get(&ip).map(|c| c.fetch_sub(1, Ordering::Relaxed));
                });
            }
            _ = &mut shutdown => {
                tracing::info!("shutdown signal received, draining connections");
                break;
            }
        }
    }

    endpoint.close(quinn::VarInt::from_u32(0), b"shutdown");
    tracing::info!("waiting for connections to close (timeout {}s)", drain_timeout);
    if drain_timeout > 0 {
        let _ = tokio::time::timeout(
            Duration::from_secs(drain_timeout),
            endpoint.wait_idle(),
        )
        .await;
    } else {
        endpoint.wait_idle().await;
    }
    tracing::info!("relay shut down");
}
