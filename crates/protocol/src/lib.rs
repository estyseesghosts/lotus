//! Versioned worker protocol types.
//!
//! The protocol version is independent of the Azahar revision a worker was
//! built from. Every cross-process integer is fixed width and every struct
//! that crosses the IPC boundary is plain data: no `std::string`, no
//! `std::vector`, no host pointers and no `Core::System` internals.

pub mod capabilities;
pub mod envelope;
pub mod error;
pub mod ffi;

/// Major protocol version. A worker that speaks a different major version is
/// refused at handshake instead of being coerced.
pub const PROTOCOL_VERSION_MAJOR: u16 = 1;

/// Minor protocol version. Minor bumps are additive only.
pub const PROTOCOL_VERSION_MINOR: u16 = 0;

/// Lowest minor version this build can talk to.
///
/// Bumping this is a breaking change for old peers and needs a protocol
/// migration note.
pub const MIN_SUPPORTED_MINOR: u16 = 0;

/// The minimum peer version, packed the same way as [`peer_version`].
const MIN_SUPPORTED_VERSION: u32 =
    ((PROTOCOL_VERSION_MAJOR as u32) << 16) | (MIN_SUPPORTED_MINOR as u32);

/// Pack a major and minor version into a single comparable number.
const fn peer_version(major: u16, minor: u16) -> u32 {
    ((major as u32) << 16) | (minor as u32)
}

/// Human readable protocol identifier, e.g. `"1.0"`.
pub fn protocol_id() -> String {
    format!("{PROTOCOL_VERSION_MAJOR}.{PROTOCOL_VERSION_MINOR}")
}

/// Check that a peer speaking `peer_major`.`peer_minor` is compatible.
///
/// The major version must match exactly. A peer with a *higher* minor
/// version is accepted: unknown optional fields may be ignored. A peer with a
/// *lower* minor version is accepted only when this build does not require
/// any field that the peer may be missing, which is what
/// [`MIN_SUPPORTED_MINOR`] expresses.
pub fn is_compatible(peer_major: u16, peer_minor: u16) -> bool {
    peer_major == PROTOCOL_VERSION_MAJOR
        && peer_version(peer_major, peer_minor) >= MIN_SUPPORTED_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_id_is_major_minor() {
        assert_eq!(protocol_id(), "1.0");
    }

    #[test]
    fn same_major_is_compatible() {
        assert!(is_compatible(1, 0));
    }

    #[test]
    fn newer_minor_is_compatible() {
        assert!(is_compatible(1, 7));
    }

    #[test]
    fn different_major_is_incompatible() {
        assert!(!is_compatible(0, 9));
        assert!(!is_compatible(2, 0));
    }

    #[test]
    fn the_minimum_supported_minor_is_accepted() {
        assert!(is_compatible(1, MIN_SUPPORTED_MINOR));
    }
}
