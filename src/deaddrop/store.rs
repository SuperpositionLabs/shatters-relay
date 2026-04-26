use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::Mutex;

use crate::protocol::Channel;
use crate::config::DeadDropConfig;

struct StoredEnvelope {
    data:       Vec<u8>,
    stored_at:  Instant,
}

pub struct DeadDropStore {
    drops:  DashMap<Channel, Mutex<VecDeque<StoredEnvelope>>>,
    channel_count: AtomicUsize,
    total_bytes:   AtomicUsize,
    config: DeadDropConfig,
}

impl DeadDropStore {
    pub fn new(config: DeadDropConfig) -> Arc<Self> {
        Arc::new(Self {
            drops: DashMap::new(),
            channel_count: AtomicUsize::new(0),
            total_bytes: AtomicUsize::new(0),
            config,
        })
    }

    pub fn store(&self, channel: &Channel, raw: Vec<u8>) -> Result<Vec<u8>, &'static str> {
        let is_new = !self.drops.contains_key(channel);
        if is_new {
            let prev = self.channel_count.fetch_add(1, Ordering::AcqRel);
            if prev >= self.config.max_channels {
                self.channel_count.fetch_sub(1, Ordering::Release);
                return Err("deaddrop channel limit reached");
            }
        }

        // Payloads are end-to-end encrypted blobs; the relay treats them as
        // opaque. We deliberately do NOT mutate the payload (e.g. injecting a
        // timestamp into it would corrupt any wire format whose bytes at the
        // mutated offset happen to be zero, such as fresh ratchet headers).
        let data = raw;
        let data_len = data.len();

        // Check total bytes budget
        let prev_bytes = self.total_bytes.fetch_add(data_len, Ordering::AcqRel);
        if prev_bytes + data_len > self.config.max_total_bytes {
            self.total_bytes.fetch_sub(data_len, Ordering::Release);
            if is_new {
                self.channel_count.fetch_sub(1, Ordering::Release);
            }
            return Err("deaddrop storage limit reached");
        }

        let envelope = StoredEnvelope {
            data:      data.clone(),
            stored_at: Instant::now(),
        };

        let entry = self.drops.entry(*channel).or_default();
        let mut q = entry.value().lock();
        q.push_back(envelope);

        while q.len() > self.config.max_per_drop {
            if let Some(evicted) = q.pop_front() {
                self.total_bytes.fetch_sub(evicted.data.len(), Ordering::Release);
            }
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
        let mut freed_bytes: usize = 0;

        for entry in self.drops.iter() {
            let mut q = entry.value().lock();
            while q.front().map_or(false, |e| e.stored_at < cutoff) {
                if let Some(evicted) = q.pop_front() {
                    freed_bytes += evicted.data.len();
                }
            }
            if q.is_empty() {
                empty_channels.push(*entry.key());
            }
        }

        let evicted = empty_channels.len();
        for ch in &empty_channels {
            if self.drops.remove_if(ch, |_, q| q.lock().is_empty()).is_some() {
                self.channel_count.fetch_sub(1, Ordering::Release);
            }
        }

        if freed_bytes > 0 {
            self.total_bytes.fetch_sub(freed_bytes, Ordering::Release);
        }

        if evicted > 0 {
            tracing::debug!(evicted_channels = evicted, freed_bytes, "deaddrop cleanup complete");
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
