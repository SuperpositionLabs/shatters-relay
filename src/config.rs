use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config file not found: {}", .0.display())]
    NotFound(PathBuf),

    #[error("failed to read config file: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid config: {0}")]
    Parse(#[from] toml::de::Error),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RelayConfig {
    pub server: ServerConfig,
    pub deaddrop: DeadDropConfig,
    pub prekey: PreKeyConfig,
    pub limits: LimitsConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub listen_addr: String,
    
    pub tls_cert_path: PathBuf,
    pub tls_key_path: PathBuf,
    pub allow_self_signed_tls: bool,
    
    pub max_auth_timeout_secs: u64,
    pub max_idle_timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeadDropConfig {
    pub default_ttl_secs: u64,
    pub max_per_drop: usize,
    pub max_channels: usize,
    pub cleanup_interval_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: String,
    pub format: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:4433".into(),
            
            tls_cert_path: "cert.pem".into(),
            tls_key_path:  "key.pem".into(),
            allow_self_signed_tls: false,
            
            max_auth_timeout_secs: 10,
            max_idle_timeout_secs: 30,
        }
    }
}

impl Default for DeadDropConfig {
    fn default() -> Self {
        Self {
            default_ttl_secs: 86_400,
            max_per_drop: 1_000,
            max_channels: 100_000,
            cleanup_interval_secs: 60,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self { level: "info".into(), format: "pretty".into() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PreKeyConfig {
    pub bundle_ttl_secs: u64,
    pub max_prekeys_per_user: usize,
    pub max_bundle_size_bytes: usize,
    pub cleanup_interval_secs: u64,
}

impl Default for PreKeyConfig {
    fn default() -> Self {
        Self {
            bundle_ttl_secs: 604_800,
            max_prekeys_per_user: 100,
            max_bundle_size_bytes: 4096,
            cleanup_interval_secs: 300,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LimitsConfig {
    pub max_subscriptions_per_conn: usize,
    pub max_connections: usize,
    pub max_total_channels: usize,
    pub requests_per_second: u32,
    pub burst_size: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_subscriptions_per_conn: 50,
            max_connections: 10_000,
            max_total_channels: 100_000,
            requests_per_second: 100,
            burst_size: 200,
        }
    }
}

impl RelayConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Err(ConfigError::NotFound(path.to_path_buf()));
        }

        let contents = std::fs::read_to_string(path)?;
        let config: RelayConfig = toml::from_str(&contents)?;
        Ok(config)
    }
}
