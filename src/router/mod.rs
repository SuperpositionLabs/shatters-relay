use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::mpsc;

use crate::protocol::{Channel, Message};

pub type SubscriberId = u64;
pub type OutboundSender = mpsc::Sender<Vec<u8>>;

struct Subscriber {
    id:     SubscriberId,
    sender: OutboundSender,
}

pub struct Router {
    channels: DashMap<Channel, Vec<Subscriber>>,
    sub_counts: DashMap<SubscriberId, usize>,

    next_id: AtomicU64,
    next_msg_id: AtomicU32,

    max_subscriptions: usize,
}

impl Router {
    pub fn new() -> Arc<Self> {
        Self::with_limits(50)
    }

    pub fn with_limits(max_subscriptions_per_conn: usize) -> Arc<Self> {
        Arc::new(Self {
            channels: DashMap::new(),
            sub_counts: DashMap::new(),
            next_id: AtomicU64::new(1),
            next_msg_id: AtomicU32::new(1),
            max_subscriptions: max_subscriptions_per_conn,
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

        let mut subs = self.channels.entry(*channel).or_default();

        if !subs.iter().any(|s| s.id == sub_id) {
            subs.push(Subscriber {
                id: sub_id,
                sender,
            });
            *self.sub_counts.entry(sub_id).or_insert(0) += 1;
            tracing::debug!(sub_id, "subscriber added");
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
                self.channels.remove(channel);
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
        let frame = data_msg.serialize();

        let subs = match self.channels.get(channel) {
            Some(s) => s,
            None    => return 0,
        };

        let mut delivered = 0usize;
        for sub in subs.iter() {
            match sub.sender.try_send(frame.clone()) {
                Ok(()) => delivered += 1,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!(
                        sub_id = sub.id,
                        "subscriber channel full, dropping message"
                    );
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    tracing::debug!(
                        sub_id = sub.id,
                        "subscriber channel closed"
                    );
                }
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
            self.channels.remove_if(&ch, |_, subs| subs.is_empty());
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
