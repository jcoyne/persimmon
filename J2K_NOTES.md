# j2k decoder notes

Persimmon decodes JP2 sources with the pure-Rust [`j2k`](https://github.com/frames-sg/j2k) crate. Until 2026-10-30 it used Kakadu, and j2k was an optional, experimental alternative. The measurements below compare the two, and the Kakadu figures come from that earlier build.

Persimmon builds against j2k 0.13.1 from crates.io (released 2026-10-29). With the previous release, 0.12.0, on the untiled JP2s that SDR produces, Kakadu was 4 to 13 times faster at generating tiles, used a small fraction of the memory, and was the only one of the two that could decode large images at all. Those measurements are kept below under [j2k 0.12.0 (2026-10-14)](#j2k-0120-2026-10-14).

0.13.0 includes the changes measured on j2k `main` in October. At commit `2d3ccc29` (2026-10-23), Kakadu was 1.2 to 1.6 times faster, j2k decoded the 18000 × 12000 images, and on sources with PLT markers j2k used 2 to 3.5 times Kakadu's memory instead of 20 to 35 times. See [j2k main (2026-10-23)](#j2k-main-2026-10-23). At commit `3967d045` (2026-10-25), sources without PLT markers no longer cost much more than sources with them. On the 18000 × 12000 image without PLT, peak RSS falls from 750 to 91 MB at concurrency 1 and from 1,840 to 675 MB at concurrency 8. See [j2k main (2026-10-25)](#j2k-main-2026-10-25).

After 0.13.0, j2k `main` at `a5cc0a61` (2026-10-26) lowers the memory of each decode further, by 25 to 44% at concurrency 8, at the same speed. See [j2k main (2026-10-26)](#j2k-main-2026-10-26). At `6ca1bdbf` (2026-10-28), j2k writes decoded rows straight into Persimmon's output buffer, and peak footprint at concurrency 8 is 64 to 72% below 0.13.0's, again at the same speed. See [j2k main (2026-10-28)](#j2k-main-2026-10-28). 0.13.1 includes those changes, and faster classic Tier-1 decoding, which j2k measured at about 12% on a one-thread 1024 px region of the 18000 × 12000 image with PLT markers.

## Using it

The `j2k` crate needs Rust 1.99 or newer. `src/j2k_decoder.rs` memory-maps the source and decodes the requested region. Like Kakadu did, it discards resolution levels for smaller outputs, decodes on one thread per request, and checks the decoded size against `PERSIMMON_MAX_DECODE_PIXELS`. If a codestream has fewer resolution levels than a request wants, it decodes at the lowest available level and the resize does the rest. Grayscale sources decode to one channel. `/metrics` counts these renders as `persimmon_j2k_renders_total`. JPEG XL sources still use `jxl-oxide`.

To try an unreleased j2k, point Cargo at a checkout of the j2k repository. Every j2k crate Persimmon uses has to be patched, or Cargo mixes two copies:

```toml
[patch.crates-io]
j2k = { path = "../j2k/crates/j2k" }
j2k-codec-math = { path = "../j2k/crates/j2k-codec-math" }
j2k-core = { path = "../j2k/crates/j2k-core" }
j2k-native = { path = "../j2k/crates/j2k-native" }
j2k-profile = { path = "../j2k/crates/j2k-profile" }
j2k-types = { path = "../j2k/crates/j2k-types" }
```

## Memory reservation

Persimmon reserves extra `PERSIMMON_MAX_TEMP_BITMAP_BYTES` for each j2k decode: 4 bytes per sample of the region j2k decodes, after discarding resolution levels, plus 4 MiB. That is on top of the 12 bytes per region pixel that every JP2 request reserves.

With j2k 0.13.1, measured on 2026-10-29 with a counting allocator around `j2k_decoder::decode_region`, over every 1024 px tile at every scale of `archival.jp2`, `large-plt.jp2`, `large.jp2` and a grayscale encoding of `archival.jp2` (654 tiles):

| Source | Tiles of 256² samples or more | Most above 3 bytes per sample | Largest peak |
| --- | ---: | ---: | ---: |
| 6000 × 4000 RGB, PLT | 2.0 to 2.6 bytes per sample | none | 6.9 MiB |
| 18000 × 12000 RGB, PLT | 2.1 to 6.2 bytes per sample | 0.9 MiB | 7.4 MiB |
| 18000 × 12000 RGB, no PLT | 2.0 to 5.0 bytes per sample | 1.9 MiB | 9.0 MiB |
| 6000 × 4000 gray | 1.9 to 2.7 bytes per sample | none | 2.6 MiB |

Peaks include the output buffer. The tiles above 3 bytes per sample are small reduced-resolution tiles of the 18000 × 12000 images, where a roughly fixed cost is spread over few samples. Every tile peaked at least 3.3 MiB below its reservation. A full 1024 px RGB tile reserves about 28 MiB: 12 MiB for the request and 16 MiB for j2k.

With j2k 0.13.0, which filled a floating-point plane per component before writing the output, the same tiles peaked at 7 to 8 bytes per sample (RGB) and 9 to 11 (gray), and the reservation was 12 bytes per sample plus 4 MiB, about 52 MiB for a full 1024 px RGB tile.

Until 2026-10-26, the reservation was 10 bytes per source sample, up to 512 MiB, sized for j2k 0.12.0, whose cost followed the source. With the default budget, that let about three j2k decodes of a large untiled image run at once. Without it, eight concurrent decodes of the 24 MP image peaked at 3.3 GB with 0.12.0.

## j2k 0.13.1 on an SDR image (2026-10-30)

[`gg891fv3841_0002.jp2`](https://stacks.stanford.edu/file/druid:gg891fv3841/gg891fv3841_0002.jp2) is a 5661 × 8287 RGB JP2 from SDR: untiled, lossy (9/7), RPCL progression, five decomposition levels, 256 px precincts at the top two resolutions and 128 px below, 20 MB. `tile_bench` rendered all 234 tiles of the 1024 px grid at every scale, three rounds, natively on an Apple M3 Pro, with peak memory from `/usr/bin/time -l`:

| Concurrency | Tiles/s | Mean | p95 | Max | Peak memory footprint | Max RSS |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 7.2 | 140 ms | 245 ms | 454 ms | 57 MiB | 81 MiB |
| 8 | 43.1 | 174 ms | 314 ms | 570 ms | 193 MiB | 352 MiB |

RSS counts the memory-mapped source pages; the footprint doesn't. At concurrency 8 the footprint stayed below the 224 MiB that Persimmon reserves for eight concurrent full 1024 px RGB tiles (28 MiB each; see [Memory reservation](#memory-reservation)). Scale 8 was the slowest, with a 450 ms median, and full-resolution tiles took 129 ms at the median on one thread.

## j2k main (2026-10-28)

### What changed since 2026-10-26

- [#294](https://github.com/frames-sg/j2k/pull/294): a region decode on one thread, as in Persimmon, now decodes all of a tile's components together and writes each finished row straight into the caller's RGB8 or Gray8 buffer, instead of filling a whole floating-point plane per component first. That removes the 12 MiB of planes from a 1024 px RGB tile. In j2k's own counters, the decoder now holds about 3.5 to 6 MiB for such a tile, against 14 to 16 MiB on 2026-10-26. Output is unchanged. A decode that fails may leave part of the output buffer written. Persimmon discards the buffer on any error, so this doesn't matter here.
- [#296](https://github.com/frames-sg/j2k/pull/296): codestreams whose tile-part headers set their own wavelet, levels or MCT (tile-part COD or COC markers) now decode correctly. Before, every j2k release decoded them wrong. None of the JP2s checked locally has such markers: Persimmon's validator fixture, the benchmark sources and the JP2 fixtures in the sul-dlss repositories. The S3 source bucket hasn't been checked.
- [#292](https://github.com/frames-sg/j2k/pull/292): a decoder that is used again for another region of the same source without PLT markers reuses the packet boundaries it found the first time. Persimmon creates a new decoder for every request, so it doesn't benefit as it stands. See [Keeping a decoder per source](#keeping-a-decoder-per-source).

### Benchmark

Same harness, sources and settings as [2026-10-26](#j2k-main-2026-10-26), with j2k 0.13.0 from crates.io and `main` at `6ca1bdbf` (built with `[patch.crates-io]`), run alternately on 2026-10-28. Each figure is the mean of two passes. The machine had other work running (load average 7 to 12), so throughput differences of a few percent are noise. Kakadu was not re-run.

| Source | j2k | Concurrency 1: mean tile time | Concurrency 1: peak footprint | Concurrency 8: tiles/s | Concurrency 8: peak RSS | Concurrency 8: peak footprint |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 6000 × 4000, PLT | 0.13.0 | 85 ms | 53 MB | 72.8 | 231 MB | 217 MB |
| 6000 × 4000, PLT | `6ca1bdbf` | 87 ms | 18 MB | 70.0 | 87 MB | 73 MB |
| 18000 × 12000, PLT | 0.13.0 | 140 ms | 57 MB | 46.5 | 330 MB | 314 MB |
| 18000 × 12000, PLT | `6ca1bdbf` | 140 ms | 25 MB | 45.0 | 109 MB | 88 MB |
| 18000 × 12000, no PLT | 0.13.0 | 160 ms | 70 MB | 38.8 | 726 MB | 359 MB |
| 18000 × 12000, no PLT | `6ca1bdbf` | 160 ms | 23 MB | 40.0 | 505 MB | 128 MB |

At concurrency 8, peak footprint falls by 144 to 231 MB, about 18 to 29 MB per concurrent decode. For the source without PLT, most of the gap between footprint and RSS is still memory the allocator keeps after decodes.

The 12 bytes per decoded sample plus 4 MiB that Persimmon [reserves](#memory-reservation) for each j2k decode was sized for 0.13.0's 7 to 8 bytes per sample. With 0.13.1 it is 4 bytes per sample plus 4 MiB; see [Memory reservation](#memory-reservation).

### Keeping a decoder per source

To see whether #292 is worth using, a separate harness (`warm` in the j2k-bench directory) decoded the same tile grid through `decode_region_scaled_pow2_into`, as `src/j2k_decoder.rs` does, decoding only, with no resize or encode. It compared a new `J2kDecoder` for every tile, as Persimmon does today, against one decoder per worker thread kept for the whole run. Means of two passes:

| Source | Concurrency | Scale-1 tiles: new decoder | Scale-1 tiles: kept decoder | All tiles/s: new | All tiles/s: kept | Peak footprint: new / kept |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 18000 × 12000, no PLT | 1 | 141 ms | 119 ms | 6.5 | 7.2 | 21 / 23 MB |
| 18000 × 12000, no PLT | 8 | 176 ms | 156 ms | 41.2 | 44.6 | 126 / 129 MB |
| 18000 × 12000, PLT | 1 | 117 ms | 116 ms | 7.5 | 7.5 | 19 / 19 MB |
| 6000 × 4000, PLT | 1 | 80 ms | 74 ms | 11.8 | 12.6 | 17 / 17 MB |

The 6000 × 4000 row's gap comes from one pass (83 against 73 ms). In the other, the two were 76 and 76 ms. A kept decoder makes full-resolution tiles of the source without PLT about 15% faster. Scaled tiles gain nothing, because j2k builds a new image for every reduced-resolution request and so discards what it learned. Sources with PLT markers already locate packets cheaply and gain nothing either. The archival-style sources Persimmon serves have PLT markers. So caching decoders per source, which would mean keeping each memory map and decoder alive across requests and evicting them, isn't worth it unless many sources lack PLT. If j2k kept its reduced-resolution images too, the gain would extend to scaled tiles of such sources.

## j2k main (2026-10-26)

### What changed since 0.13.0

[#287](https://github.com/frames-sg/j2k/pull/287): when a region decode runs on one thread, as in Persimmon, j2k decodes each sub-band's code blocks one row at a time, just before the inverse wavelet transform reads that row. Before, it decoded the whole region's coefficients first. In j2k's own counters, a 1024 px region of the 18000 × 12000 image now holds under 1 MiB of coefficients instead of about 5 MiB, and peaks at 14 to 16 MiB instead of 18 to 20 MiB. Most of what remains is the 12 MiB of decoded output planes.

### Benchmark

Same harness, machine, sources and settings as [2026-10-25](#j2k-main-2026-10-25), with j2k 0.13.0 from crates.io and `main` at `a5cc0a61` (built with `[patch.crates-io]`), run alternately on 2026-10-26. Each figure is the mean of two passes. Kakadu was not re-run.

| Source | j2k | Concurrency 1: mean tile time | Concurrency 1: peak footprint | Concurrency 8: tiles/s | Concurrency 8: peak RSS | Concurrency 8: peak footprint |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 6000 × 4000, PLT | 0.13.0 | 83 ms | 47 MB | 77.7 | 246 MB | 232 MB |
| 6000 × 4000, PLT | `a5cc0a61` | 82 ms | 26 MB | 77.9 | 190 MB | 175 MB |
| 18000 × 12000, PLT | 0.13.0 | 137 ms | 48 MB | 47.6 | 307 MB | 287 MB |
| 18000 × 12000, PLT | `a5cc0a61` | 137 ms | 33 MB | 48.0 | 191 MB | 162 MB |
| 18000 × 12000, no PLT | 0.13.0 | 155 ms | 43 MB | 42.0 | 660 MB | 300 MB |
| 18000 × 12000, no PLT | `a5cc0a61` | 157 ms | 32 MB | 42.1 | 606 MB | 225 MB |

Peak footprint is macOS's `peak memory footprint` from `/usr/bin/time -l`. At concurrency 8 it falls by 57 to 125 MB, about 7 to 16 MB per concurrent decode. Tile times are within the run-to-run noise of about 2%. For the source without PLT, most of the gap between footprint and RSS is memory the allocator keeps after decodes, as on 2026-10-25.

## j2k main (2026-10-25)

### What changed since 2026-10-23

These upstream changes stop region decodes from building packet-header state for precincts they don't use ([#190](https://github.com/frames-sg/j2k/issues/190)):

- Tag trees are stored as one grid per level, with 8-byte nodes: [#191](https://github.com/frames-sg/j2k/pull/191).
- Code blocks the region doesn't use get no layer or segment records: [#196](https://github.com/frames-sg/j2k/pull/196).
- In RPCL, PCRL and CPRL, precincts that are only parsed to reach later packets share a few scratch precincts instead of keeping their own state: [#197](https://github.com/frames-sg/j2k/pull/197).
- Precincts the region never parses get no records: [#198](https://github.com/frames-sg/j2k/pull/198).
- Smaller fixes: [#192](https://github.com/frames-sg/j2k/pull/192), [#194](https://github.com/frames-sg/j2k/pull/194) and [#195](https://github.com/frames-sg/j2k/pull/195).

In j2k's own counters, a 1024 px region of the 18000 × 12000 image now peaks at 18 to 20 MiB with or without PLT markers, wherever it lies. Before these changes, it peaked at 25 MiB with PLT and, without PLT, up to about 78 MiB at the bottom-right, 60 MiB of it packet-header state. Of the remaining peak, 12 MiB is the decoded output planes and 5 MiB the wavelet coefficients.

### Benchmark

Same harness, machine, sources and settings as [2026-10-23](#j2k-main-2026-10-23), with j2k at `2d3ccc29` (the 2026-10-23 `main`) and `3967d045` run alternately on the same day. Kakadu was not re-run; its 2026-10-23 figures are on the same machine. The machine was under other load (load average 5 to 8), so compare the two j2k rows with each other rather than with the earlier tables.

| Source | j2k | Concurrency 1: mean tile time | Concurrency 1: peak RSS | Concurrency 8: tiles/s | Concurrency 8: peak RSS |
| --- | --- | ---: | ---: | ---: | ---: |
| 6000 × 4000, PLT | `2d3ccc29` | 87 ms | 63 MB | 74.6 | 264 MB |
| 6000 × 4000, PLT | `3967d045` | 86 ms | 54 MB | 77.2 | 253 MB |
| 18000 × 12000, PLT | `2d3ccc29` | 138 ms | 59 MB | 48.2 | 382 MB |
| 18000 × 12000, PLT | `3967d045` | 137 ms | 57 MB | 48.6 | 323 MB |
| 18000 × 12000, no PLT | `2d3ccc29` | 158 ms | 750 MB | 41.9 | 1,840 MB |
| 18000 × 12000, no PLT | `3967d045` | 154 ms | 91 MB | 43.1 | 675 MB |

For the source without PLT at concurrency 8, macOS's peak memory footprint falls from 1,454 to 316 MB, about the same as the source with PLT (300 MB). Most of the gap between that and the 675 MB RSS is memory the allocator keeps after decodes; the memory-mapped source is 48 MB.

Against Kakadu's 2026-10-23 figures, j2k is now about 1.2 times slower on the 18000 × 12000 image with or without PLT. At concurrency 8 it uses about 3 times Kakadu's memory with PLT and about 6 times without, measured by RSS.

## j2k main (2026-10-23)

### What changed since 0.12.0

These upstream changes are in j2k `main` but not yet in a release:

- Region decodes size their coefficient buffers to the region and parse only the packets it needs: [#126](https://github.com/frames-sg/j2k/pull/126), [#128](https://github.com/frames-sg/j2k/pull/128), [#129](https://github.com/frames-sg/j2k/pull/129) and [#131](https://github.com/frames-sg/j2k/pull/131). This removes the whole-tile allocation and the 512 MiB failures.
- Region decodes run one component at a time and store only the windows the inverse wavelet transform reads. The last transform level writes straight into the output, and the transform streams a few rows at a time. These come from [#171](https://github.com/frames-sg/j2k/pull/171), [#172](https://github.com/frames-sg/j2k/pull/172), [#177](https://github.com/frames-sg/j2k/pull/177), [#178](https://github.com/frames-sg/j2k/pull/178), [#179](https://github.com/frames-sg/j2k/pull/179), [#182](https://github.com/frames-sg/j2k/pull/182) and [#188](https://github.com/frames-sg/j2k/pull/188), tracked in [#133](https://github.com/frames-sg/j2k/issues/133). In j2k's own counters, a 1024 px region of the 18000 × 12000 image peaks at 25 MiB, down from 50 MiB.

### Benchmark

Measured on 2026-10-23 with `examples/tile_bench.rs`, the same harness as the 0.12.0 runs. Each run renders every tile in the `info.json` tile grid at every scale factor, with 1024 px tiles, as JPEG at quality 75, through `Jp2Backend::render`.

- Hardware: Apple M3 Pro (12 cores), running natively on macOS, not in Docker. These times are not comparable with the 0.12.0 Docker runs below. Compare the decoders within one table.
- Kakadu: 8.6.2 SDK, with Persimmon's adapter built for macOS arm64 (see [Running the benchmark](#running-the-benchmark)).
- j2k at three points, built with `[patch.crates-io]`:
  - 0.12.0 from crates.io;
  - `4120e3f9`, the last commit before #171, which includes #126–#131;
  - `main` at `2d3ccc29`.
- Concurrency 8 matches the default `PERSIMMON_MAX_PARALLEL_DECODES`. j2k decodes on one thread per request, as Persimmon configures it.
- Sources:
  - `archival.jp2`: the same 6000 × 4000 photograph as below (`PC0170`), which has PLT markers.
  - 18000 × 12000 RGB mosaics made from it, one with PLT markers (`large-plt.jp2`, 48 MB) and one without (`large.jp2`).
- The 6000 × 4000 runs rendered the 33-tile set three times per decoder, alternating decoders, and the table gives the range or mean. The 18000 × 12000 runs rendered the 294-tile set once.
- Peak RSS is the whole benchmark process (`/usr/bin/time -l`), including pages of the memory-mapped source and memory the allocator keeps after a decode.

#### 6000 × 4000 RGB (PLT markers)

| Decoder | Concurrency 1: mean tile time | Concurrency 1: peak RSS | Concurrency 8: tiles/s | Concurrency 8: peak RSS |
| --- | ---: | ---: | ---: | ---: |
| j2k 0.12.0 | 279 ms | 475 MB | 18–21 | 3,290–3,356 MB |
| j2k `4120e3f9` (before #171) | 83 ms | 156 MB | 73–74 | 548–557 MB |
| j2k `main` | 82 ms | 55 MB | 75–76 | 248–252 MB |
| Kakadu 8.6.2 | 51 ms | 22 MB | 120–123 | 95–99 MB |

#### 18000 × 12000 RGB

j2k 0.12.0 fails every request for these images with `image is too large`.

| Source | Decoder | Concurrency 1: mean tile time | Concurrency 1: peak RSS | Concurrency 8: tiles/s | Concurrency 8: peak RSS |
| --- | --- | ---: | ---: | ---: | ---: |
| PLT markers | j2k `4120e3f9` | 141 ms | 146 MB | 46.3 | 961 MB |
| PLT markers | j2k `main` | 139 ms | 63 MB | 44.8 | 381 MB |
| PLT markers | Kakadu 8.6.2 | 113 ms | 31 MB | 60.0 | 109 MB |
| no PLT | j2k `4120e3f9` | 160 ms | 880 MB | 41.3 | 1,956 MB |
| no PLT | j2k `main` | 160 ms | 875 MB | 41.8 | 1,787 MB |
| no PLT | Kakadu 8.6.2 | 131 ms | 33 MB | 49.6 | 114 MB |

#### Sources without PLT markers

Without PLT markers, j2k cannot skip a packet without reading its header. It builds the full packet-header state of every precinct before the last one the region needs, so the cost depends on where the region falls in the codestream's progression order. A 1024 px region of `large.jp2` needs 27 MiB of this state at the top-left, 46 MiB in the middle and 60 MiB at the bottom-right, against 7 MiB with PLT. This is reported upstream as [#190](https://github.com/frames-sg/j2k/issues/190).

No single tile of `large.jp2` peaked above 139 MB when decoded in its own process. The 875 MB above builds up across requests: decoding the same tile repeatedly in one process added about 5 MB of RSS per decode. macOS's `leaks` reported no leaks, and only 188 KB was still allocated at exit against an 86 MB footprint, so the allocator is keeping the freed memory. Persimmon uses glibc's allocator in production, which may behave differently; this has not been measured on Linux.

#### Output

`tile_bench --decoder compare` rendered every 1024 px tile of the 6000 × 4000 source as PNG with Kakadu and j2k `main`. No sample differed by more than 1 out of 255 (overall PSNR 58.2 dB), the same as with 0.12.0.

## j2k 0.12.0 (2026-10-14)

These measurements are with j2k 0.12.0, the release Persimmon built against until 0.13.0. They describe that release, not later ones.

### Why it is slow on SDR images

SDR's JP2s are a single JPEG 2000 tile covering the whole image. (They use `kdu_compress` with precincts but no `Stiles`; see `assembly-image`.) Kakadu decodes a region of such an image by working only on the precincts and code blocks the region needs. j2k 0.12 appears to size its working buffers to every whole JPEG 2000 tile the region touches, which for an untiled image means the whole image. Its memory use follows the source, not the request, and drops to 47 MB when the same image is tiled. So:

- Every request, even a 256 px tile, costs memory proportional to the whole source: about 420 MB for a 6000 × 4000 RGB photograph and 230 MB for its grayscale version. Much of the extra time goes into allocating and clearing those buffers. System CPU time was up to a third of the total, compared with about 3% for Kakadu.
- j2k refuses any decode needing more than 512 MiB (its fixed `DEFAULT_MAX_DECODE_BYTES`). Every request for an 18000 × 12000 untiled image failed with `image is too large`. The cut-off for an untiled RGB image is roughly 30 MP.

When the same photograph was re-encoded with 1024 × 1024 JPEG 2000 tiles (`Stiles={1024,1024}`), j2k was within 20% of Kakadu and peaked at 47 MB. j2k could be practical for tiled sources, but existing SDR images would all need re-encoding first.

### Benchmark: generating tiles

Measured on 2026-10-14 with `examples/tile_bench.rs`. For each decoder, it renders every tile a IIIF viewer would request from the tile grid in `info.json`, at every scale factor. Each tile is a region of `tile × scale` source pixels, output `tile` pixels wide as JPEG at quality 75, through Persimmon's own `Jp2Backend::render`. Timings cover decode, resize, and encode only. S3, the source cache, the derivative cache, and HTTP are excluded.

- Hardware: Docker Desktop on an Apple Silicon Mac, `linux/arm64` container, 12 CPUs. This is not production hardware.
- Kakadu: 8.6.2 SDK through Persimmon's native adapter (`libpersimmon_kakadu.so`).
- j2k: 0.12.0, release build, one decode thread per request.
- Concurrency 8 matches the default `PERSIMMON_MAX_PARALLEL_DECODES`.
- Sources: a real 6000 × 4000 archival photograph (`PC0170`, 4.5 MB, 9/7 irreversible, RPCL, 5 decomposition levels, 7 layers, 256/256/128 precincts, one tile). The other sources were made from it with the same `kdu_compress` options: a grayscale version, an 18000 × 12000 3 × 3 mosaic (48 MB), and a copy with 1024 px JPEG 2000 tiles.
- The 6000 × 4000 runs rendered the full tile set three times (99 tiles at 1024 px and 1,539 at 256 px). The others rendered it once.

#### Throughput

| Source | Tile | Concurrency | Kakadu tiles/s | j2k 0.12.0 tiles/s | Kakadu advantage |
| --- | ---: | ---: | ---: | ---: | ---: |
| 6000 × 4000 RGB | 1024 | 1 | 17.7 | 2.7 | 6.6× |
| 6000 × 4000 RGB | 1024 | 8 | 107.5 | 15.2 | 7.1× |
| 6000 × 4000 RGB | 256 | 1 | 71.3 | 6.0 | 11.9× |
| 6000 × 4000 RGB | 256 | 8 | 391.7 | 30.4 | 12.9× |
| 6000 × 4000 gray | 1024 | 1 | 15.2 | 3.6 | 4.2× |
| 6000 × 4000 gray | 1024 | 8 | 96.9 | 21.1 | 4.6× |
| 6000 × 4000 gray | 256 | 1 | 55.2 | 9.7 | 5.7× |
| 6000 × 4000 gray | 256 | 8 | 327.5 | 52.9 | 6.2× |
| 18000 × 12000 RGB | 1024 | 1 | 6.9 | fails | — |
| 18000 × 12000 RGB | 1024 | 8 | 42.4 | fails | — |
| 6000 × 4000 RGB, 1024 px JPEG 2000 tiles | 1024 | 1 | 20.4 | 17.1 | 1.2× |

#### Latency and memory

Per-tile render time across all scale factors, and the benchmark process's peak resident memory:

| Source | Tile | Concurrency | Kakadu mean / p95 | j2k 0.12.0 mean / p95 | Kakadu peak RSS | j2k 0.12.0 peak RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 6000 × 4000 RGB | 1024 | 1 | 57 / 86 ms | 371 / 806 ms | 17 MB | 429 MB |
| 6000 × 4000 RGB | 1024 | 8 | 72 / 107 ms | 517 / 1,131 ms | 80 MB | 3,206 MB |
| 6000 × 4000 RGB | 256 | 1 | 14 / 24 ms | 167 / 770 ms | 10 MB | 422 MB |
| 6000 × 4000 RGB | 256 | 8 | 20 / 34 ms | 263 / 1,073 ms | 25 MB | 3,334 MB |
| 6000 × 4000 gray | 1024 | 1 | 66 / 95 ms | 280 / 688 ms | 15 MB | 233 MB |
| 6000 × 4000 gray | 256 | 8 | 24 / 36 ms | 151 / 887 ms | 21 MB | 1,634 MB |
| 18000 × 12000 RGB | 1024 | 1 | 146 / 222 ms | fails | 24 MB | — |
| 6000 × 4000 RGB, 1024 px JPEG 2000 tiles | 1024 | 1 | 49 / 86 ms | 58 / 99 ms | 17 MB | 47 MB |

j2k 0.12.0 latency is uneven on untiled images. p50 ranged from 46 to 305 ms, but p95 reached 670 to 1,130 ms. Full-resolution tiles were the slowest. For RGB, the 1/8 to 1/32 scale tiles took 80 to 130 ms.

#### Output

`tile_bench --decoder compare` rendered every 1024 px tile as PNG with both decoders. No sample differed by more than 1 out of 255 (overall PSNR 58.2 dB RGB, 60.8 dB gray, 58.4 dB tiled), which is rounding in the inverse wavelet and colour transforms. Output dimensions matched for every tile and scale factor.

## Running the benchmark

```sh
cargo run --release --example tile_bench -- \
  --jp2 image.jp2 --tile 1024 --concurrency 8 --rounds 3
```

Wrap a run in `/usr/bin/time -v` (Linux) or `/usr/bin/time -l` (macOS) to get peak memory.

The Kakadu comparisons below used this example's `--decoder kakadu` and `--decoder compare` options with the Kakadu native adapter (`native/kakadu_adapter.cpp`), both removed on 2026-10-30. Check out an earlier commit to repeat them.

## What would change the picture

- **Sources without PLT markers.** Since [#190](https://github.com/frames-sg/j2k/issues/190) (in 0.13.0), these cost j2k about as much per decode as sources with PLT markers. Re-encoding with PLT markers (`ORGgen_plt=yes`) is no longer needed for memory.
- **Tiled sources.** Re-encoding with `Stiles={1024,1024}` (or matching the advertised tile size) brought j2k 0.12.0 within about 20% of Kakadu. It also made Kakadu's full-resolution tiles about 30% faster. This means re-encoding the existing collection, and matters less with j2k `main`, which no longer allocates for the whole tile.
- **HTJ2K.** j2k's block decoder is much faster for HTJ2K than for classic JPEG 2000. It too would need re-encoding.
