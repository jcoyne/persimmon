# JPEG XL source notes

Persimmon can serve JPEG XL (JXL) source images as well as JP2. These notes explain how JXL sources are handled and why they cost more to serve than JP2 sources.

## How JXL sources are handled

Persimmon detects a JXL source by its file signature, not its S3 key or extension. Both bare codestreams (`FF 0A`) and container files are recognized. Any other file is treated as JP2 and decoded with Kakadu.

JXL sources are decoded with the pure-Rust [`jxl-oxide`](https://crates.io/crates/jxl-oxide) crate (see `src/jxl.rs`):

- `info.json` dimensions come from the JXL header. Only as much of the file as the header needs is read.
- Image requests decode only the requested region, then go through the same resize, rotate, quality, and output encoding steps as JP2 sources. Every output format works.
- `/metrics` counts these renders as `persimmon_jxl_renders_total`.

Limitations:

- CMYK JXL images are rejected.
- Transparency is dropped.
- Animations serve only their first frame.
- Colour profiles aren't converted. Pixels come out in the image's own colour space. Typical sRGB images are fine, but images with an unusual embedded profile aren't converted to sRGB.

## Why JXL thumbnails are expensive

JPEG 2000 can decode a smaller version of an image directly. Persimmon's JXL decoder can't, so every JXL request decodes its region at full resolution.

### JPEG 2000 stores the image at several sizes

JPEG 2000 compresses with a wavelet transform. The transform splits the image into a half-size version plus the detail needed to rebuild full size. It then splits that half-size version the same way, usually five or more times. The file is effectively a stack of sizes (1/32, 1/16, … 1/2, full). Each size is stored as the previous size plus extra detail.

To get a 1/8-size image, a decoder reads only the bottom few layers and stops. It never touches the fine detail, which is most of the file, and it never builds a full-size bitmap.

Persimmon uses this. `reduction_level` in `src/pipeline.rs` works out how much smaller the output is than the region and passes that to Kakadu as `-reduce N` (or the native adapter's equivalent). A 256 px thumbnail of a 20,000 px image decodes roughly a 312 px image, not a 20,000 px one.

### JPEG XL mostly doesn't, and jxl-oxide can't use what it has

The JXL format has some multi-size support:

- Lossy JXL (VarDCT, what `cjxl` produces by default) stores a 1/8-size overview of the image inside the file. In principle a decoder could render just that.
- Lossless JXL only has a similar pyramid if the encoder chose to add one (the "squeeze" transform). Many files don't.

Persimmon can't take advantage of either, for two reasons:

1. The overview is one fixed step (1/8), not the series of halvings JPEG 2000 gives you.
2. `jxl-oxide` (0.12) has no "decode at reduced size" option. It can crop to a region, but rendering is always at full resolution. Its progressive rendering is for files that haven't finished downloading: you get a blurrier full-size image, not a smaller one.

A patched `jxl-oxide` can render the 1/8 overview of lossy files directly. See [Prototype: rendering the 1/8 overview](#prototype-rendering-the-18-overview).

### What this means in practice

For a JXL source, `full/256,/0/default.jpg` on a 100-megapixel image decodes all 100 megapixels, then scales them down. With a JP2 source the same request decodes about 0.1 megapixels. As a result, the JXL path:

- counts the whole region against `PERSIMMON_MAX_DECODE_PIXELS` instead of the reduced size, so regions over that limit (100 MP by default) fail;
- reserves 32 bytes of `PERSIMMON_MAX_TEMP_BITMAP_BYTES` per region pixel instead of 12, because `jxl-oxide` decodes through 32-bit float planes (see the measurements below);
- is much slower for zoomed-out views and thumbnails of large images.

Tile requests at full zoom cost about the same in both formats, since there's nothing to skip. The penalty is for viewers showing a large image zoomed out.

## Benchmark: one thumbnail

Measured on 2026-10-07: the request `full/400,267/0/default.jpg` for a 6000×4000 (24 MP) lossy JP2 photograph, through each render path.

| Source | Path | File size | Median time | Peak memory |
| --- | --- | --- | --- | --- |
| JP2 (original) | Kakadu native adapter | 4.5 MB | 41 ms | 12 MB |
| JP2 (original) | Kakadu `kdu_expand` | 4.5 MB | 48 ms | 8 MB |
| JXL, lossy (`cjxl` default) | jxl-oxide | 2.6 MB | 1,127 ms | 646 MB |
| JXL, lossless (`cjxl -d 0`) | jxl-oxide | 15.6 MB | 3,605 ms | 231 MB |

For this thumbnail, lossy JXL was about 25× slower than Kakadu and used about 50 to 80× more memory. Lossless JXL was about 75× slower.

How it was measured:

- A small benchmark binary called Persimmon's render functions directly (`KakaduNative::render`, `KakaduCli::render`, and `pipeline::render_jxl`). Each run did the same work as an uncached request: header read, decode, resize, and JPEG encode (default quality 75).
- Kakadu 8.6.2 was built for arm64 Linux. Everything ran single-threaded in one Docker container on an Apple M3 Pro.
- Source files were on local disk, so S3 download time is excluded.
- Times are medians of 30 runs for Kakadu, 10 for lossy JXL, and 5 for lossless JXL. Peak memory is the peak RSS of a separate one-request process, measured with GNU `time`. The `kdu_expand` figure includes the child process.
- The JXL files were made with `cjxl` 0.11.2 from a full-size decode of the JP2. Lossy used the defaults (distance 1, effort 7).

What the numbers show:

- Kakadu decoded only 750×500 (`-reduce 3`, 0.375 MP), so it stayed near 10 MB.
- jxl-oxide decoded all 24 MP. Lossy JXL peaked at about 27 bytes per source pixel, which is why the reservation is 32 bytes per pixel.
- Lossless JXL used less memory (about 10 bytes per pixel) but took about 3× longer than lossy. The lower memory suggests the lossy decoder holds extra full-size working buffers; this hasn't been confirmed.
- The four thumbnails look the same. They differ slightly at the pixel level (28 dB PSNR between Kakadu and JXL output) because Kakadu shrinks to 750×500 before resizing, while the JXL path resizes from full size.

## Prototype: rendering the 1/8 overview

Lossy JXL stores a 1/8-size overview, and jxl-oxide computes it internally before building the full-size image. A small patch to jxl-oxide 0.12 exposes it. Persimmon doesn't use this patch; it was tried only in the benchmark harness.

Results for the same request and setup as above (median of 30 runs):

| Source | Path | Median time | Peak memory |
| --- | --- | --- | --- |
| JP2 | Kakadu native adapter | 41 ms | 12 MB |
| JP2 | Kakadu `kdu_expand` | 48 ms | 8 MB |
| JXL, lossy | jxl-oxide, 1/8 overview (patched) | 56 ms | 26 MB |
| JXL, lossy | jxl-oxide, full decode (current Persimmon) | 1,127 ms | 646 MB |

Rendering the overview was about 20× faster than the full decode and used about 25× less memory, within about 1.4× of the Kakadu native adapter's time. The thumbnail looks the same as Kakadu's, including colour. Its PSNR is 36.9 dB against the full-decode JXL thumbnail and 28.3 dB against Kakadu's; the full-decode JXL thumbnail scores 28.1 dB against Kakadu's.

How the patch works:

- **jxl-render:** a new `RenderContext::render_keyframe_lf` runs the first part of the lossy (VarDCT) renderer. It decodes the overview, applies chroma-from-luma and adaptive smoothing, and converts it to the output colour space, then stops. It skips the fine-detail data, the full-size buffers, and the full-size filters.
- **jxl-oxide:** a new `JxlImage::render_frame_lf_u8` wraps it and returns 8-bit samples.
- **Benchmark harness:** crops the 750×500 overview to the region (divided by 8), then resizes and encodes it the same way `transform_encode` does.

Limitations of the prototype:

- Lossy files only. Lossless (modular) files return "not supported" and still need the full decode.
- Not handled: JPEGs losslessly converted to JXL (YCbCr frames, common in archives), files with a separate progressive overview frame, orientation, and animations.
- It only applies when the output is at least 8× smaller than the region. There's no halfway step like Kakadu's `-reduce 1` and `-reduce 2`, so outputs between 2× and 8× smaller still need the full decode.
- It decodes the overview for the whole image and then crops, rather than only the requested region. For a full-image thumbnail that's the same work, but cropped zoomed-out views waste some.
- It relies on jxl-oxide internals, so using it means carrying a patched fork or getting the change accepted upstream.

To use it in Persimmon, `jxl::decode_region` would try the overview when the output is at least 8× smaller than the region and the file is lossy, and fall back to the full decode otherwise. The patch would be carried with `[patch.crates-io]` or replaced by an upstream jxl-oxide release.

<details>
<summary>Patch: appended to <code>jxl-render-0.12.4/src/lib.rs</code></summary>

```rust
/// # Reduced-resolution rendering (local patch)
impl RenderContext {
    /// Renders only the 1/8-resolution LF image of a VarDCT keyframe, skipping
    /// HF coefficients and restoration filters, converted to the output color
    /// encoding.
    pub fn render_keyframe_lf(&self, keyframe_idx: usize) -> Result<ImageWithRegion> {
        use jxl_frame::header::Encoding;

        let idx = *self
            .keyframes
            .get(keyframe_idx)
            .ok_or(Error::IncompleteFrame)?;
        let frame = &*self.frames[idx];
        let frame_header = frame.header();
        if frame_header.encoding != Encoding::VarDct {
            return Err(Error::NotSupported("LF rendering requires a VarDCT frame"));
        }
        if frame_header.do_ycbcr {
            return Err(Error::NotSupported("LF rendering of YCbCr frames"));
        }
        if self.frame_deps[idx].lf != usize::MAX {
            return Err(Error::NotSupported("LF rendering with a separate LF frame"));
        }

        let lf_global = frame
            .try_parse_lf_global::<i32>()
            .ok_or(Error::IncompleteFrame)??;
        let lf_global_vardct = lf_global.vardct.as_ref().unwrap();
        let mut gmodular = lf_global.gmodular.try_clone()?;
        let lf_width = frame_header.color_sample_width().div_ceil(8);
        let lf_height = frame_header.color_sample_height().div_ceil(8);
        let lf_region = Region::with_size(lf_width, lf_height);
        let modular_lf_region =
            modular::compute_modular_region(frame_header, &gmodular, lf_region, true)
                .intersection(lf_region);
        let mut modular_image = gmodular.modular.image_mut();
        let lf_group_image = modular_image
            .as_mut()
            .map(|x| x.prepare_groups(frame.pass_shifts()))
            .transpose()?
            .map(|x| x.lf_groups)
            .unwrap_or_default();

        let mut lf_groups = std::collections::HashMap::new();
        let mut lf_xyb = util::load_lf_groups(
            frame,
            &lf_global,
            &mut lf_groups,
            lf_group_image,
            modular_lf_region,
            &self.pool,
        )?
        .ok_or(Error::IncompleteFrame)?;

        vardct::chroma_from_luma_lf(
            lf_xyb.as_color_floats_mut(),
            &lf_global_vardct.lf_chan_corr,
        );
        if !frame_header.flags.skip_adaptive_lf_smoothing() {
            vardct::adaptive_lf_smoothing(
                lf_xyb.as_color_floats_mut(),
                &lf_global.lf_dequant,
                &lf_global_vardct.quantizer,
            )?;
        }
        util::convert_color_for_record(&self.image_header, false, &mut lf_xyb, &self.pool)?;
        Ok(lf_xyb)
    }
}
```

</details>

<details>
<summary>Patch: appended to <code>jxl-oxide-0.12.6/src/lib.rs</code></summary>

```rust
/// # Reduced-resolution rendering (local patch)
impl JxlImage {
    /// Renders the 1/8-resolution LF image of a VarDCT keyframe as interleaved
    /// 8-bit color samples. Returns width, height, channel count, and samples.
    /// Orientation is not applied.
    pub fn render_frame_lf_u8(&self, keyframe_index: usize) -> Result<(u32, u32, usize, Vec<u8>)> {
        let image = self.ctx.render_keyframe_lf(keyframe_index)?;
        let width = self.image_header.size.width.div_ceil(8);
        let height = self.image_header.size.height.div_ceil(8);
        let channels = image.color_channels().min(3);
        let grids: Vec<_> = image.buffer()[..channels]
            .iter()
            .map(|b| b.as_float().expect("float color channel"))
            .collect();
        let mut out = Vec::with_capacity(width as usize * height as usize * channels);
        for y in 0..height as usize {
            for x in 0..width as usize {
                for grid in &grids {
                    let v = grid.buf()[y * grid.width() + x];
                    out.push((v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
                }
            }
        }
        Ok((width, height, channels, out))
    }
}
```

</details>

## Possible improvements

- **Render the 1/8 overview.** The [prototype](#prototype-rendering-the-18-overview) brings lossy JXL thumbnails close to Kakadu. It needs a patched or upstreamed jxl-oxide, and it only helps lossy files.
- **Encode with built-in sizes.** Produce sources with a multi-size layout, for example with `cjxl`'s progressive options. The decoder would still need support for rendering at reduced size before this helps.
- **Recommend JP2 for very large images.** Until one of the above lands, JP2 remains the better source format for large images that are often viewed zoomed out.
