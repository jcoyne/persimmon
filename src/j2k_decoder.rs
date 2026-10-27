use std::{fs::File, path::Path};

use anyhow::Context;
use image::{DynamicImage, GrayImage, RgbImage};
use j2k::{CpuDecodeParallelism, J2kDecoder, J2kScratchPool, PixelFormat};

use crate::iiif::Rect;

/// Decode one region of a JP2 with the pure-Rust `j2k` crate, discarding
/// `level` resolution levels as Kakadu's `-reduce` does. The source is memory
/// mapped so only the codestream bytes the region needs are read from disk.
pub fn decode_region(
    path: &Path,
    region: Rect,
    level: u32,
    max_pixels: u64,
) -> anyhow::Result<DynamicImage> {
    let file = File::open(path)?;
    // The source cache holds a lease on this file for the request, and
    // purges replace sources under a new path rather than rewriting them.
    let bytes = unsafe { memmap2::Mmap::map(&file) }.context("map JP2 source")?;
    let mut decoder = J2kDecoder::new(&bytes).context("read JP2 with j2k")?;
    // Match Kakadu's one thread per request; PERSIMMON_MAX_PARALLEL_DECODES
    // sets concurrency across requests.
    decoder.set_cpu_decode_parallelism(CpuDecodeParallelism::Serial);
    let info = decoder.info().clone();
    let gray = info.components < 3;
    let format = if gray {
        PixelFormat::Gray8
    } else {
        PixelFormat::Rgb8
    };
    let roi = j2k::Rect {
        x: region.x,
        y: region.y,
        w: region.width,
        h: region.height,
    };
    // Codestreams with fewer decomposition levels than requested still
    // decode at their lowest resolution; the resize makes up the rest.
    let available = u32::from(info.resolution_levels.saturating_sub(1));
    let level = level.min(available);
    let attempt = |decoder: &mut J2kDecoder, level: u32| {
        let divisor = 1u32 << level;
        let x0 = roi.x / divisor;
        let y0 = roi.y / divisor;
        let width = (roi.x + roi.w).div_ceil(divisor) - x0;
        let height = (roi.y + roi.h).div_ceil(divisor) - y0;
        if u64::from(width) * u64::from(height) > max_pixels {
            anyhow::bail!("decoded region exceeds pixel limit");
        }
        let stride = width as usize * format.bytes_per_pixel();
        let mut pixels = vec![0u8; stride * height as usize];
        let outcome = decoder.decode_region_scaled_pow2_into(
            &mut J2kScratchPool::new(),
            &mut pixels,
            stride,
            format,
            roi,
            level as u8,
        )?;
        anyhow::ensure!(
            (outcome.decoded.w, outcome.decoded.h) == (width, height),
            "j2k decoded an unexpected region size"
        );
        Ok((width, height, pixels))
    };
    // Tile or component coding styles can still have fewer levels than the
    // main header. Fall back to full resolution like the Kakadu command.
    let (width, height, pixels) = match attempt(&mut decoder, level) {
        Err(e) if level > 0 => {
            tracing::warn!(error = %format!("{e:#}"), "j2k reduced decode failed; retrying without reduction");
            attempt(&mut decoder, 0)?
        }
        result => result?,
    };
    Ok(if gray {
        DynamicImage::ImageLuma8(
            GrayImage::from_raw(width, height, pixels).context("j2k buffer size")?,
        )
    } else {
        DynamicImage::ImageRgb8(
            RgbImage::from_raw(width, height, pixels).context("j2k buffer size")?,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "tests/fixtures/iiif-validator.jp2";

    #[test]
    fn decodes_full_and_reduced_regions() {
        let path = Path::new(FIXTURE);
        let region = Rect {
            x: 257,
            y: 263,
            width: 512,
            height: 512,
        };
        let full = decode_region(path, region, 0, 1_000_000).unwrap();
        assert_eq!((full.width(), full.height()), (512, 512));
        let reduced = decode_region(path, region, 1, 1_000_000).unwrap();
        // The reduced region covers x 128..385 and y 131..388 at half resolution.
        assert_eq!((reduced.width(), reduced.height()), (257, 257));
        assert!(decode_region(path, region, 0, 512 * 512 - 1).is_err());
    }

    #[test]
    fn clamps_reduction_to_available_levels() {
        let path = Path::new(FIXTURE);
        let full = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 1000,
        };
        // The fixture has four decomposition levels, so ten clamps to four.
        let image = decode_region(path, full, 10, 1_000_000).unwrap();
        assert_eq!((image.width(), image.height()), (63, 63));
    }
}
