//! Plain, fixed-width types that cross the IPC boundary.
//!
//! These structs are `#[repr(C)]` and contain only fixed-width integers and
//! fixed-size byte arrays. They never contain `std::string`, `std::vector`,
//! host pointers or C++ internal layouts.

/// Number of bytes in a raw SHA-256 digest.
pub const SHA256_LEN: usize = 32;

/// Shared-memory frame slot header.
///
/// Layout is stable for a given protocol major version. Consumers must
/// validate `struct_size` before reading the rest of the header.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameHeader {
    /// Monotonic slot generation; incremented when the slot is recycled.
    pub generation: u64,
    /// Worker-local sequence number of the frame in this slot.
    pub seq: u64,
    /// State epoch the frame was produced in.
    pub epoch: u64,
    /// Slot index inside the shared-memory ring.
    pub slot: u16,
    /// `SCREEN_TOP` or `SCREEN_BOTTOM`.
    pub screen: u16,
    /// `EYE_LEFT` or `EYE_RIGHT`.
    pub eye: u16,
    /// Pixel format, e.g. `PIXEL_FORMAT_RGBA8`.
    pub format: u32,
    /// Row stride in bytes.
    pub stride: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl FrameHeader {
    /// Byte size of the header as laid out in shared memory.
    pub const STRUCT_SIZE: usize = std::mem::size_of::<FrameHeader>();

    /// Bytes of pixel data described by this header.
    pub fn pixel_bytes(&self) -> usize {
        self.stride as usize * self.height as usize
    }

    /// Whether the header describes a usable frame.
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.stride >= self.width * 4
            && matches!(self.format, PIXEL_FORMAT_RGBA8)
            && matches!(self.screen, SCREEN_TOP | SCREEN_BOTTOM)
            && matches!(self.eye, EYE_LEFT | EYE_RIGHT)
    }
}

/// Top screen.
pub const SCREEN_TOP: u16 = 0;
/// Bottom screen.
pub const SCREEN_BOTTOM: u16 = 1;
/// Left eye of a stereoscopic frame.
pub const EYE_LEFT: u16 = 0;
/// Right eye of a stereoscopic frame.
pub const EYE_RIGHT: u16 = 1;
/// 8 bit RGBA, four bytes per pixel.
pub const PIXEL_FORMAT_RGBA8: u32 = 0;

/// Handshake exchanged when a client attaches to a worker.
///
/// The worker fills this in and the client validates it before issuing any
/// other request.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerHello {
    /// Size of this struct, for forward-compatible reads.
    pub struct_size: u32,
    /// ABI version of the shared-memory layout.
    pub abi_version: u32,
    /// Protocol major version spoken by the worker.
    pub protocol_version_major: u16,
    /// Protocol minor version spoken by the worker.
    pub protocol_version_minor: u16,
    /// Raw SHA-256 of the Azahar revision the worker was built from.
    pub worker_commit: [u8; SHA256_LEN],
    /// Largest single memory read the worker accepts.
    pub max_memory_read_bytes: u64,
    /// Largest trace the worker will hold in memory.
    pub max_trace_bytes: u64,
}

impl WorkerHello {
    /// ABI version implemented by this build of the protocol crate.
    pub const ABI_VERSION: u32 = 1;

    /// Build a handshake payload for a worker at `commit_sha256`.
    pub fn new(
        commit_sha256: [u8; SHA256_LEN],
        max_memory_read_bytes: u64,
        max_trace_bytes: u64,
    ) -> Self {
        Self {
            struct_size: std::mem::size_of::<WorkerHello>() as u32,
            abi_version: Self::ABI_VERSION,
            protocol_version_major: crate::PROTOCOL_VERSION_MAJOR,
            protocol_version_minor: crate::PROTOCOL_VERSION_MINOR,
            worker_commit: commit_sha256,
            max_memory_read_bytes,
            max_trace_bytes,
        }
    }

    /// Whether the peer speaks a compatible protocol version.
    pub fn is_compatible(&self) -> bool {
        crate::is_compatible(self.protocol_version_major, self.protocol_version_minor)
    }
}

/// Bounded event record emitted by the worker into the event stream.
///
/// The payload is an opaque, versioned, worker-defined blob. Its meaning is
/// defined by the event category, never by the raw bytes alone.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventHeader {
    /// Size of this header.
    pub struct_size: u32,
    /// Event category identifier.
    pub category: u32,
    /// Worker-local event sequence number.
    pub sequence: u64,
    /// Frame the event belongs to.
    pub frame: u64,
    /// Virtual guest tick, not host time and not calibrated hardware time.
    pub guest_tick: u64,
    /// Length of the payload that follows the header.
    pub payload_len: u32,
}

impl Default for EventHeader {
    fn default() -> Self {
        Self {
            struct_size: std::mem::size_of::<EventHeader>() as u32,
            category: 0,
            sequence: 0,
            frame: 0,
            guest_tick: 0,
            payload_len: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_header_has_fixed_layout() {
        // Fixed width only: the header must not grow with any pointer width
        // or with a platform specific alignment surprise. The C worker header
        // static asserts the same size.
        assert_eq!(FrameHeader::STRUCT_SIZE, 48);
    }

    #[test]
    fn other_abi_structs_have_fixed_layout() {
        assert_eq!(std::mem::size_of::<WorkerHello>(), 64);
        assert_eq!(std::mem::size_of::<EventHeader>(), 40);
    }

    #[test]
    fn pixel_bytes_uses_stride() {
        let header = FrameHeader {
            width: 400,
            height: 240,
            stride: 400 * 4,
            ..FrameHeader::default()
        };
        assert_eq!(header.pixel_bytes(), 400 * 240 * 4);
    }

    #[test]
    fn validity_rejects_bad_geometry() {
        let mut header = FrameHeader {
            width: 320,
            height: 240,
            stride: 320 * 4,
            format: PIXEL_FORMAT_RGBA8,
            screen: SCREEN_BOTTOM,
            eye: EYE_LEFT,
            ..FrameHeader::default()
        };
        assert!(header.is_valid());

        header.stride = 4;
        assert!(!header.is_valid());

        header.stride = 320 * 4;
        header.screen = 7;
        assert!(!header.is_valid());
    }

    #[test]
    fn hello_reports_compatible_protocol() {
        let hello = WorkerHello::new([0u8; SHA256_LEN], 1024, 2048);
        assert!(hello.is_compatible());
        assert_eq!(hello.protocol_version_major, crate::PROTOCOL_VERSION_MAJOR);
        assert_eq!(hello.protocol_version_minor, crate::PROTOCOL_VERSION_MINOR);
    }
}
