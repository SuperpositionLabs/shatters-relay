use ed25519_dalek::{Signature, VerifyingKey};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::protocol::message::Channel;

const AUTH_DOMAIN: &[u8] = b"$hatter$-auth-v1\x00";
const CHAN_DOMAIN: &[u8] = b"$hatter$-chan-v1\x00";

pub const PROOF_SIZE: usize = 64;

const PUBKEY_SIZE: usize = 32;
const TIMESTAMP_SIZE: usize = 8;
const AUTH_PAYLOAD_MIN: usize = PUBKEY_SIZE + TIMESTAMP_SIZE + PROOF_SIZE;

pub struct AuthState {
    key: Option<VerifyingKey>,
}

impl AuthState {
    pub fn new() -> Self {
        Self { key: None }
    }

    pub fn is_authenticated(&self) -> bool {
        self.key.is_some()
    }

    /// Process an Authenticate message payload.
    ///
    /// Payload: `[pubkey: 32] [timestamp_ms: 8 BE] [signature: 64]`
    /// Signature covers: `AUTH_DOMAIN || pubkey || timestamp_ms`
    pub fn authenticate(
        &mut self,
        payload: &[u8],
        max_skew_secs: u64,
    ) -> Result<(), &'static str> {
        if payload.len() < AUTH_PAYLOAD_MIN {
            return Err("auth payload too short");
        }

        let pubkey_bytes: &[u8; 32] = payload[..32]
            .try_into()
            .map_err(|_| "invalid pubkey")?;

        let timestamp_ms = u64::from_be_bytes(
            payload[32..40]
                .try_into()
                .map_err(|_| "invalid timestamp")?,
        );

        let sig_bytes: &[u8; 64] = payload[40..104]
            .try_into()
            .map_err(|_| "invalid signature")?;

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let skew_ms = max_skew_secs.saturating_mul(1000);
        if now_ms.abs_diff(timestamp_ms) > skew_ms {
            return Err("auth timestamp expired");
        }

        let verifying_key =
            VerifyingKey::from_bytes(pubkey_bytes).map_err(|_| "invalid public key")?;
        let signature = Signature::from_bytes(sig_bytes);

        let mut signed_msg = Vec::with_capacity(AUTH_DOMAIN.len() + 32 + 8);
        signed_msg.extend_from_slice(AUTH_DOMAIN);
        signed_msg.extend_from_slice(pubkey_bytes);
        signed_msg.extend_from_slice(&timestamp_ms.to_be_bytes());

        verifying_key
            .verify_strict(&signed_msg, &signature)
            .map_err(|_| "auth signature invalid")?;

        self.key = Some(verifying_key);
        Ok(())
    }

    /// Verify that the authenticated user authorised access to `channel`.
    ///
    /// The first 64 bytes of `payload` must be `sign(CHAN_DOMAIN || channel, sk)`.
    /// Returns the remaining payload after the 64-byte proof.
    pub fn verify_channel_proof<'a>(
        &self,
        channel: &Channel,
        payload: &'a [u8],
    ) -> Result<&'a [u8], &'static str> {
        let pubkey = self.key.as_ref().ok_or("not authenticated")?;

        if payload.len() < PROOF_SIZE {
            return Err("channel proof missing");
        }

        let sig_bytes: &[u8; 64] = payload[..PROOF_SIZE]
            .try_into()
            .map_err(|_| "invalid proof")?;
        let signature = Signature::from_bytes(sig_bytes);

        let mut signed_msg = Vec::with_capacity(CHAN_DOMAIN.len() + channel.len());
        signed_msg.extend_from_slice(CHAN_DOMAIN);
        signed_msg.extend_from_slice(channel);

        pubkey
            .verify_strict(&signed_msg, &signature)
            .map_err(|_| "channel proof invalid")?;

        Ok(&payload[PROOF_SIZE..])
    }
}
