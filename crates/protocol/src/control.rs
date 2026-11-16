//! Bounded local control messages. Large images never travel on this channel.
use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Maximum encoded control message, checked before allocation.
pub const MAX_CONTROL_BYTES: u32 = 65_536;

/// Commands implemented by the supervisor spike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Hello,
    Debug,
    Status,
    Stop,
    Load,
    Run,
    RunUntil,
    Pause,
    Reset,
    Capture,
    Input,
    ReadMemory,
    Registers,
}

/// Completion status; OK always means completed, never merely queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    InvalidArgument,
    Unsupported,
    NotReady,
    StaleEpoch,
    PermissionDenied,
}

/// Request envelope for one inherited private pipe connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version_major: u16,
    pub version_minor: u16,
    pub request_id: u64,
    pub session: String,
    pub epoch: u64,
    pub method: Method,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Response envelope. Mock identity is explicit and has no emulator capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub version_major: u16,
    pub version_minor: u16,
    pub request_id: u64,
    pub session: String,
    pub epoch: u64,
    pub status: Status,
    pub state: String,
    pub backend: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

/// Encode a JSON control message behind a little-endian u32 byte count.
pub fn encode<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    let payload = serde_json::to_vec(value).map_err(io::Error::other)?;
    let length = u32::try_from(payload.len()).map_err(io::Error::other)?;
    validate_length(length)?;
    let mut bytes = Vec::with_capacity(payload.len() + 4);
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

/// Reject invalid lengths before allocating payload storage.
pub fn validate_length(length: u32) -> io::Result<()> {
    if length == 0 || length > MAX_CONTROL_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid control length",
        ));
    }
    Ok(())
}

/// Read exactly one bounded message; truncation is an error.
pub fn read<T: DeserializeOwned>(reader: &mut impl Read) -> io::Result<T> {
    let mut prefix = [0; 4];
    reader.read_exact(&mut prefix)?;
    let length = u32::from_le_bytes(prefix);
    validate_length(length)?;
    let mut payload = vec![0; length as usize];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(io::Error::other)
}

/// Write and flush a complete message.
pub fn write<T: Serialize>(writer: &mut impl Write, value: &T) -> io::Result<()> {
    writer.write_all(&encode(value)?)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_lengths_and_truncated_json_are_rejected() {
        for length in [0, MAX_CONTROL_BYTES + 1, u32::MAX] {
            assert!(read::<Request>(&mut length.to_le_bytes().as_slice()).is_err());
        }
        assert!(read::<Request>(&mut [2, 0, 0, 0, b'{'].as_slice()).is_err());
        assert!(read::<Request>(&mut [2, 0, 0, 0, b'{', b'}'].as_slice()).is_err());
    }
}
