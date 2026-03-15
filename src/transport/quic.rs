use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use quinn::{Endpoint, RecvStream, SendStream, ServerConfig, TransportConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use thiserror::Error;

const MAX_FRAME_SIZE: u32 = 1_048_576;

#[derive(Debug, Error)]
pub enum QuicError {
    #[error("tls configuration error: {0}")]
    Tls(#[from] rustls::Error),

    #[error("endpoint bind error: {0}")]
    Io(#[from] std::io::Error),

    #[error("quic configuration error: {0}")]
    Config(String),

    #[error("frame too large: {size} bytes (max {max})")]
    FrameTooLarge { size: u32, max: u32 },

    #[error("frame data exceeds protocol limit")]
    FrameOverflow,

    #[error("stream read error: {0}")]
    Read(#[from] quinn::ReadExactError),

    #[error("stream write error: {0}")]
    Write(#[from] quinn::WriteError),
}

pub fn build_endpoint(
    addr: SocketAddr,
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    idle_timeout: Duration,
) -> Result<Endpoint, QuicError> {
    let mut crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    crypto.alpn_protocols = vec![b"$hatter$/1".to_vec()];
    crypto.max_early_data_size = 0;

    let mut transport = TransportConfig::default();
    transport.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(idle_timeout)
            .map_err(|e| QuicError::Config(e.to_string()))?,
    ));
    transport.keep_alive_interval(Some(idle_timeout / 3));

    let quic_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(crypto)
        .map_err(|e| QuicError::Config(e.to_string()))?;

    let mut config = ServerConfig::with_crypto(Arc::new(quic_crypto));
    config.transport_config(Arc::new(transport));

    Ok(Endpoint::server(config, addr)?)
}

pub async fn read_frame(recv: &mut RecvStream) -> Result<Vec<u8>, QuicError> {
    let mut header = [0u8; 4];
    recv.read_exact(&mut header).await?;

    let len = u32::from_be_bytes(header);
    if len > MAX_FRAME_SIZE {
        return Err(QuicError::FrameTooLarge {
            size: len,
            max: MAX_FRAME_SIZE,
        });
    }

    let mut payload = vec![0u8; len as usize];
    recv.read_exact(&mut payload).await?;

    Ok(payload)
}

pub async fn write_frame(send: &mut SendStream, data: &[u8]) -> Result<(), QuicError> {
    let len = u32::try_from(data.len()).map_err(|_| QuicError::FrameOverflow)?;
    send.write_all(&len.to_be_bytes()).await?;
    send.write_all(data).await?;

    Ok(())
}
