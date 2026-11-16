//! Native screen geometry shared by synthetic display and input tests.

/// Displayed bottom-screen rectangle in logical host pixels.
#[derive(Debug, Clone, Copy)]
pub struct TouchTransform {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

impl TouchTransform {
    /// Invert the actual displayed rectangle. Letterbox and outside clicks miss.
    #[must_use]
    pub fn map_logical(&self, x: f32, y: f32) -> Option<(u16, u16)> {
        if ![self.left, self.top, self.width, self.height, x, y]
            .iter()
            .all(|v| v.is_finite())
            || self.width <= 0.0
            || self.height <= 0.0
        {
            return None;
        }
        let x = (x - self.left) / self.width;
        let y = (y - self.top) / self.height;
        if !(0.0..1.0).contains(&x) || !(0.0..1.0).contains(&y) {
            return None;
        }
        Some(((x * 320.0).floor() as u16, (y * 240.0).floor() as u16))
    }

    /// Convert physical pointer coordinates using the window's actual DPI scale.
    #[must_use]
    pub fn map_physical(&self, x: f32, y: f32, dpi: f32) -> Option<(u16, u16)> {
        if !dpi.is_finite() || dpi <= 0.0 {
            return None;
        }
        self.map_logical(x / dpi, y / dpi)
    }
}

/// Generate normalized native-resolution BGRA pixels, as required by GPUI.
#[must_use]
pub fn pattern(width: u32, session: u8, sequence: u8) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(width as usize * 240 * 4);
    for y in 0..240 {
        for x in 0..width {
            pixels.extend_from_slice(&[x as u8 ^ sequence, y as u8, session * 60, 255]);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_corners_and_center_survive_zoom_and_dpi() {
        for zoom in [0.5, 1.0, 1.5, 2.0] {
            let transform = TouchTransform {
                left: 113.0,
                top: 71.0,
                width: 320.0 * zoom,
                height: 240.0 * zoom,
            };
            for dpi in [1.25, 2.0] {
                for (x, y) in [(0, 0), (319, 0), (0, 239), (319, 239), (160, 120)] {
                    let host_x = (transform.left + (f32::from(x) + 0.5) * zoom) * dpi;
                    let host_y = (transform.top + (f32::from(y) + 0.5) * zoom) * dpi;
                    assert_eq!(transform.map_physical(host_x, host_y, dpi), Some((x, y)));
                }
            }
            assert_eq!(transform.map_logical(112.0, 71.0), None);
            assert_eq!(
                transform.map_logical(transform.left + transform.width, 71.0),
                None
            );
        }
    }

    #[test]
    fn invalid_transforms_never_create_guest_coordinates() {
        let transform = TouchTransform {
            left: 0.0,
            top: 0.0,
            width: 320.0,
            height: 240.0,
        };
        assert_eq!(transform.map_physical(1.0, 1.0, 0.0), None);
        assert_eq!(transform.map_logical(f32::NAN, 0.0), None);
        assert_eq!(
            TouchTransform {
                width: 0.0,
                ..transform
            }
            .map_logical(0.0, 0.0),
            None
        );
    }

    #[test]
    fn pattern_preserves_native_geometry_and_changes_each_session_and_frame() {
        assert_eq!(pattern(400, 0, 0).len(), 400 * 240 * 4);
        assert_eq!(pattern(320, 0, 0).len(), 320 * 240 * 4);
        assert_ne!(pattern(320, 0, 0), pattern(320, 0, 1));
        assert_ne!(pattern(320, 0, 0), pattern(320, 1, 0));
    }
}
