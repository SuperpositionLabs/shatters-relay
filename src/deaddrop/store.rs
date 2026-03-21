use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use parking_lot::Mutex;

use crate::protocol::Channel;
use crate::config::DeadDropConfig;

struct StoredEnvelope {
    data:       Vec<u8>,
    stored_at:  Instant,
}

const DEADDROP_ID_SIZE: usize = 32;
const TIMESTAMP_SIZE:   usize = 8;
const HEADER_SIZE:      usize = DEADDROP_ID_SIZE + TIMESTAMP_SIZE;

fn inject_timestamp(mut data: Vec<u8>) -> Vec<u8> {
    if data.len() < HEADER_SIZE {
        return data;
    }

    let ts_offset = DEADDROP_ID_SIZE;
    let ts = u64::from_be_bytes(
        data[ts_offset..ts_offset + TIMESTAMP_SIZE]
            .try_into()
            .unwrap(),
    );

    if ts == 0 {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        data[ts_offset..ts_offset + TIMESTAMP_SIZE].copy_from_slice(&now_ms.to_be_bytes());
    }

    data
}

pub struct DeadDropStore {
    drops:  DashMap<Channel, Mutex<VecDeque<StoredEnvelope>>>,
    config: DeadDropConfig,
}

impl DeadDropStore {
    pub fn new(config: DeadDropConfig) -> Arc<Self> {
        Arc::new(Self {
            drops: DashMap::new(),
            config,
        })
    }

    pub fn store(&self, channel: &Channel, raw: Vec<u8>) -> Result<Vec<u8>, &'static str> {
        if !self.drops.contains_key(channel) && self.drops.len() >= self.config.max_channels {
            return Err("deaddrop channel limit reached");
        }

        let data = inject_timestamp(raw);

        let envelope = StoredEnvelope {
            data:      data.clone(),
            stored_at: Instant::now(),
        };

        let entry = self.drops.entry(*channel).or_default();
        let mut q = entry.value().lock();
        q.push_back(envelope);

        while q.len() > self.config.max_per_drop {
            q.pop_front();
        }

        tracing::trace!(stored = q.len(), "envelope stored");
        Ok(data)
    }

    pub fn retrieve(&self, channel: &Channel, max_age: Duration) -> Vec<Vec<u8>> {
        let entry = match self.drops.get(channel) {
            Some(e) => e,
            None    => return Vec::new(),
        };
        let q = entry.value().lock();

        let cutoff = match Instant::now().checked_sub(max_age) {
            Some(c) => c,
            None => return q.iter().map(|e| e.data.clone()).collect(),
        };

        q.iter()
            .filter(|e| e.stored_at >= cutoff)
            .map(|e| e.data.clone())
            .collect()
    }

    pub fn cleanup(&self) {
        let cutoff = match Instant::now().checked_sub(Duration::from_secs(self.config.default_ttl_secs)) {
            Some(c) => c,
            None => return,
        };

        let mut empty_channels: Vec<Channel> = Vec::new();

        for entry in self.drops.iter() {
            let mut q = entry.value().lock();
            while q.front().map_or(false, |e| e.stored_at < cutoff) {
                q.pop_front();
            }
            if q.is_empty() {
                empty_channels.push(*entry.key());
            }
        }

        let evicted = empty_channels.len();
        for ch in &empty_channels {
            self.drops.remove_if(ch, |_, q| q.lock().is_empty());
        }

        if evicted > 0 {
            tracing::debug!(evicted_channels = evicted, "deaddrop cleanup complete");
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

    #[allow(dead_code)]
    pub fn channel_count(&self) -> usize {
        self.drops.len()
    }

    #[allow(dead_code)]
    pub fn total_envelopes(&self) -> usize {
        self.drops.iter().map(|e| e.value().lock().len()).sum()
    }
}
