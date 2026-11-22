use gpui::RenderImage;
use lotus_session::native_session::Frame;
use std::sync::Arc;

/// Install only a complete, native-resolution pair from one worker frame.
pub(crate) fn make_pair(frames: Vec<Frame>) -> Option<Vec<Arc<RenderImage>>> {
    let [top, bottom]: [Frame; 2] = frames.try_into().ok()?;
    if (top.lease.epoch, top.lease.sequence) != (bottom.lease.epoch, bottom.lease.sequence) {
        return None;
    }
    Some(vec![
        make_image(top, "top", 400)?,
        make_image(bottom, "bottom", 320)?,
    ])
}

fn make_image(frame: Frame, screen: &str, width: u32) -> Option<Arc<RenderImage>> {
    let lease = frame.lease;
    if lease.screen != screen
        || lease.format != "bgra8"
        || lease.orientation != "top_left_row_major"
        || lease.width != width
        || lease.height != 240
        || lease.stride != width * 4
        || lease.bytes != (lease.stride * lease.height) as usize
        || frame.bgra.len() != lease.bytes
    {
        return None;
    }
    // Pinned GPUI RenderImage stores BGRA, and the Windows atlas uploads it as
    // DXGI_FORMAT_B8G8R8A8_UNORM (gpui/assets.rs, gpui_windows/directx_atlas.rs).
    // image::Frame uses an RgbaImage container, but from_raw only wraps the Vec.
    let storage = image::RgbaImage::from_raw(lease.width, lease.height, frame.bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(storage)])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lotus_session::native_session::FrameLease;

    fn frame(screen: &str, width: u32) -> Frame {
        let stride = width * 4;
        let bytes = (stride * 240) as usize;
        let mut bgra = vec![0; bytes];
        bgra[..8].copy_from_slice(&[0, 0, 255, 255, 255, 0, 0, 255]);
        let row = stride as usize;
        bgra[row..row + 8].copy_from_slice(&[0, 255, 0, 128, 33, 22, 11, 0]);
        Frame {
            lease: FrameLease {
                screen: screen.into(),
                path: String::new(),
                epoch: 4,
                sequence: 8,
                slot: 0,
                width,
                height: 240,
                stride,
                format: "bgra8".into(),
                orientation: "top_left_row_major".into(),
                bytes,
            },
            bgra,
        }
    }

    #[test]
    fn render_image_keeps_native_rows_channels_and_alpha_without_copying_pixels() {
        let frames = vec![frame("top", 400), frame("bottom", 320)];
        let original: Vec<_> = frames
            .iter()
            .map(|frame| (frame.bgra.as_ptr(), frame.bgra.clone()))
            .collect();
        let images = make_pair(frames).unwrap();
        for (image, (pointer, expected)) in images.iter().zip(original) {
            let bytes = image.as_bytes(0).unwrap();
            assert_eq!(bytes, expected);
            assert_eq!(bytes.as_ptr(), pointer);
        }
    }

    #[test]
    fn invalid_format_stride_orientation_and_epoch_refuse_the_pair() {
        let mut bottom = frame("bottom", 320);
        bottom.lease.format = "rgba8".into();
        assert!(make_pair(vec![frame("top", 400), bottom]).is_none());
        let mut bottom = frame("bottom", 320);
        bottom.lease.stride += 4;
        assert!(make_pair(vec![frame("top", 400), bottom]).is_none());
        let mut bottom = frame("bottom", 320);
        bottom.lease.orientation = "bottom_left_row_major".into();
        assert!(make_pair(vec![frame("top", 400), bottom]).is_none());
        let mut bottom = frame("bottom", 320);
        bottom.lease.epoch += 1;
        assert!(make_pair(vec![frame("top", 400), bottom]).is_none());
    }
}
