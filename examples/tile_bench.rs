//! Compare JP2 tile rendering between the Kakadu native adapter and the
//! pure-Rust j2k decoder, using Persimmon's own render path (decode, resize,
//! and JPEG encode) and the tile grid that `info.json` advertises.
//!
//! ```sh
//! cargo run --release --features j2k --example tile_bench -- \
//!   --jp2 image.jp2 --native-lib /usr/local/lib/libpersimmon_kakadu.so \
//!   --decoder kakadu --tile 1024 --concurrency 8
//! ```
//!
//! `--decoder compare` renders every tile with both decoders as PNG and
//! reports how far the j2k pixels are from Kakadu's.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use persimmon::{
    iiif::{Format, ImageRequest, Quality, Rect, Region, Size},
    jp2,
    pipeline::{EncodeSettings, Jp2Backend, KakaduCli, KakaduNative},
};

struct Args {
    jp2: PathBuf,
    native_lib: Option<PathBuf>,
    decoder: String,
    tile: u32,
    concurrency: usize,
    rounds: usize,
    format: Format,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = Args {
        jp2: PathBuf::new(),
        native_lib: None,
        decoder: "j2k".into(),
        tile: 1024,
        concurrency: 1,
        rounds: 1,
        format: Format::Jpeg,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        let mut value = || {
            iter.next()
                .ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--jp2" => args.jp2 = value()?.into(),
            "--native-lib" => args.native_lib = Some(value()?.into()),
            "--decoder" => args.decoder = value()?,
            "--tile" => args.tile = value()?.parse()?,
            "--concurrency" => args.concurrency = value()?.parse()?,
            "--rounds" => args.rounds = value()?.parse()?,
            "--format" => {
                args.format = match value()?.as_str() {
                    "jpg" => Format::Jpeg,
                    "png" => Format::Png,
                    "webp" => Format::Webp,
                    other => anyhow::bail!("unsupported format {other}"),
                }
            }
            _ => anyhow::bail!("unknown argument {flag}"),
        }
    }
    Ok(args)
}

/// The tiles a IIIF viewer requests: every grid cell at every advertised
/// scale factor, as `/x,y,w,h/{tile/s},/0/default.{format}`.
fn tile_requests(
    width: u32,
    height: u32,
    tile: u32,
    format: Format,
) -> Vec<(u32, Rect, (u32, u32), ImageRequest)> {
    let mut requests = Vec::new();
    let mut factor = 1u32;
    loop {
        let span = tile * factor;
        for y in (0..height).step_by(span as usize) {
            for x in (0..width).step_by(span as usize) {
                let region = Rect {
                    x,
                    y,
                    width: span.min(width - x),
                    height: span.min(height - y),
                };
                let size = (
                    region.width.div_ceil(factor).max(1),
                    region.height.div_ceil(factor).max(1),
                );
                let request = ImageRequest {
                    identifier: "bench".into(),
                    region: Region::Pixels(x, y, region.width, region.height),
                    size: Size::Width(size.0),
                    rotation: 0,
                    quality: Quality::Default,
                    format,
                };
                requests.push((factor, region, size, request));
            }
        }
        if width.max(height) / factor <= tile {
            break;
        }
        factor *= 2;
    }
    requests
}

fn backend(args: &Args, name: &str) -> anyhow::Result<Jp2Backend> {
    let max_decode_pixels = 100_000_000;
    Ok(match name {
        "j2k" => Jp2Backend::J2k { max_decode_pixels },
        "kakadu" => {
            let lib = args
                .native_lib
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("--native-lib is required for kakadu"))?;
            Jp2Backend::Native {
                native: KakaduNative::load(lib, 10_000_000)?,
                fallback: KakaduCli {
                    executable: "kdu_expand".into(),
                    temp_dir: std::env::temp_dir(),
                    max_decode_pixels,
                },
            }
        }
        other => anyhow::bail!("unknown decoder {other}"),
    })
}

fn percentile(sorted: &[Duration], p: f64) -> f64 {
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index].as_secs_f64() * 1000.0
}

async fn run(args: &Args, name: &str) -> anyhow::Result<()> {
    let (width, height, components) = jp2::metadata(&args.jp2)?;
    let requests = Arc::new(tile_requests(width, height, args.tile, args.format));
    let backend = Arc::new(backend(args, name)?);
    let jp2 = Arc::new(args.jp2.clone());
    let total = requests.len() * args.rounds;
    let next = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();
    let mut workers = Vec::new();
    for _ in 0..args.concurrency {
        let (requests, backend, jp2, next) =
            (requests.clone(), backend.clone(), jp2.clone(), next.clone());
        workers.push(tokio::spawn(async move {
            let mut timings = Vec::new();
            loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= total {
                    break;
                }
                let (factor, region, size, request) = &requests[index % requests.len()];
                let tile_started = Instant::now();
                let rendered = backend
                    .render(
                        &jp2,
                        (width, height),
                        *region,
                        *size,
                        request,
                        EncodeSettings::default(),
                    )
                    .await?;
                timings.push((*factor, tile_started.elapsed(), rendered.bytes.len()));
            }
            anyhow::Ok(timings)
        }));
    }
    let mut timings = Vec::new();
    for worker in workers {
        timings.extend(worker.await??);
    }
    let elapsed = started.elapsed();
    println!(
        "decoder={name} image={}x{}x{components} tile={} concurrency={} tiles={} wall={:.3}s rate={:.1} tiles/s",
        width,
        height,
        args.tile,
        args.concurrency,
        timings.len(),
        elapsed.as_secs_f64(),
        timings.len() as f64 / elapsed.as_secs_f64(),
    );
    let mut factors: Vec<u32> = timings.iter().map(|t| t.0).collect();
    factors.sort_unstable();
    factors.dedup();
    for factor in factors.into_iter().chain([0]) {
        let mut durations: Vec<Duration> = timings
            .iter()
            .filter(|t| factor == 0 || t.0 == factor)
            .map(|t| t.1)
            .collect();
        durations.sort_unstable();
        let mean =
            durations.iter().sum::<Duration>().as_secs_f64() * 1000.0 / durations.len() as f64;
        println!(
            "  scale={:<4} n={:<5} mean={:>8.2}ms p50={:>8.2}ms p95={:>8.2}ms max={:>8.2}ms",
            if factor == 0 {
                "all".to_string()
            } else {
                factor.to_string()
            },
            durations.len(),
            mean,
            percentile(&durations, 0.5),
            percentile(&durations, 0.95),
            durations.last().unwrap().as_secs_f64() * 1000.0,
        );
    }
    Ok(())
}

/// Render every tile with both decoders as PNG and report pixel differences.
async fn compare(args: &Args) -> anyhow::Result<()> {
    let (width, height, _) = jp2::metadata(&args.jp2)?;
    let kakadu = backend(args, "kakadu")?;
    let j2k = backend(args, "j2k")?;
    let mut worst_max = 0u8;
    let mut worst_psnr = f64::INFINITY;
    let mut squared = 0f64;
    let mut samples = 0u64;
    for (factor, region, size, request) in tile_requests(width, height, args.tile, Format::Png) {
        let mut decoded = Vec::new();
        for backend in [&kakadu, &j2k] {
            let rendered = backend
                .render(
                    &args.jp2,
                    (width, height),
                    region,
                    size,
                    &request,
                    EncodeSettings::default(),
                )
                .await?;
            decoded.push(image::load_from_memory(&rendered.bytes)?.to_rgb8());
        }
        let (a, b) = (&decoded[0], &decoded[1]);
        anyhow::ensure!(
            a.dimensions() == b.dimensions(),
            "size differs at {region:?}"
        );
        let mut tile_squared = 0f64;
        for (x, y) in a.as_raw().iter().zip(b.as_raw()) {
            let diff = x.abs_diff(*y);
            worst_max = worst_max.max(diff);
            tile_squared += f64::from(diff) * f64::from(diff);
        }
        let n = a.as_raw().len() as f64;
        let psnr = 10.0 * (255.0f64 * 255.0 / (tile_squared / n).max(1e-12)).log10();
        if psnr < worst_psnr {
            worst_psnr = psnr;
            println!("  new worst tile: scale={factor} {region:?} psnr={psnr:.2}dB");
        }
        squared += tile_squared;
        samples += n as u64;
    }
    let psnr = 10.0 * (255.0f64 * 255.0 / (squared / samples as f64).max(1e-12)).log10();
    println!(
        "compare image={}x{} tile={} overall_psnr={psnr:.2}dB worst_tile_psnr={worst_psnr:.2}dB max_abs_diff={worst_max}",
        width, height, args.tile
    );
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    if args.decoder == "compare" {
        return compare(&args).await;
    }
    run(&args, &args.decoder.clone()).await
}
