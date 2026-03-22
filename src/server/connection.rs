use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use quinn::Connection;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::auth::AuthState;
use crate::config::LimitsConfig;
use crate::deaddrop::store::DeadDropStore;
use crate::prekey::store::PreKeyStore;
use crate::protocol::message::{Message, MessageType, ProtocolError, PROTOCOL_VERSION};
use crate::ratelimit::TokenBucket;
use crate::router::{OutboundFrame, OutboundSender, Router, SubscriberId};
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
    auth_timeout_secs: u64,
    max_payload_size:  usize,
    max_bytes_per_conn: usize,
    bytes_in_flight: Arc<AtomicUsize>,
}

impl ConnectionCtx {
    fn send_reply(&self, msg: Message) {
        let frame = Arc::new(msg.serialize());
        let len = frame.len();
        if self.bytes_in_flight.load(Ordering::Relaxed) + len > self.max_bytes_per_conn {
            tracing::warn!(sub_id = self.sub_id, "per-connection memory budget exceeded, dropping");
            return;
        }
        self.bytes_in_flight.fetch_add(len, Ordering::Relaxed);
        if self.outbound_tx.try_send(frame).is_err() {
            self.bytes_in_flight.fetch_sub(len, Ordering::Relaxed);
            tracing::warn!(sub_id = self.sub_id, "outbound channel full/closed");
        }
    }
}

pub async fn handle(
    conn:          Connection,
    router:        Arc<Router>,
    store:         Arc<DeadDropStore>,
    prekeys:       Arc<PreKeyStore>,
    limits:        LimitsConfig,
    auth_timeout:  u64,
) -> Result<(), ConnectionError> {
    let (send, recv) = conn.accept_bi().await?;

    let sub_id = router.next_subscriber_id();
    tracing::info!(sub_id, "stream accepted");

    let (outbound_tx, outbound_rx) = mpsc::channel::<OutboundFrame>(OUTBOUND_CHANNEL_SIZE);

    let bytes_in_flight = Arc::new(AtomicUsize::new(0));

    let ctx = ConnectionCtx {
        sub_id,
        outbound_tx,
        router,
        store,
        prekeys,
        limiter: TokenBucket::new(limits.requests_per_second, limits.burst_size),
        auth_timeout_secs: auth_timeout,
        max_payload_size: limits.max_payload_size,
        max_bytes_per_conn: limits.max_bytes_per_conn,
        bytes_in_flight: Arc::clone(&bytes_in_flight),
    };

    let writer_handle = tokio::spawn(writer_task(send, outbound_rx, bytes_in_flight));

    let result = reader_loop(recv, &ctx).await;

    let router = Arc::clone(&ctx.router);
    drop(ctx);

    let _ = writer_handle.await;
    router.remove_subscriber(sub_id).await;

    match &result {
        Ok(()) => tracing::info!(sub_id, "stream closed cleanly"),
        Err(e) => tracing::warn!(sub_id, error = %e, "stream error"),
    }

    result
}

async fn writer_task(
    mut send: quinn::SendStream,
    mut rx:   mpsc::Receiver<OutboundFrame>,
    bytes_in_flight: Arc<AtomicUsize>,
) {
    while let Some(frame) = rx.recv().await {
        let len = frame.len();
        if let Err(e) = quic::write_frame(&mut send, &frame).await {
            tracing::warn!(error = %e, "write_frame failed, closing writer");
            bytes_in_flight.fetch_sub(len, Ordering::Relaxed);
            break;
        }
        bytes_in_flight.fetch_sub(len, Ordering::Relaxed);
    }
    let _ = send.finish();
}

async fn reader_loop(
    mut recv: quinn::RecvStream,
    ctx:      &ConnectionCtx,
) -> Result<(), ConnectionError> {
    let mut auth = AuthState::new();

    loop {
        let frame = match quic::read_frame(&mut recv).await {
            Ok(f)  => f,
            Err(QuicError::Read(quinn::ReadExactError::FinishedEarly(_))) => {
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };

        let msg = match Message::deserialize_bounded(&frame, Some(ctx.max_payload_size)) {
            Ok(m)  => m,
            Err(ProtocolError::UnsupportedVersion(v)) => {
                tracing::warn!(sub_id = ctx.sub_id, client_version = v, "unsupported protocol version");
                let reason = format!("unsupported version {v}, server supports {PROTOCOL_VERSION}");
                ctx.send_reply(Message::nack(0, &reason));
                continue;
            }
            Err(ProtocolError::PayloadTooLarge { size, max }) => {
                tracing::warn!(sub_id = ctx.sub_id, size, max, "payload too large");
                ctx.send_reply(Message::nack(0, "payload too large"));
                continue;
            }
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

        if let Err(e) = dispatch(msg, ctx, &mut auth).await {
            tracing::warn!(sub_id = ctx.sub_id, error = %e, "dispatch error");
        }
    }
}

async fn dispatch(
    msg:  Message,
    ctx:  &ConnectionCtx,
    auth: &mut AuthState,
) -> Result<(), ConnectionError> {
    match msg.msg_type {
        /* --- open operations --------------------------------------- */

        MessageType::Authenticate => {
            match auth.authenticate(&msg.payload, ctx.auth_timeout_secs) {
                Ok(()) => {
                    tracing::info!(sub_id = ctx.sub_id, "client authenticated");
                    ctx.send_reply(Message::ack(msg.id));
                }
                Err(reason) => {
                    tracing::warn!(sub_id = ctx.sub_id, reason, "auth rejected");
                    ctx.send_reply(Message::nack(msg.id, reason));
                }
            }
        }

        MessageType::Publish => {
            let inner = match auth.verify_channel_proof(&msg.channel, &msg.payload) {
                Ok(inner) => inner,
                Err(reason) => {
                    ctx.send_reply(Message::nack(msg.id, reason));
                    return Ok(());
                }
            };

            match ctx.store.store(&msg.channel, inner.to_vec()) {
                Ok(payload) => {
                    ctx.router.publish(&msg.channel, &payload).await;
                    ctx.send_reply(Message::ack(msg.id));
                }
                Err(reason) => {
                    ctx.send_reply(Message::nack(msg.id, reason));
                }
            }
        }

        MessageType::FetchBundle => {
            if !auth.is_authenticated() {
                ctx.send_reply(Message::nack(msg.id, "not authenticated"));
                return Ok(());
            }

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

        /* --- privileged operations ----------------------------------- */

        MessageType::Subscribe => {
            if let Err(reason) = auth.verify_channel_proof(&msg.channel, &msg.payload) {
                ctx.send_reply(Message::nack(msg.id, reason));
                return Ok(());
            }

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
            if !auth.is_authenticated() {
                ctx.send_reply(Message::nack(msg.id, "not authenticated"));
                return Ok(());
            }
            ctx.router.unsubscribe(&msg.channel, ctx.sub_id).await;
            ctx.send_reply(Message::ack(msg.id));
        }

        MessageType::Retrieve => {
            let inner = match auth.verify_channel_proof(&msg.channel, &msg.payload) {
                Ok(inner) => inner,
                Err(reason) => {
                    ctx.send_reply(Message::nack(msg.id, reason));
                    return Ok(());
                }
            };

            let ttl = parse_ttl(inner);
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
            let inner = match auth.verify_channel_proof(&msg.channel, &msg.payload) {
                Ok(inner) => inner,
                Err(reason) => {
                    tracing::warn!(sub_id = ctx.sub_id, reason, "upload auth failed");
                    ctx.send_reply(Message::nack(msg.id, reason));
                    return Ok(());
                }
            };

            match ctx.prekeys.upload(msg.channel, inner.to_vec()) {
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
