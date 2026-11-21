// Copyright 2017–2026 Citra Emulator Project / Azahar Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Decode a texture copied at a PICA draw boundary. Layout and format IDs
//! follow the worker's video_core/texture/texture_decode.cpp, etc1.cpp and
//! pica/regs_texturing.h. Pixels are BGRA for GPUI, like worker Surface replies.

use lotus_session::supervisor::DebugCancellation;
use serde_json::Value;
use std::io;

const MAX_DIMENSION: usize = 1024;

#[derive(Clone, Copy)]
enum Format {
    Rgba8,
    Rgb8,
    Rgb5A1,
    Rgb565,
    Rgba4,
    Ia8,
    Rg8,
    I8,
    A8,
    Ia4,
    I4,
    A4,
    Etc1,
    Etc1A4,
}

impl Format {
    const ALL: [Self; 14] = [
        Self::Rgba8,
        Self::Rgb8,
        Self::Rgb5A1,
        Self::Rgb565,
        Self::Rgba4,
        Self::Ia8,
        Self::Rg8,
        Self::I8,
        Self::A8,
        Self::Ia4,
        Self::I4,
        Self::A4,
        Self::Etc1,
        Self::Etc1A4,
    ];

    fn tile_size(self) -> usize {
        match self {
            Self::Rgba8 => 256,
            Self::Rgb8 => 192,
            Self::Rgb5A1 | Self::Rgb565 | Self::Rgba4 | Self::Ia8 | Self::Rg8 => 128,
            Self::I8 | Self::A8 | Self::Ia4 | Self::Etc1A4 => 64,
            Self::I4 | Self::A4 | Self::Etc1 => 32,
        }
    }

    fn pixel(self, tile: &[u8], x: usize, y: usize) -> [u8; 4] {
        let i = (x & 1)
            | ((y & 1) << 1)
            | ((x & 2) << 1)
            | ((y & 2) << 2)
            | ((x & 4) << 2)
            | ((y & 4) << 3);
        let [r, g, b, a] = match self {
            Self::Rgba8 => [
                tile[i * 4 + 3],
                tile[i * 4 + 2],
                tile[i * 4 + 1],
                tile[i * 4],
            ],
            Self::Rgb8 => [tile[i * 3 + 2], tile[i * 3 + 1], tile[i * 3], 255],
            Self::Rgb5A1 | Self::Rgb565 | Self::Rgba4 => {
                let word = u16::from_le_bytes([tile[i * 2], tile[i * 2 + 1]]);
                let field = |shift, mask: u16| ((word >> shift) & mask) as u8;
                match self {
                    Self::Rgb5A1 => [
                        five(field(11, 31)),
                        five(field(6, 31)),
                        five(field(1, 31)),
                        field(0, 1) * 255,
                    ],
                    Self::Rgb565 => {
                        let green = field(5, 63);
                        [
                            five(field(11, 31)),
                            (green << 2) | (green >> 4),
                            five(field(0, 31)),
                            255,
                        ]
                    }
                    _ => [
                        four(field(12, 15)),
                        four(field(8, 15)),
                        four(field(4, 15)),
                        four(field(0, 15)),
                    ],
                }
            }
            Self::Ia8 => [
                tile[i * 2 + 1],
                tile[i * 2 + 1],
                tile[i * 2 + 1],
                tile[i * 2],
            ],
            Self::Rg8 => [tile[i * 2 + 1], tile[i * 2], 0, 255],
            Self::I8 => [tile[i], tile[i], tile[i], 255],
            Self::A8 => [0, 0, 0, tile[i]],
            Self::Ia4 => {
                let intensity = four(tile[i] >> 4);
                [intensity, intensity, intensity, four(tile[i] & 15)]
            }
            Self::I4 | Self::A4 => {
                let component = four((tile[i / 2] >> (4 * (i % 2))) & 15);
                if matches!(self, Self::I4) {
                    [component, component, component, 255]
                } else {
                    [0, 0, 0, component]
                }
            }
            Self::Etc1 | Self::Etc1A4 => etc1(tile, x, y, matches!(self, Self::Etc1A4)),
        };
        [b, g, r, a]
    }
}

fn four(value: u8) -> u8 {
    (value << 4) | value
}

fn five(value: u8) -> u8 {
    (value << 3) | (value >> 2)
}

fn etc1(tile: &[u8], x: usize, y: usize, alpha: bool) -> [u8; 4] {
    const MODIFIERS: [[i16; 2]; 8] = [
        [2, 8],
        [5, 17],
        [9, 29],
        [13, 42],
        [18, 60],
        [24, 80],
        [33, 106],
        [47, 183],
    ];
    let subtile = (x / 4 + 2 * (y / 4)) * if alpha { 16 } else { 8 };
    let x = x % 4;
    let y = y % 4;
    let texel = x * 4 + y;
    let alpha_value = if alpha {
        let packed = u64::from_le_bytes(tile[subtile..subtile + 8].try_into().unwrap());
        four(((packed >> (4 * texel)) & 15) as u8)
    } else {
        255
    };
    let offset = subtile + usize::from(alpha) * 8;
    let block = u64::from_le_bytes(tile[offset..offset + 8].try_into().unwrap());
    let second = if block & (1 << 32) != 0 {
        y >= 2
    } else {
        x >= 2
    };
    let table = ((block >> if second { 34 } else { 37 }) & 7) as usize;
    let code = ((block >> texel) & 1) as usize;
    let sign = if block & (1 << (texel + 16)) != 0 {
        -1
    } else {
        1
    };
    let adjust = |shift| {
        let packed = ((block >> shift) & 255_u64) as u8;
        let base = if block & (1 << 33) != 0 {
            let delta = ((packed as i8) << 5) >> 5;
            let value = (packed >> 3) as i8 + if second { delta } else { 0 };
            five((value as u8) & 31)
        } else {
            four(if second { packed & 15 } else { packed >> 4 })
        };
        (i16::from(base) + sign * MODIFIERS[table][code]).clamp(0, 255) as u8
    };
    [adjust(56), adjust(48), adjust(40), alpha_value]
}

#[derive(Clone, Copy)]
pub(crate) struct Layout {
    width: usize,
    height: usize,
    format: Format,
}

impl Layout {
    pub(crate) fn from_resource(resource: &Value) -> io::Result<Self> {
        if resource["kind"] != "texture" {
            return Err(io::Error::other("Captured resource has no texture layout"));
        }
        let dimension = |key| {
            resource[key]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| (1..=MAX_DIMENSION).contains(n))
                .ok_or_else(|| io::Error::other("Missing or unsupported texture dimensions"))
        };
        let format = resource["format"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .and_then(|n| Format::ALL.get(n))
            .copied()
            .ok_or_else(|| io::Error::other("Unknown PICA texture format"))?;
        Ok(Self {
            width: dimension("width")?,
            height: dimension("height")?,
            format,
        })
    }

    pub(crate) fn checked_hex(self, resource: &Value) -> io::Result<&str> {
        let expected = self.width.div_ceil(8) * self.height.div_ceil(8) * self.format.tile_size();
        let hex = resource["data_hex"]
            .as_str()
            .ok_or_else(|| io::Error::other("Saved texture bytes unavailable"))?;
        if resource["bytes"].as_u64() != Some(expected as u64) || hex.len() != 2 * expected {
            return Err(io::Error::other(
                "Saved texture bytes are incomplete or have an invalid length",
            ));
        }
        Ok(hex)
    }

    pub(crate) fn decode(
        self,
        resource: &Value,
        cancellation: &DebugCancellation,
    ) -> io::Result<image::RgbaImage> {
        let hex = self.checked_hex(resource)?.as_bytes();
        let mut source = Vec::with_capacity(hex.len() / 2);
        let nibble = |byte| match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            _ => Err(io::Error::other("Invalid hex in saved texture bytes")),
        };
        for block in hex.chunks(64 * 1024) {
            cancellation.check()?;
            for &[high, low] in block.as_chunks::<2>().0 {
                source.push((nibble(high)? << 4) | nibble(low)?);
            }
        }
        let mut pixels = image::RgbaImage::new(self.width as u32, self.height as u32);
        let columns = self.width.div_ceil(8);
        let tile_size = self.format.tile_size();
        for (y, row) in pixels.rows_mut().enumerate() {
            cancellation.check()?;
            let tile_row = (y / 8) * columns * tile_size;
            for (x, pixel) in row.enumerate() {
                let offset = tile_row + (x / 8) * tile_size;
                pixel.0 = self
                    .format
                    .pixel(&source[offset..offset + tile_size], x % 8, y % 8);
            }
        }
        Ok(pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_morton_tiles_and_rounds_up_partial_tiles() {
        let mut resource = serde_json::json!({
            "kind": "texture", "format": 1, "width": 9, "height": 2, "bytes": 384,
            "data_hex": format!("{}010203{}040506{}070809{}",
                "00".repeat(6), "00".repeat(3), "00".repeat(177), "00".repeat(189))
        });
        let layout = Layout::from_resource(&resource).unwrap();
        let image = layout
            .decode(&resource, &DebugCancellation::default())
            .unwrap();
        assert_eq!(image.get_pixel(0, 1).0, [1, 2, 3, 255]);
        assert_eq!(image.get_pixel(1, 1).0, [0, 0, 0, 255]);
        assert_eq!(image.get_pixel(2, 0).0, [4, 5, 6, 255]);
        assert_eq!(image.get_pixel(8, 0).0, [7, 8, 9, 255]);
        resource["data_hex"] = "1234".into();
        assert!(layout
            .decode(&resource, &DebugCancellation::default())
            .is_err());
        resource["width"] = 1025.into();
        assert!(Layout::from_resource(&resource).is_err());
    }

    #[test]
    fn decodes_color_alpha_and_pica_ordered_etc_subtiles() {
        assert_eq!(
            Format::Rgba8.pixel(&[17, 34, 51, 68], 0, 0),
            [34, 51, 68, 17]
        );
        assert_eq!(
            Format::Rgb565.pixel(&[0x1f, 0xf8], 0, 0),
            [255, 0, 255, 255]
        );
        assert_eq!(Format::A4.pixel(&[0x73], 1, 0), [0, 0, 0, 119]);
        let mut tile = [0_u8; 64];
        tile[32..40].copy_from_slice(&(7_u64 << 24).to_le_bytes());
        tile[40..48].copy_from_slice(&(0xf1_u64 << 56 | 1 << 6).to_le_bytes());
        assert_eq!(Format::Etc1A4.pixel(&tile, 1, 6), [8, 8, 255, 119]);
        assert_eq!(Format::Etc1A4.pixel(&tile, 3, 6), [2, 2, 19, 0]);
    }
}
