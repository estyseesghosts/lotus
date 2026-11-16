//! Request/reply/event envelope shared by every IPC channel.

use serde::{Deserialize, Serialize};

use crate::error::{ProtocolError, Result};

/// Maximum payload accepted in a single envelope.
///
/// Every request must define a maximum. This is the hard ceiling enforced
/// before any allocation happens.
pub const MAX_PAYLOAD_LEN: u32 = 64 * 1024 * 1024;

/// Kind of message carried by an envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum EnvelopeKind {
    /// A request from a client to a worker.
    Request = 1,
    /// A reply to a previously issued request.
    Reply = 2,
    /// An unsolicited event from a worker.
    Event = 3,
}

impl EnvelopeKind {
    /// Convert a wire byte into an envelope kind.
    pub fn from_wire(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Request),
            2 => Ok(Self::Reply),
            3 => Ok(Self::Event),
            other => Err(ProtocolError::Malformed(format!(
                "unknown envelope kind {other}"
            ))),
        }
    }
}

/// Fixed-size session identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionUuid(pub [u8; 16]);

impl SessionUuid {
    /// Generate a random session identifier.
    pub fn generate() -> Self {
        // A random value is good enough for a per-run local identifier; it is
        // never used as a security token.
        let mut bytes = [0u8; 16];
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let pid = std::process::id() as u128;
        let mut state = nanos ^ (pid << 64) ^ 0x9E37_79B9_7F4A_7C15;
        for byte in bytes.iter_mut() {
            // xorshift64*
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let value = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
            *byte = (value >> 32) as u8;
        }
        Self(bytes)
    }

    /// Raw bytes of the identifier.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl std::fmt::Display for SessionUuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0.iter() {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Header of every IPC message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvelopeHeader {
    /// Protocol major version of the sender.
    pub version_major: u16,
    /// Protocol minor version of the sender.
    pub version_minor: u16,
    /// Identifier of the request this message belongs to.
    pub request_id: u64,
    /// Session the message belongs to.
    pub session_uuid: SessionUuid,
    /// State epoch the message was produced in.
    pub session_epoch: u64,
    /// Message kind.
    pub kind: EnvelopeKind,
    /// Length of the payload that follows.
    pub payload_len: u32,
}

impl EnvelopeHeader {
    /// Build a header for a message of `payload_len` bytes.
    pub fn new(
        request_id: u64,
        session_uuid: SessionUuid,
        session_epoch: u64,
        kind: EnvelopeKind,
        payload_len: u32,
    ) -> Self {
        Self {
            version_major: crate::PROTOCOL_VERSION_MAJOR,
            version_minor: crate::PROTOCOL_VERSION_MINOR,
            request_id,
            session_uuid,
            session_epoch,
            kind,
            payload_len,
        }
    }

    /// Reject payloads that exceed the hard ceiling before allocating.
    pub fn check_payload_len(&self) -> Result<()> {
        if self.payload_len > MAX_PAYLOAD_LEN {
            return Err(ProtocolError::PayloadTooLarge {
                len: self.payload_len,
                max: MAX_PAYLOAD_LEN,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_uuids_differ() {
        let a = SessionUuid::generate();
        let b = SessionUuid::generate();
        assert_ne!(a, b);
        assert_eq!(a.to_string().len(), 32);
    }

    #[test]
    fn payload_limit_is_enforced() {
        let header = EnvelopeHeader::new(
            1,
            SessionUuid::generate(),
            0,
            EnvelopeKind::Request,
            MAX_PAYLOAD_LEN + 1,
        );
        assert!(matches!(
            header.check_payload_len(),
            Err(ProtocolError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn envelope_kind_round_trips() {
        for kind in [
            EnvelopeKind::Request,
            EnvelopeKind::Reply,
            EnvelopeKind::Event,
        ] {
            assert_eq!(EnvelopeKind::from_wire(kind as u8).unwrap(), kind);
        }
        assert!(EnvelopeKind::from_wire(9).is_err());
    }
}
