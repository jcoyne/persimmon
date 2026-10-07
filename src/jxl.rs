use std::{fs::File, io::Read, path::Path};

use anyhow::Context;
use image::{DynamicImage, GrayImage, RgbImage};
use jxl_oxide::{CropInfo, InitializeResult, JxlImage, PixelFormat};

use crate::iiif::Rect;

const CODESTREAM_SIGNATURE: &[u8] = b"\xff\x0a";
const CONTAINER_SIGNATURE: &[u8] = b"\0\0\0\x0cJXL \r\n\x87\n";

/// Report whether a file starts with a JPEG XL codestream or container signature.
pub fn is_jxl(path: &Path) -> anyhow::Result<bool> {
    let mut header = Vec::with_capacity(CONTAINER_SIGNATURE.len());
    File::open(path)?
        .take(CONTAINER_SIGNATURE.len() as u64)
        .read_to_end(&mut header)?;
    Ok(header.starts_with(CODESTREAM_SIGNATURE) || header.starts_with(CONTAINER_SIGNATURE))
}

fn header_error(e: Box<dyn std::error::Error + Send + Sync>) -> anyhow::Error {
    anyhow::anyhow!("read JPEG XL header: {e}")
}

/// Read width, height, and color channel count from the JPEG XL header,
/// with the image orientation applied.
pub fn metadata(path: &Path) -> anyhow::Result<(u32, u32, u16)> {
    let mut file = File::open(path)?;
    let mut uninit = JxlImage::builder().build_uninit();
    let mut buf = vec![0; 64 * 1024];
    let image = loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            anyhow::bail!("truncated JPEG XL header");
        }
        uninit.feed_bytes(&buf[..read]).map_err(header_error)?;
        match uninit.try_init().map_err(header_error)? {
            InitializeResult::NeedMoreData(more) => uninit = more,
            InitializeResult::Initialized(image) => break image,
        }
    };
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        anyhow::bail!("zero JPEG XL dimensions");
    }
    let components = if image.pixel_format().is_grayscale() {
        1
    } else {
        3
    };
    Ok((width, height, components))
}

/// Decode one region of the first frame at full resolution. JPEG XL has no
/// resolution levels to reduce, so the whole region counts against the limit.
pub fn decode_region(path: &Path, region: Rect, max_pixels: u64) -> anyhow::Result<DynamicImage> {
    if u64::from(region.width) * u64::from(region.height) > max_pixels {
        anyhow::bail!("decoded region exceeds pixel limit");
    }
    let mut image = JxlImage::builder()
        .open(path)
        .map_err(|e| anyhow::anyhow!("read JPEG XL: {e}"))?;
    image.set_image_region(CropInfo {
        width: region.width,
        height: region.height,
        left: region.x,
        top: region.y,
    });
    let gray = match image.pixel_format() {
        PixelFormat::Gray | PixelFormat::Graya => true,
        PixelFormat::Rgb | PixelFormat::Rgba => false,
        PixelFormat::Cmyk | PixelFormat::Cmyka => anyhow::bail!("CMYK JPEG XL is not supported"),
    };
    let render = image
        .render_frame(0)
        .map_err(|e| anyhow::anyhow!("decode JPEG XL: {e}"))?;
    let mut stream = render.stream_no_alpha();
    let (width, height) = (stream.width(), stream.height());
    let mut pixels = vec![0u8; width as usize * height as usize * stream.channels() as usize];
    stream.write_to_buffer(&mut pixels);
    Ok(if gray {
        DynamicImage::ImageLuma8(
            GrayImage::from_raw(width, height, pixels).context("JPEG XL buffer size")?,
        )
    } else {
        DynamicImage::ImageRgb8(
            RgbImage::from_raw(width, height, pixels).context("JPEG XL buffer size")?,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zune_core::{bit_depth::BitDepth, colorspace::ColorSpace, options::EncoderOptions};
    use zune_jpegxl::JxlSimpleEncoder;

    fn write_jxl(dir: &Path, pixels: &[u8], colorspace: ColorSpace) -> std::path::PathBuf {
        let options = EncoderOptions::new(40, 30, colorspace, BitDepth::Eight);
        let mut data = Vec::new();
        JxlSimpleEncoder::new(pixels, options)
            .encode(&mut data)
            .unwrap();
        let path = dir.join("test.jxl");
        std::fs::write(&path, data).unwrap();
        path
    }

    #[test]
    fn reads_metadata_and_decodes_a_region() {
        let dir = tempfile::tempdir().unwrap();
        let source = RgbImage::from_fn(40, 30, |x, y| image::Rgb([x as u8 * 6, y as u8 * 8, 64]));
        let path = write_jxl(dir.path(), source.as_raw(), ColorSpace::RGB);
        assert!(is_jxl(&path).unwrap());
        assert_eq!(metadata(&path).unwrap(), (40, 30, 3));
        let region = Rect {
            x: 5,
            y: 7,
            width: 20,
            height: 10,
        };
        let decoded = decode_region(&path, region, 200).unwrap();
        let expected = image::imageops::crop_imm(&source, 5, 7, 20, 10).to_image();
        assert_eq!(decoded.as_rgb8().unwrap(), &expected);
        assert!(decode_region(&path, region, 199).is_err());
    }

    #[test]
    fn decodes_grayscale() {
        let dir = tempfile::tempdir().unwrap();
        let source = GrayImage::from_fn(40, 30, |x, y| image::Luma([(x + y) as u8 * 3]));
        let path = write_jxl(dir.path(), source.as_raw(), ColorSpace::Luma);
        assert_eq!(metadata(&path).unwrap(), (40, 30, 1));
        let full = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 30,
        };
        assert_eq!(
            decode_region(&path, full, 1200)
                .unwrap()
                .as_luma8()
                .unwrap(),
            &source
        );
    }

    #[test]
    fn rejects_jp2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jp2");
        std::fs::write(&path, b"\0\0\0\x0cjP  \r\n\x87\n").unwrap();
        assert!(!is_jxl(&path).unwrap());
    }
}
