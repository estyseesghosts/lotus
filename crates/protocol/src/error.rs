//! Protocol level errors.

use thiserror::Error;

/// Result alias for protocol operations.
pub type Result<T> = std::result::Result<T, ProtocolError>;

/// Errors raised by the protocol layer.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    /// The peer speaks an incompatible protocol version.
    #[error("incompatible protocol version: peer {peer_major}.{peer_minor}, local {local_major}.{local_minor}")]
    IncompatibleVersion {
        /// Peer major version.
        peer_major: u16,
        /// Peer minor version.
        peer_minor: u16,
        /// Local major version.
        local_major: u16,
        /// Local minor version.
        local_minor: u16,
    },
    /// The payload exceeds the enforced maximum.
    #[error("payload too large: {len} bytes, maximum is {max}")]
    PayloadTooLarge {
        /// Observed payload length.
        len: u32,
        /// Enforced maximum.
        max: u32,
    },
    /// The message could not be decoded.
    #[error("malformed message: {0}")]
    Malformed(String),
    /// The request is not known to this build.
    #[error("unknown request: {0}")]
    UnknownRequest(String),
    /// The worker is not running or not reachable.
    #[error("worker unavailable: {0}")]
    WorkerUnavailable(String),
    /// The worker did not answer in time.
    #[error("worker timed out after {0} ms")]
    Timeout(u64),
    /// A read or write was rejected by the worker.
    #[error("operation rejected: {0}")]
    Rejected(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_display_without_panicking() {
        let error = ProtocolError::IncompatibleVersion {
            peer_major: 2,
            peer_minor: 0,
            local_major: 1,
            local_minor: 0,
        };
        assert!(error.to_string().contains("2.0"));
    }
}
