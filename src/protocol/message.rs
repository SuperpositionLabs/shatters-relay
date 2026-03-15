use thiserror::Error;

pub const CHANNEL_SIZE: usize = 32;
pub type Channel = [u8; CHANNEL_SIZE];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    Publish       = 0x01,
    Subscribe     = 0x02,
    Unsubscribe   = 0x03,
    Retrieve      = 0x04,
    UploadBundle  = 0x05,
    FetchBundle   = 0x06,
    Ack           = 0x10,
    Nack          = 0x11,
    Data          = 0x20,
    BundleData    = 0x21,
}

impl MessageType {
    pub fn from_byte(b: u8) -> Result<Self, ProtocolError> {
        match b {
            0x01 => Ok(Self::Publish),
            0x02 => Ok(Self::Subscribe),
            0x03 => Ok(Self::Unsubscribe),
            0x04 => Ok(Self::Retrieve),
            0x05 => Ok(Self::UploadBundle),
            0x06 => Ok(Self::FetchBundle),
            0x10 => Ok(Self::Ack),
            0x11 => Ok(Self::Nack),
            0x20 => Ok(Self::Data),
            0x21 => Ok(Self::BundleData),
            _    => Err(ProtocolError::InvalidMessageType(b)),
        }
    }

    pub fn token_cost(&self) -> u32 {
        match self {
            Self::Publish      => 3,
            Self::UploadBundle => 3,
            Self::Subscribe    => 2,
            Self::Retrieve     => 2,
            Self::FetchBundle  => 2,
            Self::Unsubscribe  => 1,
            _                  => 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Message {
    pub msg_type: MessageType,
    pub id:       u32,
    pub channel:  Channel,
    pub payload:  Vec<u8>,
}

impl Message {
    pub fn ack(id: u32) -> Self {
        Self {
            msg_type: MessageType::Ack,
            id,
            channel: [0u8; CHANNEL_SIZE],
            payload: Vec::new(),
        }
    }

    pub fn nack(id: u32, reason: &str) -> Self {
        Self {
            msg_type: MessageType::Nack,
            id,
            channel: [0u8; CHANNEL_SIZE],
            payload: reason.as_bytes().to_vec(),
        }
    }

    pub fn data(id: u32, channel: Channel, payload: Vec<u8>) -> Self {
        Self {
            msg_type: MessageType::Data,
            id,
            channel,
            payload,
        }
    }

    pub fn serialize(&self) -> Vec<u8> {
        let total = 1 + 4 + CHANNEL_SIZE + self.payload.len();

        let mut buf = Vec::with_capacity(total);
        buf.push(self.msg_type as u8);
        buf.extend_from_slice(&self.id.to_be_bytes());
        buf.extend_from_slice(&self.channel);
        buf.extend_from_slice(&self.payload);
        buf
    }

    pub fn deserialize(data: &[u8]) -> Result<Self, ProtocolError> {
        const MIN_HEADER: usize = 1 + 4 + CHANNEL_SIZE;

        if data.len() < MIN_HEADER {
            return Err(ProtocolError::TooShort {
                need: MIN_HEADER,
                got: data.len(),
            });
        }

        let msg_type = MessageType::from_byte(data[0])?;
        let id = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);

        let mut channel = [0u8; CHANNEL_SIZE];
        channel.copy_from_slice(&data[5..5 + CHANNEL_SIZE]);

        let payload = data[5 + CHANNEL_SIZE..].to_vec();

        Ok(Self {
            msg_type,
            id,
            channel,
            payload,
        })
    }
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("message too short: need {need}, got {got}")]
    TooShort { need: usize, got: usize },

    #[error("invalid message type: 0x{0:02x}")]
    InvalidMessageType(u8),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel(fill: u8) -> Channel {
        [fill; CHANNEL_SIZE]
    }

    #[test]
    fn round_trip_publish() {
        let msg = Message {
            msg_type: MessageType::Publish,
            id: 42,
            channel: make_channel(0xAA),
            payload: vec![0xCA, 0xFE],
        };
        let wire = msg.serialize();
        let decoded = Message::deserialize(&wire).unwrap();
        assert_eq!(decoded.msg_type, MessageType::Publish);
        assert_eq!(decoded.id, 42);
        assert_eq!(decoded.channel, make_channel(0xAA));
        assert_eq!(decoded.payload, vec![0xCA, 0xFE]);
    }

    #[test]
    fn round_trip_ack() {
        let msg = Message::ack(99);
        let wire = msg.serialize();
        let decoded = Message::deserialize(&wire).unwrap();
        assert_eq!(decoded.msg_type, MessageType::Ack);
        assert_eq!(decoded.id, 99);
    }

    #[test]
    fn round_trip_data() {
        let ch = make_channel(0xBB);
        let msg = Message::data(7, ch, vec![1, 2, 3]);
        let wire = msg.serialize();
        let decoded = Message::deserialize(&wire).unwrap();
        assert_eq!(decoded.msg_type, MessageType::Data);
        assert_eq!(decoded.channel, ch);
        assert_eq!(decoded.payload, vec![1, 2, 3]);
    }

    #[test]
    fn too_short_returns_error() {
        assert!(Message::deserialize(&[0x01, 0x00]).is_err());
    }

    #[test]
    fn bad_type_returns_error() {
        let mut bad = vec![0xFF];
        bad.extend_from_slice(&[0u8; 36]); // id(4) + channel(32)
        assert!(Message::deserialize(&bad).is_err());
    }

    #[test]
    fn exact_min_header() {
        // 37 bytes: type(1) + id(4) + channel(32), no payload
        let mut data = vec![0x01]; // Publish
        data.extend_from_slice(&[0u8; 4]); // id = 0
        data.extend_from_slice(&[0x42u8; 32]); // channel
        let msg = Message::deserialize(&data).unwrap();
        assert_eq!(msg.msg_type, MessageType::Publish);
        assert_eq!(msg.channel, [0x42u8; 32]);
        assert!(msg.payload.is_empty());
    }

    #[test]
    fn fixed_header_size() {
        let msg = Message {
            msg_type: MessageType::Publish,
            id: 1,
            channel: make_channel(0x01),
            payload: vec![],
        };
        let wire = msg.serialize();
        assert_eq!(wire.len(), 37); // 1 + 4 + 32
    }
}
