use std::io;

use base64::Engine;
use lotus_session::native_session::Frame;
use rmcp::model::ContentBlock;

pub(crate) fn inline_images(mut frames: Vec<Frame>) -> io::Result<Vec<ContentBlock>> {
    frames.sort_by(|a, b| b.lease.screen.cmp(&a.lease.screen));
    let [top, bottom] = frames.as_slice() else {
        return Err(io::Error::other("expected two screen frames"));
    };
    if top.lease.screen != "top"
        || bottom.lease.screen != "bottom"
        || top.lease.epoch != bottom.lease.epoch
        || top.lease.sequence != bottom.lease.sequence
        || top.lease.height != bottom.lease.height
    {
        return Err(io::Error::other(
            "capture did not contain a matching top and bottom screen",
        ));
    }

    let mut content = Vec::with_capacity(4);
    for mut frame in frames {
        let lease = &frame.lease;
        let png = encode_png(&mut frame.bgra, lease.width, lease.height)?;
        content.push(ContentBlock::text(format!(
            "{} screen ({} × {})",
            lease.screen, lease.width, lease.height,
        )));
        content.push(ContentBlock::image(
            base64::engine::general_purpose::STANDARD.encode(png),
            "image/png",
        ));
    }
    Ok(content)
}

/// Native frame leases require tightly packed BGRA8 rows in top-left order.
pub(crate) fn encode_png(bgra: &mut [u8], width: u32, height: u32) -> io::Result<Vec<u8>> {
    if !(1..=1600).contains(&width)
        || !(1..=960).contains(&height)
        || bgra.len() != (width * height * 4) as usize
    {
        return Err(io::Error::other("invalid screenshot size"));
    }
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(io::Error::other)?;
    writer.write_image_data(bgra).map_err(io::Error::other)?;
    writer.finish().map_err(io::Error::other)?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_preserves_channel_order_alpha_and_rows() {
        let mut bgra = [0, 0, 255, 255, 255, 0, 0, 128, 0, 255, 0, 64, 1, 2, 3, 0];
        let png = encode_png(&mut bgra, 2, 2).expect("encode screenshot");
        let mut reader = png::Decoder::new(png.as_slice()).read_info().unwrap();
        let mut rgba = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut rgba).unwrap();
        assert_eq!((info.width, info.height), (2, 2));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(
            rgba,
            [255, 0, 0, 255, 0, 0, 255, 128, 0, 255, 0, 64, 3, 2, 1, 0]
        );
        assert!(encode_png(&mut bgra, 4, 2).is_err());
        assert!(encode_png(&mut [], 1601, 960).is_err());
    }
}
