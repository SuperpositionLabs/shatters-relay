use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::config::PreKeyConfig;
use crate::protocol::message::Channel;

const OPK_COUNT_OFFSET: usize = 128;
const OPK_START: usize = 130;
const OPK_ENTRY_SIZE: usize = 4 + 32; // id(4) + pubkey(32)
const MIN_BUNDLE_SIZE: usize = OPK_START;

fn opk_count(bundle: &[u8]) -> usize {
    if bundle.len() < OPK_START {
        return 0;
    }
    u16::from_be_bytes([
        bundle[OPK_COUNT_OFFSET],
        bundle[OPK_COUNT_OFFSET + 1],
    ]) as usize
}

fn consume_one_opk(bundle: &[u8]) -> Vec<u8> {
    let count = opk_count(bundle);
    if count == 0 {
        return bundle.to_vec();
    }

    let new_count = count - 1;
    let mut out = Vec::with_capacity(OPK_START + new_count * OPK_ENTRY_SIZE);
    out.extend_from_slice(&bundle[..OPK_COUNT_OFFSET]);
    out.push((new_count >> 8) as u8);
    out.push((new_count & 0xFF) as u8);

    let remaining_start = OPK_START + OPK_ENTRY_SIZE;
    if remaining_start <= bundle.len() {
        out.extend_from_slice(&bundle[remaining_start..]);
    }
    out
}

type ChannelKey = Channel;

struct StoredBundle {
    data:      Vec<u8>,
    stored_at: Instant,
}

pub struct PreKeyStore {
    bundles: DashMap<ChannelKey, StoredBundle>,
    config:  PreKeyConfig,
}

impl PreKeyStore {
    pub fn new(config: PreKeyConfig) -> Arc<Self> {
        Arc::new(Self {
            bundles: DashMap::new(),
            config,
        })
    }

    pub fn upload(&self, channel: Channel, bundle_data: Vec<u8>) -> Result<(), &'static str> {
        if bundle_data.len() < MIN_BUNDLE_SIZE {
            return Err("bundle too small");
        }
        if bundle_data.len() > self.config.max_bundle_size_bytes {
            return Err("bundle too large");
        }

        let count = opk_count(&bundle_data);
        if count > self.config.max_prekeys_per_user {
            return Err("too many one-time prekeys");
        }

        let expected_len = OPK_START + count * OPK_ENTRY_SIZE;
        if bundle_data.len() != expected_len {
            return Err("bundle length inconsistent with OPK count");
        }

        self.bundles.insert(channel, StoredBundle {
            data: bundle_data,
            stored_at: Instant::now(),
        });

        tracing::debug!("prekey bundle stored");
        Ok(())
    }

    pub fn fetch(&self, channel: &Channel) -> Option<Vec<u8>> {
        let mut entry = self.bundles.get_mut(channel)?;
        let bundle = &mut entry.value_mut();

        let response = bundle.data.clone();
        bundle.data = consume_one_opk(&bundle.data);

        Some(response)
    }

    pub fn cleanup(&self) {
        let ttl = Duration::from_secs(self.config.bundle_ttl_secs);
        let cutoff = match Instant::now().checked_sub(ttl) {
            Some(c) => c,
            None => return,
        };

        let before = self.bundles.len();
        self.bundles.retain(|_, b| b.stored_at >= cutoff);
        let evicted = before - self.bundles.len();

        if evicted > 0 {
            tracing::debug!(evicted, "prekey cleanup complete");
        }
    }

    pub fn spawn_cleanup(self: &Arc<Self>) {
        let store = Arc::clone(self);
        let interval = Duration::from_secs(self.config.cleanup_interval_secs);

        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            loop {
                tick.tick().await;
                store.cleanup();
            }
        });
    }
}
