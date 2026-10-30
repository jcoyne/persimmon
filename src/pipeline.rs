use std::{io::Cursor, path::Path};

use image::{
    DynamicImage, ImageFormat,
    codecs::{avif::AvifEncoder, jpeg::JpegEncoder},
    imageops::FilterType,
};
use zune_core::{bit_depth::BitDepth, colorspace::ColorSpace, options::EncoderOptions};
use zune_jpegxl::JxlSimpleEncoder;

use crate::iiif::{Format, ImageRequest, Quality, Rect};

/// Settings for the lossy output encoders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodeSettings {
    /// JPEG quality, 1 (worst) to 100 (best).
    pub jpeg_quality: u8,
    /// AVIF quality, 1 (worst) to 100 (best).
    pub avif_quality: u8,
    /// AVIF encoder speed, 1 (slowest, smallest files) to 10 (fastest).
    pub avif_speed: u8,
}

impl Default for EncodeSettings {
    fn default() -> Self {
        Self {
            jpeg_quality: 75,
            avif_quality: 80,
            avif_speed: 10,
        }
    }
}

fn transform_encode(
    mut image: DynamicImage,
    size: (u32, u32),
    request: &ImageRequest,
    settings: EncodeSettings,
) -> anyhow::Result<Vec<u8>> {
    if image.width() != size.0 || image.height() != size.1 {
        image = image.resize_exact(size.0, size.1, FilterType::Lanczos3);
    }
    image = match request.rotation {
        0 => image,
        90 => image.rotate90(),
        180 => image.rotate180(),
        270 => image.rotate270(),
        _ => anyhow::bail!("unsupported rotation"),
    };
    if request.quality == Quality::Gray {
        image = DynamicImage::ImageLuma8(image.to_luma8());
    }
    if request.format == Format::Jxl {
        let (pixels, colorspace) = if image.color().has_color() {
            (image.to_rgb8().into_raw(), ColorSpace::RGB)
        } else {
            (image.to_luma8().into_raw(), ColorSpace::Luma)
        };
        let options = EncoderOptions::new(
            image.width() as usize,
            image.height() as usize,
            colorspace,
            BitDepth::Eight,
        )
        .set_num_threads(1);
        let mut data = Vec::new();
        JxlSimpleEncoder::new(&pixels, options)
            .encode(&mut data)
            .map_err(|e| anyhow::anyhow!("JPEG XL encode failed: {e:?}"))?;
        return Ok(data);
    }
    let mut data = Cursor::new(Vec::new());
    match request.format {
        Format::Jpeg => image.write_with_encoder(JpegEncoder::new_with_quality(
            &mut data,
            settings.jpeg_quality,
        ))?,
        Format::Avif => image.write_with_encoder(AvifEncoder::new_with_speed_quality(
            &mut data,
            settings.avif_speed,
            settings.avif_quality,
        ))?,
        Format::Png => image.write_to(&mut data, ImageFormat::Png)?,
        Format::Webp => image.write_to(&mut data, ImageFormat::WebP)?,
        Format::Jxl => unreachable!(),
    }
    Ok(data.into_inner())
}

pub fn reduction_level(region: Rect, size: (u32, u32)) -> u32 {
    let ratio = (region.width / size.0).min(region.height / size.1);
    if ratio > 1 { ratio.ilog2().min(10) } else { 0 }
}

/// Size of `region` after discarding `level` resolution levels, on the
/// reduced image's own grid.
pub fn reduced_size(region: Rect, level: u32) -> (u32, u32) {
    let divisor = 1u32 << level;
    let x0 = region.x / divisor;
    let y0 = region.y / divisor;
    (
        (region.x + region.width).div_ceil(divisor) - x0,
        (region.y + region.height).div_ceil(divisor) - y0,
    )
}

/// Decode a JP2 source region with `j2k` and encode the requested output.
pub async fn render_jp2(
    source: &Path,
    region: Rect,
    size: (u32, u32),
    request: &ImageRequest,
    encoding: EncodeSettings,
    max_decode_pixels: u64,
) -> anyhow::Result<Vec<u8>> {
    let path = source.to_owned();
    let level = reduction_level(region, size);
    let request = request.clone();
    tokio::task::spawn_blocking(move || {
        let image = crate::j2k_decoder::decode_region(&path, region, level, max_decode_pixels)?;
        transform_encode(image, size, &request, encoding)
    })
    .await?
}

/// Decode a JPEG XL source region and encode the requested output.
pub async fn render_jxl(
    source: &Path,
    region: Rect,
    size: (u32, u32),
    request: &ImageRequest,
    encoding: EncodeSettings,
    max_decode_pixels: u64,
) -> anyhow::Result<Vec<u8>> {
    let path = source.to_owned();
    let request = request.clone();
    tokio::task::spawn_blocking(move || {
        let image = crate::jxl::decode_region(&path, region, max_decode_pixels)?;
        transform_encode(image, size, &request, encoding)
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iiif::{self, Route};
    use image::RgbImage;

    #[test]
    fn reduced_size_covers_the_region_on_the_reduced_grid() {
        let region = Rect {
            x: 1023,
            y: 0,
            width: 2049,
            height: 4096,
        };
        assert_eq!(reduced_size(region, 0), (2049, 4096));
        // Columns 1023..3072 become 255..768 at 1/4 scale.
        assert_eq!(reduced_size(region, 2), (513, 1024));
        assert_eq!(reduction_level(region, (512, 1024)), 2);
    }

    #[test]
    fn encodes_color_and_gray_avif() {
        let source = DynamicImage::ImageRgb8(RgbImage::from_fn(16, 16, |x, y| {
            image::Rgb([x as u8 * 8, y as u8 * 8, 64])
        }));
        for quality in ["default", "gray"] {
            let Route::Image(request) =
                iiif::parse_route(&format!("/test/full/max/0/{quality}.avif")).unwrap()
            else {
                panic!("expected image request");
            };
            let bytes = transform_encode(
                source.clone(),
                (16, 16),
                &request,
                EncodeSettings::default(),
            )
            .unwrap();
            assert_eq!(&bytes[4..12], b"ftypavif");
            assert_eq!(request.format.mime(), "image/avif");
        }
    }

    #[test]
    fn lossy_encoders_use_configured_settings() {
        let source = DynamicImage::ImageRgb8(RgbImage::from_fn(64, 64, |x, y| {
            image::Rgb([(x * y) as u8, (x * 4) as u8, (y * 4) as u8])
        }));
        for format in ["jpg", "avif"] {
            let Route::Image(request) =
                iiif::parse_route(&format!("/test/full/max/0/default.{format}")).unwrap()
            else {
                panic!("expected image request");
            };
            let encode = |quality| {
                let settings = EncodeSettings {
                    jpeg_quality: quality,
                    avif_quality: quality,
                    ..EncodeSettings::default()
                };
                transform_encode(source.clone(), (64, 64), &request, settings)
                    .unwrap()
                    .len()
            };
            assert!(encode(10) < encode(95), "{format}");
        }
    }

    #[test]
    fn encodes_color_and_gray_jxl() {
        let source = DynamicImage::ImageRgb8(RgbImage::from_fn(16, 16, |x, y| {
            image::Rgb([x as u8 * 8, y as u8 * 8, 64])
        }));
        for quality in ["default", "gray"] {
            let Route::Image(request) =
                iiif::parse_route(&format!("/test/full/max/0/{quality}.jxl")).unwrap()
            else {
                panic!("expected image request");
            };
            let bytes = transform_encode(
                source.clone(),
                (16, 16),
                &request,
                EncodeSettings::default(),
            )
            .unwrap();
            assert_eq!(&bytes[..2], b"\xff\x0a");
            assert_eq!(request.format.mime(), "image/jxl");
            let decoded = jxl_oxide::JxlImage::builder()
                .read(Cursor::new(bytes))
                .unwrap();
            assert_eq!(decoded.image_header().size.width, 16);
            assert_eq!(decoded.image_header().size.height, 16);
            let render = decoded.render_frame(0).unwrap();
            let mut stream = render.stream();
            assert_eq!(stream.channels(), if quality == "gray" { 1 } else { 3 });
            let mut pixels = vec![0; 16 * 16 * stream.channels() as usize];
            assert_eq!(stream.write_to_buffer(&mut pixels), pixels.len());
            let expected = if quality == "gray" {
                source.to_luma8().into_raw()
            } else {
                source.to_rgb8().into_raw()
            };
            assert_eq!(pixels, expected);
        }
    }

    #[tokio::test]
    async fn jp2_region_matches_crop_of_full_decode() {
        let source = Path::new("tests/fixtures/iiif-validator.jp2");
        let full = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 1000,
        };
        let Route::Image(full_request) = iiif::parse_route("/test/full/max/0/default.png").unwrap()
        else {
            panic!("expected image request");
        };
        let reference = render_jp2(
            source,
            full,
            (1000, 1000),
            &full_request,
            EncodeSettings::default(),
            1_000_000,
        )
        .await
        .unwrap();
        let reference = image::load_from_memory_with_format(&reference, ImageFormat::Png).unwrap();
        let Route::Image(cropped_request) =
            iiif::parse_route("/test/257,263,70,60/70,60/0/default.png").unwrap()
        else {
            panic!("expected image request");
        };
        let cropped = render_jp2(
            source,
            Rect {
                x: 257,
                y: 263,
                width: 70,
                height: 60,
            },
            (70, 60),
            &cropped_request,
            EncodeSettings::default(),
            1_000_000,
        )
        .await
        .unwrap();
        let cropped = image::load_from_memory_with_format(&cropped, ImageFormat::Png).unwrap();
        assert_eq!(
            cropped.to_rgb8(),
            reference.crop_imm(257, 263, 70, 60).to_rgb8(),
            "regional decoding must match a crop of the full decode"
        );
        for (tail, format) in [
            ("gray.png", ImageFormat::Png),
            ("gray.jpg", ImageFormat::Jpeg),
            ("gray.webp", ImageFormat::WebP),
            ("color.webp", ImageFormat::WebP),
        ] {
            let route = format!("/test/200,200,400,400/100,/90/{tail}");
            let Route::Image(request) = iiif::parse_route(&route).unwrap() else {
                panic!("expected image request");
            };
            let data = render_jp2(
                source,
                Rect {
                    x: 200,
                    y: 200,
                    width: 400,
                    height: 400,
                },
                (100, 100),
                &request,
                EncodeSettings::default(),
                1_000_000,
            )
            .await
            .unwrap();
            let image = image::load_from_memory_with_format(&data, format).unwrap();
            assert_eq!((image.width(), image.height()), (100, 100));
            if tail == "gray.png" {
                assert_eq!(image.color(), image::ColorType::L8);
            }
        }
    }
}
