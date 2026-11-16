//! Differential comparison of emulated output.
//!
//! Comparisons run on raw worker output, never on a scaled or composited
//! screenshot. Exact match and tolerance based match are reported separately:
//! "close" is not "equal".

use lotus_hardware::TimingDomain;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Number of channels in an RGBA8 frame.
pub const CHANNELS: usize = 4;

/// Errors raised while comparing frames.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DiffError {
    /// The two frames have different dimensions.
    #[error("dimension mismatch: {left:?} vs {right:?}")]
    DimensionMismatch {
        /// Dimensions of the left frame.
        left: (u32, u32),
        /// Dimensions of the right frame.
        right: (u32, u32),
    },
    /// The pixel buffer length does not match the dimensions.
    #[error("pixel buffer length {len} does not match {width}x{height} RGBA8")]
    InvalidLength {
        /// Length of the pixel buffer.
        len: usize,
        /// Frame width.
        width: u32,
        /// Frame height.
        height: u32,
    },
}

/// A raw RGBA8 frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Framebuffer {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Row-major RGBA8 pixels, four bytes per pixel.
    pub pixels: Vec<u8>,
}

impl Framebuffer {
    /// Create a frame from raw RGBA8 pixels.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, DiffError> {
        let expected = width as usize * height as usize * CHANNELS;
        if pixels.len() != expected {
            return Err(DiffError::InvalidLength {
                len: pixels.len(),
                width,
                height,
            });
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Create a frame filled with a single colour.
    pub fn filled(width: u32, height: u32, colour: [u8; CHANNELS]) -> Self {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * CHANNELS);
        for _ in 0..(width as usize * height as usize) {
            pixels.extend_from_slice(&colour);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Number of pixels in the frame.
    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// 64 bit FNV-1a hash of the pixel data.
    ///
    /// This identifies frame content, it is not a cryptographic digest.
    pub fn content_hash(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in &self.pixels {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
        }
        hash
    }
}

/// Result of comparing two frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameDiff {
    /// Whether every pixel matched exactly.
    pub exact_match: bool,
    /// Number of pixels that differ in any channel.
    pub mismatched_pixels: usize,
    /// Total number of compared pixels.
    pub total_pixels: usize,
    /// Largest absolute per channel difference.
    pub max_abs_delta: u8,
    /// Mean absolute per channel difference.
    pub mean_abs_delta: f64,
    /// Mean absolute difference per channel: R, G, B, A.
    pub per_channel_mean_abs_delta: [f64; CHANNELS],
    /// Time domain the comparison itself was measured in.
    pub measured_in: TimingDomain,
}

/// Compare two frames pixel by pixel.
///
/// The comparison runs in host time; that is reported explicitly so it is
/// never mistaken for a guest or hardware timing measurement.
pub fn compare(left: &Framebuffer, right: &Framebuffer) -> Result<FrameDiff, DiffError> {
    if left.width != right.width || left.height != right.height {
        return Err(DiffError::DimensionMismatch {
            left: (left.width, left.height),
            right: (right.width, right.height),
        });
    }

    let mut mismatched_pixels = 0usize;
    let mut max_abs_delta = 0u8;
    let mut per_channel_sum = [0u64; CHANNELS];

    for (l, r) in left
        .pixels
        .as_chunks::<CHANNELS>()
        .0
        .iter()
        .zip(right.pixels.as_chunks::<CHANNELS>().0.iter())
    {
        let mut pixel_differs = false;
        for channel in 0..CHANNELS {
            let delta = l[channel].abs_diff(r[channel]);
            if delta != 0 {
                pixel_differs = true;
                max_abs_delta = max_abs_delta.max(delta);
                per_channel_sum[channel] += delta as u64;
            }
        }
        if pixel_differs {
            mismatched_pixels += 1;
        }
    }

    let total_pixels = left.pixel_count();
    let samples = (total_pixels * CHANNELS) as f64;
    let per_channel_mean_abs_delta = [
        per_channel_sum[0] as f64 / samples,
        per_channel_sum[1] as f64 / samples,
        per_channel_sum[2] as f64 / samples,
        per_channel_sum[3] as f64 / samples,
    ];
    let total_delta: u64 = per_channel_sum.iter().sum();

    Ok(FrameDiff {
        exact_match: mismatched_pixels == 0,
        mismatched_pixels,
        total_pixels,
        max_abs_delta,
        mean_abs_delta: total_delta as f64 / samples,
        per_channel_mean_abs_delta,
        measured_in: TimingDomain::HostMillis,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_frames_match_exactly() {
        let a = Framebuffer::filled(4, 4, [10, 20, 30, 255]);
        let b = a.clone();
        let diff = compare(&a, &b).unwrap();
        assert!(diff.exact_match);
        assert_eq!(diff.mismatched_pixels, 0);
        assert_eq!(diff.max_abs_delta, 0);
        assert_eq!(diff.measured_in, TimingDomain::HostMillis);
    }

    #[test]
    fn one_changed_pixel_is_reported() {
        let a = Framebuffer::filled(4, 4, [0, 0, 0, 255]);
        let mut b = a.clone();
        b.pixels[0] = 5;
        let diff = compare(&a, &b).unwrap();
        assert!(!diff.exact_match);
        assert_eq!(diff.mismatched_pixels, 1);
        assert_eq!(diff.max_abs_delta, 5);
        assert!((diff.per_channel_mean_abs_delta[0] - 5.0 / 64.0).abs() < 1e-9);
    }

    #[test]
    fn dimension_mismatch_is_an_error() {
        let a = Framebuffer::filled(4, 4, [0, 0, 0, 255]);
        let b = Framebuffer::filled(4, 5, [0, 0, 0, 255]);
        assert!(matches!(
            compare(&a, &b),
            Err(DiffError::DimensionMismatch { .. })
        ));
    }

    #[test]
    fn invalid_length_is_rejected() {
        assert!(matches!(
            Framebuffer::new(2, 2, vec![0; 3]),
            Err(DiffError::InvalidLength { .. })
        ));
    }

    #[test]
    fn hash_changes_with_content() {
        let a = Framebuffer::filled(2, 2, [1, 2, 3, 4]);
        let mut b = a.clone();
        b.pixels[7] = 9;
        assert_ne!(a.content_hash(), b.content_hash());
    }
}
