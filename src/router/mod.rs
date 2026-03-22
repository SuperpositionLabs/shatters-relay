use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::mpsc;

use crate::protocol::{Channel, Message};

pub type SubscriberId = u64;
pub type OutboundFrame = Arc<Vec<u8>>;
pub type OutboundSender = mpsc::Sender<OutboundFrame>;

struct Subscriber {
    id:         SubscriberId,
    sender:     OutboundSender,
    drop_count: AtomicU32,
}

pub struct Router {
    channels: DashMap<Channel, Vec<Subscriber>>,
    sub_counts: DashMap<SubscriberId, usize>,

    channel_count_atomic: AtomicU64,
    next_id: AtomicU64,
    next_msg_id: AtomicU32,

    max_subscriptions: usize,
    max_channels: usize,
    slow_consumer_threshold: usize,
}

impl Router {
    pub fn new() -> Arc<Self> {
        Self::with_limits(50, 100_000, 10)
    }

    pub fn with_limits(
        max_subscriptions_per_conn: usize,
        max_channels: usize,
        slow_consumer_threshold: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            channels: DashMap::new(),
            sub_counts: DashMap::new(),
            channel_count_atomic: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
            next_msg_id: AtomicU32::new(1),
            max_subscriptions: max_subscriptions_per_conn,
            max_channels,
            slow_consumer_threshold,
        })
    }

    pub fn next_subscriber_id(&self) -> SubscriberId {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn alloc_msg_id(&self) -> u32 {
        self.next_msg_id.fetch_add(1, Ordering::Relaxed)
    }

    pub async fn subscribe(
        &self,
        channel: &Channel,
        sub_id:  SubscriberId,
        sender:  OutboundSender,
    ) -> Result<(), &'static str> {
        let count = self.sub_counts.get(&sub_id).map(|c| *c).unwrap_or(0);
        if count >= self.max_subscriptions {
            return Err("subscription limit reached");
        }

        let is_new_channel = !self.channels.contains_key(channel);
        if is_new_channel {
            let prev = self.channel_count_atomic.fetch_add(1, Ordering::AcqRel);
            if prev as usize >= self.max_channels {
                self.channel_count_atomic.fetch_sub(1, Ordering::Release);
                return Err("channel limit reached");
            }
        }

        let mut subs = self.channels.entry(*channel).or_default();

        if !subs.iter().any(|s| s.id == sub_id) {
            subs.push(Subscriber {
                id: sub_id,
                sender,
                drop_count: AtomicU32::new(0),
            });
            *self.sub_counts.entry(sub_id).or_insert(0) += 1;
            tracing::debug!(sub_id, "subscriber added");
        } else if is_new_channel {
            /*  channel entry was created but subscriber already existed
             *  but correct the counter if the entry was truly new
             */
            self.channel_count_atomic.fetch_sub(1, Ordering::Release);
        }
        Ok(())
    }

    pub async fn unsubscribe(&self, channel: &Channel, sub_id: SubscriberId) {
        if let Some(mut subs) = self.channels.get_mut(channel) {
            let before = subs.len();
            subs.retain(|s| s.id != sub_id);
            
            let removed = before - subs.len();
            if removed > 0 {
                if let Some(mut count) = self.sub_counts.get_mut(&sub_id) {
                    *count = count.saturating_sub(removed);
                }
            }

            if subs.is_empty() {
                drop(subs);
                if self.channels.remove(channel).is_some() {
                    self.channel_count_atomic.fetch_sub(1, Ordering::Release);
                }
            }
            tracing::debug!(sub_id, "subscriber removed");
        }
    }

    pub async fn publish(
        &self,
        channel: &Channel,
        payload: &[u8],
    ) -> usize {
        let data_msg = Message::data(
            self.alloc_msg_id(),
            *channel,
            payload.to_vec(),
        );
        let frame = Arc::new(data_msg.serialize());

        let mut subs = match self.channels.get_mut(channel) {
            Some(s) => s,
            None    => return 0,
        };

        let threshold = self.slow_consumer_threshold as u32;
        let mut delivered = 0usize;
        let mut to_evict = Vec::new();

        for (idx, sub) in subs.iter().enumerate() {
            match sub.sender.try_send(Arc::clone(&frame)) {
                Ok(()) => {
                    sub.drop_count.store(0, Ordering::Relaxed);
                    delivered += 1;
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    let drops = sub.drop_count.fetch_add(1, Ordering::Relaxed) + 1;
                    if threshold > 0 && drops >= threshold {
                        tracing::warn!(
                            sub_id = sub.id,
                            consecutive_drops = drops,
                            "evicting slow consumer"
                        );
                        to_evict.push(idx);
                    } else {
                        tracing::warn!(
                            sub_id = sub.id,
                            "subscriber channel full, dropping message"
                        );
                    }
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    tracing::debug!(
                        sub_id = sub.id,
                        "subscriber channel closed"
                    );
                    to_evict.push(idx);
                }
            }
        }

        /* remove evicted subscribers in reverse order to preserve indices */
        for &idx in to_evict.iter().rev() {
            let evicted = subs.swap_remove(idx);
            if let Some(mut count) = self.sub_counts.get_mut(&evicted.id) {
                *count = count.saturating_sub(1);
            }
        }

        if subs.is_empty() {
            drop(subs);
            if self.channels.remove(channel).is_some() {
                self.channel_count_atomic.fetch_sub(1, Ordering::Release);
            }
        }

        delivered
    }

    pub async fn remove_subscriber(&self, sub_id: SubscriberId) {
        let mut empty_channels = Vec::new();

        for mut entry in self.channels.iter_mut() {
            entry.value_mut().retain(|s| s.id != sub_id);
            if entry.value().is_empty() {
                empty_channels.push(*entry.key());
            }
        }

        for ch in empty_channels {
            if self.channels.remove_if(&ch, |_, subs| subs.is_empty()).is_some() {
                self.channel_count_atomic.fetch_sub(1, Ordering::Release);
            }
        }

        self.sub_counts.remove(&sub_id);

        tracing::debug!(sub_id, "subscriber removed from all channels");
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn subscription_count(&self) -> usize {
        self.channels
            .iter()
            .map(|entry| entry.value().len())
            .sum()
    }
}
