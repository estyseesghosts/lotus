//! Read side of the worker's shared-memory live preview ring (worker `preview` method,
//! `ctr_worker/frames.h` `PreviewRing`). Windows only: the worker creates a pagefile-backed
//! named section and the host maps it read-only. The worker writes a slot only while
//! answering `preview`, and the host copies it before its next request to that worker
//! (the session's command lock serializes both), so a slot is never read while written.
//! Every read still checks the slot's LTRF header against the lease.
use std::io;

use serde::Deserialize;

/// Bytes of one slot's LTRF header (same layout as the capture files).
pub const HEADER: usize = 32;

#[derive(Debug, Clone, Deserialize)]
pub struct PreviewLease {
    pub screen: String,
    pub offset: usize,
    pub epoch: u64,
    pub sequence: u64,
    pub slot: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: String,
    pub orientation: String,
    pub bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PreviewReply {
    pub name: String,
    pub bytes: usize,
    pub frames: Vec<PreviewLease>,
    /// Presented frame the pixels belong to (Vulkan may return the previous request's).
    pub frame: u64,
    pub current_frame: u64,
    pub current_epoch: u64,
}

/// Checks a lease against the native screen geometry and the ring size.
pub fn validate(lease: &PreviewLease, ring_bytes: usize) -> io::Result<()> {
    let width = match lease.screen.as_str() {
        "top" => 400,
        "bottom" => 320,
        _ => 0,
    };
    let end = lease
        .offset
        .checked_add(HEADER)
        .and_then(|at| at.checked_add(lease.bytes));
    if width == 0
        || lease.width != width
        || lease.height != 240
        || lease.stride != lease.width * 4
        || lease.bytes != (lease.stride * lease.height) as usize
        || lease.format != "bgra8"
        || lease.orientation != "top_left_row_major"
        || end.is_none_or(|end| end > ring_bytes)
    {
        return Err(io::Error::other("invalid preview lease"));
    }
    Ok(())
}

/// The 32-byte header the worker writes before each screen's pixels.
#[must_use]
pub fn header(lease: &PreviewLease) -> [u8; HEADER] {
    let mut out = [0u8; HEADER];
    out[..4].copy_from_slice(b"LTRF");
    out[4..8].copy_from_slice(&1u32.to_le_bytes());
    out[8..16].copy_from_slice(&lease.epoch.to_le_bytes());
    out[16..24].copy_from_slice(&lease.sequence.to_le_bytes());
    out[24..28].copy_from_slice(&lease.width.to_le_bytes());
    out[28..32].copy_from_slice(&lease.height.to_le_bytes());
    out
}

#[cfg(windows)]
mod platform {
    use std::io;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Memory::{
        MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS,
    };

    /// A read-only view of one worker's ring.
    pub struct Mapping {
        pub name: String,
        handle: HANDLE,
        view: MEMORY_MAPPED_VIEW_ADDRESS,
        len: usize,
    }

    impl std::fmt::Debug for Mapping {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Mapping")
                .field("name", &self.name)
                .field("len", &self.len)
                .finish_non_exhaustive()
        }
    }

    // The view is read-only memory owned by this value; it is only read through `bytes`.
    unsafe impl Send for Mapping {}
    unsafe impl Sync for Mapping {}

    impl Mapping {
        pub fn open(name: &str, len: usize) -> io::Result<Self> {
            if !name.starts_with(r"Local\lotus-ctr-") {
                return Err(io::Error::other("unexpected preview mapping name"));
            }
            let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            // SAFETY: `wide` is NUL-terminated; a null handle is checked.
            let handle = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, wide.as_ptr()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: valid section handle; mapping fails if `len` exceeds the section.
            let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, len) };
            if view.Value.is_null() {
                let error = io::Error::last_os_error();
                // SAFETY: handle came from OpenFileMappingW above.
                unsafe { CloseHandle(handle) };
                return Err(error);
            }
            Ok(Self {
                name: name.to_string(),
                handle,
                view,
                len,
            })
        }

        pub fn size(&self) -> usize {
            self.len
        }

        pub fn bytes(&self, offset: usize, len: usize) -> &[u8] {
            assert!(offset.checked_add(len).is_some_and(|end| end <= self.len));
            // SAFETY: bounds checked against the mapped length; the view lives as long as self.
            unsafe { std::slice::from_raw_parts(self.view.Value.cast::<u8>().add(offset), len) }
        }
    }

    impl Drop for Mapping {
        fn drop(&mut self) {
            // SAFETY: both came from a successful open and are released once.
            unsafe {
                UnmapViewOfFile(self.view);
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::io;

    /// Workers only offer shared-memory previews on Windows.
    #[derive(Debug)]
    pub struct Mapping {
        pub name: String,
    }

    impl Mapping {
        pub fn open(_name: &str, _len: usize) -> io::Result<Self> {
            Err(io::Error::other("shared-memory preview is Windows only"))
        }
        pub fn size(&self) -> usize {
            0
        }
        pub fn bytes(&self, _offset: usize, _len: usize) -> &[u8] {
            &[]
        }
    }
}

pub use platform::Mapping;

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(screen: &str, width: u32, offset: usize) -> PreviewLease {
        PreviewLease {
            screen: screen.into(),
            offset,
            epoch: 3,
            sequence: 7,
            slot: 1,
            width,
            height: 240,
            stride: width * 4,
            format: "bgra8".into(),
            orientation: "top_left_row_major".into(),
            bytes: (width * 240 * 4) as usize,
        }
    }

    #[test]
    fn leases_must_fit_the_ring_and_native_geometry() {
        let ring = 3 * (2 * HEADER + 400 * 240 * 4 + 320 * 240 * 4);
        assert!(validate(&lease("top", 400, 0), ring).is_ok());
        assert!(validate(&lease("bottom", 320, ring - HEADER - 320 * 240 * 4), ring).is_ok());
        assert!(validate(&lease("bottom", 320, ring - 10), ring).is_err());
        assert!(validate(&lease("bottom", 320, usize::MAX - 4), ring).is_err());
        assert!(validate(&lease("top", 320, 0), ring).is_err());
        let mut scaled = lease("top", 400, 0);
        scaled.format = "rgba8".into();
        assert!(validate(&scaled, ring).is_err());
        let header = header(&lease("top", 400, 0));
        assert_eq!(&header[..4], b"LTRF");
        assert_eq!(u64::from_le_bytes(header[8..16].try_into().unwrap()), 3);
    }
}
