use std::{
    ffi::{CStr, CString, c_char, c_void},
    io::Cursor,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use anyhow::Context;
use image::{DynamicImage, ImageFormat, RgbImage, imageops::FilterType};
use libloading::Library;
use tokio::process::Command;
use zune_core::{bit_depth::BitDepth, colorspace::ColorSpace, options::EncoderOptions};
use zune_jpegxl::JxlSimpleEncoder;

use crate::iiif::{Format, ImageRequest, Quality, Rect};

fn supported_kakadu_version(text: &str) -> bool {
    let mut found = false;
    for word in text.split_whitespace() {
        let Some(version) = word.strip_prefix('v') else {
            continue;
        };
        let mut parts = version.split('.');
        let (Some(Ok(major)), Some(Ok(minor))) = (
            parts.next().map(str::parse::<u32>),
            parts.next().map(str::parse::<u32>),
        ) else {
            continue;
        };
        if !parts.all(|part| !part.is_empty() && part.parse::<u32>().is_ok()) {
            continue;
        }
        found = true;
        if major < 8 || (major == 8 && minor < 6) {
            return false;
        }
    }
    found
}

#[cfg(test)]
mod version_tests {
    use super::supported_kakadu_version;

    #[test]
    fn accepts_kakadu_8_6_and_newer() {
        for version in ["v8.6", "v8.6.2", "v8.7", "v8.10.1", "v9.0"] {
            assert!(supported_kakadu_version(version), "{version}");
        }
        assert!(supported_kakadu_version(
            "Current core system version is v8.6.2\n"
        ));
    }

    #[test]
    fn rejects_older_or_missing_kakadu_versions() {
        for version in ["v8.4.1", "v8.5.9", "v7.10", "v8", "v8.6.bad", "not Kakadu"] {
            assert!(!supported_kakadu_version(version), "{version}");
        }
        assert!(!supported_kakadu_version(
            "Compiled against version v8.7\nCurrent core system version is v8.4.1"
        ));
    }
}

struct TemporaryBitmap(PathBuf);

impl Drop for TemporaryBitmap {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn transform_encode(
    mut image: DynamicImage,
    size: (u32, u32),
    request: &ImageRequest,
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
    let format = match request.format {
        Format::Jpeg => ImageFormat::Jpeg,
        Format::Png => ImageFormat::Png,
        Format::Webp => ImageFormat::WebP,
        Format::Avif => ImageFormat::Avif,
        Format::Jxl => unreachable!(),
    };
    let mut data = Cursor::new(Vec::new());
    image.write_to(&mut data, format)?;
    Ok(data.into_inner())
}

pub struct KakaduCli {
    pub executable: PathBuf,
    pub temp_dir: PathBuf,
    pub max_decode_pixels: u64,
}

impl KakaduCli {
    pub async fn verify_version(&self) -> anyhow::Result<()> {
        let output = Command::new(&self.executable)
            .arg("-version")
            .kill_on_drop(true)
            .output()
            .await?;
        let text = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() || !supported_kakadu_version(&text) {
            anyhow::bail!(
                "Kakadu 8.6 or newer kdu_expand is required: {}",
                text.trim()
            );
        }
        Ok(())
    }

    pub async fn render(
        &self,
        source: &Path,
        source_dimensions: (u32, u32),
        region: Rect,
        size: (u32, u32),
        request: &ImageRequest,
    ) -> anyhow::Result<Vec<u8>> {
        let temp = self
            .temp_dir
            .join(format!("render-{}.bmp", uuid::Uuid::new_v4()));
        let _temporary_bitmap = TemporaryBitmap(temp.clone());
        let region_arg = format!(
            "{{{:.12},{:.12}}},{{{:.12},{:.12}}}",
            region.y as f64 / source_dimensions.1 as f64,
            region.x as f64 / source_dimensions.0 as f64,
            region.height as f64 / source_dimensions.1 as f64,
            region.width as f64 / source_dimensions.0 as f64,
        );
        let ratio = (region.width / size.0).min(region.height / size.1);
        let reduce = if ratio > 1 { ratio.ilog2().min(10) } else { 0 };
        // Some JP2 codestreams have fewer decomposition levels than the
        // requested reduction. Fall back to regional full-resolution decode.
        for level in [reduce, 0]
            .into_iter()
            .take(if reduce == 0 { 1 } else { 2 })
        {
            let divisor = 1u64 << level;
            let decoded_width = u64::from(region.width).div_ceil(divisor);
            let decoded_height = u64::from(region.height).div_ceil(divisor);
            if decoded_width * decoded_height > self.max_decode_pixels {
                anyhow::bail!("decoded region exceeds pixel limit");
            }
            let mut command = Command::new(&self.executable);
            command
                .kill_on_drop(true)
                .arg("-i")
                .arg(source)
                .arg("-o")
                .arg(&temp)
                .arg("-region")
                .arg(&region_arg)
                .arg("-num_threads")
                .arg("1")
                .arg("-fprec")
                .arg("8M");
            if level > 0 {
                command.arg("-reduce").arg(level.to_string());
            }
            let result = command.output().await;
            match result {
                Ok(output) if output.status.success() => break,
                Ok(output) if level > 0 => {
                    let _ = tokio::fs::remove_file(&temp).await;
                    tracing::warn!(stderr = %String::from_utf8_lossy(&output.stderr), "Kakadu reduced decode failed; retrying without reduction");
                }
                Ok(output) => {
                    let _ = tokio::fs::remove_file(&temp).await;
                    anyhow::bail!(
                        "Kakadu decode failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                Err(e) => {
                    let _ = tokio::fs::remove_file(&temp).await;
                    return Err(e.into());
                }
            }
        }
        let path = temp.clone();
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            let image = image::open(&path).context("read Kakadu bitmap")?;
            transform_encode(image, size, &request)
        })
        .await?
    }
}

type VersionFn = unsafe extern "C" fn() -> *const c_char;
type DecodeReducedFn = unsafe extern "C" fn(
    *const c_char,
    u32,
    u32,
    u32,
    u32,
    u32,
    u64,
    *mut *mut u8,
    *mut u32,
    *mut u32,
) -> i32;
type FreeFn = unsafe extern "C" fn(*mut c_void);

struct NativeBuffer {
    pointer: *mut u8,
    free: FreeFn,
}

impl Drop for NativeBuffer {
    fn drop(&mut self) {
        unsafe { (self.free)(self.pointer.cast()) };
    }
}

#[derive(Debug)]
struct NativeFallback(i32);

impl std::fmt::Display for NativeFallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native decode requires command fallback ({})", self.0)
    }
}

impl std::error::Error for NativeFallback {}

fn reduction_level(region: Rect, size: (u32, u32)) -> u32 {
    let ratio = (region.width / size.0).min(region.height / size.1);
    if ratio > 1 { ratio.ilog2().min(10) } else { 0 }
}

pub struct KakaduNative {
    _library: Library,
    version: VersionFn,
    decode: DecodeReducedFn,
    free: FreeFn,
    max_decode_pixels: u64,
}

impl KakaduNative {
    pub fn load(path: &Path, max_decode_pixels: u64) -> anyhow::Result<Self> {
        // The function pointers remain valid because this struct owns the library.
        let library = unsafe { Library::new(path) }.context("load Kakadu adapter")?;
        let version = unsafe { *library.get::<VersionFn>(b"persimmon_kakadu_version\0")? };
        let decode = unsafe {
            *library.get::<DecodeReducedFn>(b"persimmon_kakadu_decode_reduced_region\0")?
        };
        let free = unsafe { *library.get::<FreeFn>(b"persimmon_kakadu_free\0")? };
        Ok(Self {
            _library: library,
            version,
            decode,
            free,
            max_decode_pixels,
        })
    }

    pub fn version(&self) -> anyhow::Result<String> {
        let value = unsafe { (self.version)() };
        if value.is_null() {
            anyhow::bail!("Kakadu adapter returned no version");
        }
        Ok(unsafe { CStr::from_ptr(value) }.to_str()?.to_owned())
    }

    pub fn verify_version(&self) -> anyhow::Result<()> {
        let version = self.version()?;
        if !supported_kakadu_version(&version) {
            anyhow::bail!("Kakadu 8.6 or newer native adapter is required: {version}");
        }
        Ok(())
    }

    pub async fn render(
        &self,
        source: &Path,
        region: Rect,
        size: (u32, u32),
        request: &ImageRequest,
    ) -> anyhow::Result<Vec<u8>> {
        let path = CString::new(source.as_os_str().as_bytes())?;
        let decode = self.decode;
        let free = self.free;
        let level = reduction_level(region, size);
        let max_pixels = self.max_decode_pixels;
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            let mut pointer = std::ptr::null_mut();
            let mut width = 0;
            let mut height = 0;
            let status = unsafe {
                decode(
                    path.as_ptr(),
                    region.x,
                    region.y,
                    region.width,
                    region.height,
                    level,
                    max_pixels,
                    &mut pointer,
                    &mut width,
                    &mut height,
                )
            };
            if matches!(status, 4 | 6 | 7) {
                return Err(NativeFallback(status).into());
            }
            if status != 0 {
                anyhow::bail!("Kakadu native decode failed with status {status}");
            }
            let buffer = NativeBuffer { pointer, free };
            let pixels = u64::from(width) * u64::from(height);
            if pointer.is_null() || pixels == 0 || pixels > max_pixels {
                anyhow::bail!("Kakadu native decoder returned invalid dimensions or buffer");
            }
            let length =
                usize::try_from(pixels.checked_mul(3).context("RGB buffer size overflow")?)?;
            let rgb = unsafe { std::slice::from_raw_parts(buffer.pointer, length).to_vec() };
            let image = RgbImage::from_raw(width, height, rgb)
                .context("native RGB buffer has invalid dimensions")?;
            transform_encode(DynamicImage::ImageRgb8(image), size, &request)
        })
        .await?
    }
}

pub enum KakaduBackend {
    Command(KakaduCli),
    Native {
        native: KakaduNative,
        fallback: KakaduCli,
    },
}

pub enum RenderPath {
    Native,
    Command,
    CommandFallback,
}

pub struct RenderResult {
    pub bytes: Vec<u8>,
    pub path: RenderPath,
}

impl KakaduBackend {
    pub async fn verify_version(&self) -> anyhow::Result<()> {
        match self {
            Self::Command(command) => command.verify_version().await,
            Self::Native { native, fallback } => {
                native.verify_version()?;
                fallback.verify_version().await
            }
        }
    }

    pub async fn render(
        &self,
        source: &Path,
        source_dimensions: (u32, u32),
        region: Rect,
        size: (u32, u32),
        request: &ImageRequest,
    ) -> anyhow::Result<RenderResult> {
        match self {
            Self::Command(command) => {
                let bytes = command
                    .render(source, source_dimensions, region, size, request)
                    .await?;
                Ok(RenderResult {
                    bytes,
                    path: RenderPath::Command,
                })
            }
            Self::Native { native, fallback } => {
                if reduction_level(region, size) == 0
                    && u64::from(region.width) * u64::from(region.height) > native.max_decode_pixels
                {
                    let bytes = fallback
                        .render(source, source_dimensions, region, size, request)
                        .await?;
                    Ok(RenderResult {
                        bytes,
                        path: RenderPath::CommandFallback,
                    })
                } else {
                    match native.render(source, region, size, request).await {
                        Err(e) if e.is::<NativeFallback>() => {
                            let bytes = fallback
                                .render(source, source_dimensions, region, size, request)
                                .await?;
                            Ok(RenderResult {
                                bytes,
                                path: RenderPath::CommandFallback,
                            })
                        }
                        Ok(bytes) => Ok(RenderResult {
                            bytes,
                            path: RenderPath::Native,
                        }),
                        Err(e) => Err(e),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iiif::{self, Route};
    use std::sync::Arc;

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
            let bytes = transform_encode(source.clone(), (16, 16), &request).unwrap();
            assert_eq!(&bytes[4..12], b"ftypavif");
            assert_eq!(request.format.mime(), "image/avif");
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
            let bytes = transform_encode(source.clone(), (16, 16), &request).unwrap();
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
    #[ignore = "requires local Kakadu and a JP2 fixture"]
    async fn decodes_cropped_region_and_encodes_all_formats() {
        let source = PathBuf::from(std::env::var("PERSIMMON_TEST_JP2").unwrap());
        assert_eq!(crate::jp2::metadata(&source).unwrap().0, 16);
        assert_eq!(crate::jp2::metadata(&source).unwrap().1, 16);
        let executable = PathBuf::from(std::env::var("PERSIMMON_TEST_KDU_EXPAND").unwrap());
        let temp = tempfile::tempdir().unwrap();
        let kakadu = KakaduCli {
            executable,
            temp_dir: temp.path().into(),
            max_decode_pixels: 1_000_000,
        };
        let full_bmp = temp.path().join("full.bmp");
        let full = Command::new(&kakadu.executable)
            .arg("-i")
            .arg(&source)
            .arg("-o")
            .arg(&full_bmp)
            .output()
            .await
            .unwrap();
        assert!(full.status.success());
        let reference = image::open(full_bmp).unwrap();
        let Route::Image(cropped_request) =
            iiif::parse_route("/test/3,5,7,6/7,6/0/default.png").unwrap()
        else {
            panic!("expected image request");
        };
        let cropped = kakadu
            .render(
                &source,
                (16, 16),
                Rect {
                    x: 3,
                    y: 5,
                    width: 7,
                    height: 6,
                },
                (7, 6),
                &cropped_request,
            )
            .await
            .unwrap();
        let cropped = image::load_from_memory_with_format(&cropped, ImageFormat::Png).unwrap();
        assert_eq!(
            cropped.to_rgb8(),
            reference.crop_imm(3, 5, 7, 6).to_rgb8(),
            "regional decoding must match a crop of the full decode"
        );
        for (tail, format) in [
            ("gray.png", ImageFormat::Png),
            ("gray.jpg", ImageFormat::Jpeg),
            ("gray.webp", ImageFormat::WebP),
            ("color.webp", ImageFormat::WebP),
        ] {
            let route = format!("/test/4,4,8,8/4,/90/{tail}");
            let Route::Image(request) = iiif::parse_route(&route).unwrap() else {
                panic!("expected image request");
            };
            let data = kakadu
                .render(
                    &source,
                    (16, 16),
                    Rect {
                        x: 4,
                        y: 4,
                        width: 8,
                        height: 8,
                    },
                    (4, 4),
                    &request,
                )
                .await
                .unwrap();
            let image = image::load_from_memory_with_format(&data, format).unwrap();
            assert_eq!((image.width(), image.height()), (4, 4));
            if tail == "gray.png" {
                assert_eq!(image.color(), image::ColorType::L8);
            }
        }
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    #[ignore = "requires a locally built Kakadu adapter and JP2 fixture"]
    async fn native_region_matches_command_output_under_concurrency() {
        let source = PathBuf::from(std::env::var("PERSIMMON_TEST_JP2").unwrap());
        let executable = PathBuf::from(std::env::var("PERSIMMON_TEST_KDU_EXPAND").unwrap());
        let native_library = PathBuf::from(std::env::var("PERSIMMON_TEST_NATIVE_LIB").unwrap());
        let temp = tempfile::tempdir().unwrap();
        let command = Arc::new(KakaduCli {
            executable,
            temp_dir: temp.path().into(),
            max_decode_pixels: 1_000_000,
        });
        let native = Arc::new(KakaduNative::load(&native_library, 1_000_000).unwrap());
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let source = source.clone();
            let command = command.clone();
            let native = native.clone();
            tasks.push(tokio::spawn(async move {
                let Route::Image(request) =
                    iiif::parse_route("/test/3,5,7,6/7,6/0/default.png").unwrap()
                else {
                    panic!("expected image request");
                };
                let region = Rect {
                    x: 3,
                    y: 5,
                    width: 7,
                    height: 6,
                };
                let expected = command
                    .render(&source, (16, 16), region, (7, 6), &request)
                    .await
                    .unwrap();
                let actual = native
                    .render(&source, region, (7, 6), &request)
                    .await
                    .unwrap();
                let expected = image::load_from_memory_with_format(&expected, ImageFormat::Png)
                    .unwrap()
                    .to_rgb8();
                let actual = image::load_from_memory_with_format(&actual, ImageFormat::Png)
                    .unwrap()
                    .to_rgb8();
                assert_eq!(actual, expected);
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        for (region, size, path) in [
            (
                Rect {
                    x: 0,
                    y: 0,
                    width: 16,
                    height: 16,
                },
                (4, 4),
                "/test/full/4,4/0/default.png",
            ),
            (
                Rect {
                    x: 3,
                    y: 5,
                    width: 7,
                    height: 6,
                },
                (3, 3),
                "/test/3,5,7,6/3,3/0/default.png",
            ),
        ] {
            let Route::Image(request) = iiif::parse_route(path).unwrap() else {
                panic!("expected image request");
            };
            let expected = command
                .render(&source, (16, 16), region, size, &request)
                .await
                .unwrap();
            let actual = native
                .render(&source, region, size, &request)
                .await
                .unwrap();
            let expected = image::load_from_memory_with_format(&expected, ImageFormat::Png)
                .unwrap()
                .to_rgb8();
            let actual = image::load_from_memory_with_format(&actual, ImageFormat::Png)
                .unwrap()
                .to_rgb8();
            assert_eq!(actual, expected, "reduced decode differs for {path}");
        }
    }

    #[tokio::test]
    #[ignore = "requires local Kakadu adapter, command, and the IIIF validator JP2 fixture"]
    async fn native_reduction_matches_command_for_large_regions() {
        let source = PathBuf::from("tests/fixtures/iiif-validator.jp2");
        let executable = PathBuf::from(std::env::var("PERSIMMON_TEST_KDU_EXPAND").unwrap());
        let native_library = PathBuf::from(std::env::var("PERSIMMON_TEST_NATIVE_LIB").unwrap());
        let temp = tempfile::tempdir().unwrap();
        let command = KakaduCli {
            executable,
            temp_dir: temp.path().into(),
            max_decode_pixels: 2_000_000,
        };
        let native = KakaduNative::load(&native_library, 2_000_000).unwrap();
        for (region, size) in [
            (
                Rect {
                    x: 0,
                    y: 0,
                    width: 1000,
                    height: 1000,
                },
                (125, 125),
            ),
            (
                Rect {
                    x: 257,
                    y: 263,
                    width: 512,
                    height: 512,
                },
                (256, 256),
            ),
        ] {
            let request = ImageRequest {
                identifier: "validator".into(),
                region: crate::iiif::Region::Pixels(
                    region.x,
                    region.y,
                    region.width,
                    region.height,
                ),
                size: crate::iiif::Size::Exact(size.0, size.1),
                rotation: 0,
                quality: Quality::Default,
                format: Format::Png,
            };
            let expected = command
                .render(&source, (1000, 1000), region, size, &request)
                .await
                .unwrap();
            let actual = native
                .render(&source, region, size, &request)
                .await
                .unwrap();
            let expected = image::load_from_memory_with_format(&expected, ImageFormat::Png)
                .unwrap()
                .to_rgb8();
            let actual = image::load_from_memory_with_format(&actual, ImageFormat::Png)
                .unwrap()
                .to_rgb8();
            assert_eq!(actual, expected, "reduced decode differs for {region:?}");
        }
    }
}
