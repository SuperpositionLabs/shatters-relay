use std::path::Path;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TlsError {
    #[error("tls i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("no private key found in pem file")]
    NoPrivateKey,

    #[error("tls certificate files required but not found")]
    CertRequired,

    #[error("certificate generation failed: {0}")]
    CertGen(#[from] rcgen::Error),
}

pub fn load_credentials(
    cert_path: &Path,
    key_path: &Path,
    allow_self_signed: bool,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), TlsError> {
    if !cert_path.exists() || !key_path.exists() {
        if !allow_self_signed {
            return Err(TlsError::CertRequired);
        }
        tracing::warn!(
            cert = ?cert_path,
            key = ?key_path,
            "tls files not found, generating self-signed certificate (dev mode)"
        );
        return generate_self_signed();
    }

    let cert_pem = std::fs::read(cert_path)?;
    let key_pem = std::fs::read(key_path)?;

    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut &cert_pem[..]).collect::<Result<Vec<_>, _>>()?;

    let key = rustls_pemfile::private_key(&mut &key_pem[..])?.ok_or(TlsError::NoPrivateKey)?;

    Ok((certs, key))
}

fn generate_self_signed() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), TlsError>
{
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    let key_der = PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der());

    Ok((vec![certified.cert.into()], PrivateKeyDer::Pkcs8(key_der)))
}
