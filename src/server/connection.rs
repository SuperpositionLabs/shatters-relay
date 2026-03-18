use std::sync::Arc;
use std::time::Duration;

use quinn::Connection;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::config::LimitsConfig;
use crate::deaddrop::store::DeadDropStore;
use crate::prekey::store::PreKeyStore;
use crate::protocol::message::{Message, MessageType, ProtocolError};
use crate::ratelimit::TokenBucket;
use crate::router::{OutboundSender, Router, SubscriberId};
use crate::transport::quic::{self, QuicError};

const OUTBOUND_CHANNEL_SIZE: usize = 256;

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error("quic connection error: {0}")]
    Quinn(#[from] quinn::ConnectionError),

    #[error("quic stream error: {0}")]
    Quic(#[from] QuicError),

    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
}

struct ConnectionCtx {
    sub_id:      SubscriberId,
    outbound_tx: OutboundSender,
    router:      Arc<Router>,
    store:       Arc<DeadDropStore>,
    prekeys:     Arc<PreKeyStore>,
    limiter:     TokenBucket,
}

impl ConnectionCtx {
    fn send_reply(&self, msg: Message) {
        let frame = msg.serialize();
        if self.outbound_tx.try_send(frame).is_err() {
            tracing::warn!(sub_id = self.sub_id, "outbound channel full/closed");
        }
    }
}

pub async fn handle(
    conn:     Connection,
    router:   Arc<Router>,
    store:    Arc<DeadDropStore>,
    prekeys:  Arc<PreKeyStore>,
    limits:   LimitsConfig,
) -> Result<(), ConnectionError> {
    let remote = conn.remote_address();
    let (send, recv) = conn.accept_bi().await?;

    let sub_id = router.next_subscriber_id();
    tracing::info!(remote = %remote, sub_id, "stream accepted");

    let (outbound_tx, outbound_rx) = mpsc::channel::<Vec<u8>>(OUTBOUND_CHANNEL_SIZE);

    let ctx = ConnectionCtx {
        sub_id,
        outbound_tx,
        router,
        store,
        prekeys,
        limiter: TokenBucket::new(limits.requests_per_second, limits.burst_size),
    };

    let writer_handle = tokio::spawn(writer_task(send, outbound_rx));

    let result = reader_loop(recv, &ctx).await;

    let router = Arc::clone(&ctx.router);
    drop(ctx);

    let _ = writer_handle.await;
    router.remove_subscriber(sub_id).await;

    match &result {
        Ok(()) => tracing::info!(remote = %remote, sub_id, "stream closed cleanly"),
        Err(e) => tracing::warn!(remote = %remote, sub_id, error = %e, "stream error"),
    }

    result
}

async fn writer_task(
    mut send: quinn::SendStream,
    mut rx:   mpsc::Receiver<Vec<u8>>,
) {
    while let Some(frame) = rx.recv().await {
        if let Err(e) = quic::write_frame(&mut send, &frame).await {
            tracing::warn!(error = %e, "write_frame failed, closing writer");
            break;
        }
    }
    let _ = send.finish();
}

async fn reader_loop(
    mut recv: quinn::RecvStream,
    ctx:      &ConnectionCtx,
) -> Result<(), ConnectionError> {
    loop {
        let frame = match quic::read_frame(&mut recv).await {
            Ok(f)  => f,
            Err(QuicError::Read(quinn::ReadExactError::FinishedEarly(_))) => {
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };

        let msg = match Message::deserialize(&frame) {
            Ok(m)  => m,
            Err(e) => {
                tracing::warn!(sub_id = ctx.sub_id, error = %e, "bad message, skipping");
                continue;
            }
        };

        if !ctx.limiter.try_acquire(msg.msg_type.token_cost()) {
            tracing::warn!(sub_id = ctx.sub_id, msg_type = msg.msg_type as u8, "rate limited");
            ctx.send_reply(Message::nack(msg.id, "rate limited"));
            continue;
        }

        if let Err(e) = dispatch(msg, ctx).await {
            tracing::warn!(sub_id = ctx.sub_id, error = %e, "dispatch error");
        }
    }
}

async fn dispatch(
    msg: Message,
    ctx: &ConnectionCtx,
) -> Result<(), ConnectionError> {
    match msg.msg_type {
        MessageType::Publish => {
            let payload = ctx.store.store(&msg.channel, msg.payload);
            ctx.router.publish(&msg.channel, &payload).await;
            ctx.send_reply(Message::ack(msg.id));
        }

        MessageType::Subscribe => {
            match ctx.router
                .subscribe(&msg.channel, ctx.sub_id, ctx.outbound_tx.clone())
                .await
            {
                Ok(()) => ctx.send_reply(Message::ack(msg.id)),
                Err(reason) => {
                    tracing::warn!(sub_id = ctx.sub_id, reason, "subscription rejected");
                    ctx.send_reply(Message::nack(msg.id, reason));
                }
            }
        }

        MessageType::Unsubscribe => {
            ctx.router.unsubscribe(&msg.channel, ctx.sub_id).await;
            ctx.send_reply(Message::ack(msg.id));
        }

        MessageType::Retrieve => {
            let ttl = parse_ttl(&msg.payload);
            let envelopes = ctx.store.retrieve(&msg.channel, ttl);

            tracing::debug!(
                count = envelopes.len(),
                ttl_secs = ttl.as_secs(),
                "retrieve"
            );

            for env_data in envelopes {
                let data_msg = Message::data(
                    ctx.router.alloc_msg_id(),
                    msg.channel,
                    env_data,
                );
                ctx.send_reply(data_msg);
            }

            ctx.send_reply(Message::ack(msg.id));
        }

        MessageType::UploadBundle => {
            match ctx.prekeys.upload(msg.channel, msg.payload) {
                Ok(()) => {
                    tracing::debug!(sub_id = ctx.sub_id, "prekey bundle uploaded");
                    ctx.send_reply(Message::ack(msg.id));
                }
                Err(reason) => {
                    tracing::warn!(sub_id = ctx.sub_id, reason, "prekey upload rejected");
                    ctx.send_reply(Message::nack(msg.id, reason));
                }
            }
        }

        MessageType::FetchBundle => {
            match ctx.prekeys.fetch(&msg.channel) {
                Some(bundle) => {
                    let bundle_msg = Message {
                        msg_type: MessageType::BundleData,
                        id: ctx.router.alloc_msg_id(),
                        channel: msg.channel,
                        payload: bundle,
                    };
                    ctx.send_reply(bundle_msg);
                    ctx.send_reply(Message::ack(msg.id));
                }
                None => {
                    ctx.send_reply(Message::nack(msg.id, "bundle not found"));
                }
            }
        }

        _ => {
            tracing::debug!(
                sub_id = ctx.sub_id,
                msg_type = msg.msg_type as u8,
                "ignoring unexpected message type from client"
            );
        }
    }

    Ok(())
}

// ---- helpers ------------------------------------------------------------

/// Parse the TTL hint from a Retrieve payload (4 bytes BE seconds).
/// Falls back to 24 h if the payload is too short or zero.
fn parse_ttl(payload: &[u8]) -> Duration {
    const DEFAULT_TTL: Duration = Duration::from_secs(86_400);

    if payload.len() < 4 {
        return DEFAULT_TTL;
    }

    let secs = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    if secs == 0 {
        DEFAULT_TTL
    } else {
        Duration::from_secs(secs as u64)
    }
}
