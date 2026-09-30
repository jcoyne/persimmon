# Local JP2 throughput comparison: Persimmon and Cantaloupe

Measured on 2026-09-30. This is a development benchmark, not a production throughput claim.

## Setup

- **Persimmon:** repository commit `3b804a6`, Docker image `persimmon:dev` (`sha256:45c815827926f0e7519e510036e66773a9b7ff84c2c8fefd478f924756396dc1`), Kakadu 8.6.2 native regional adapter. `/metrics` showed 64 native renders per cold image, zero command renders and zero fallbacks. The subsequent `info.json` quality-advertisement fix was not in the measured image and does not affect tile rendering.
- **Cantaloupe:** [v5.0.7 release](https://github.com/cantaloupe-project/cantaloupe/releases/tag/v5.0.7), bundled Kakadu 8.4.1, Docker image `cantaloupe:bench` (`sha256:4cf82a6a858ee63a8d7a671f2acdebfc6d75d6744cae6cb5a1e2cf4aa88f15b8`). JP2 was mapped to `KakaduNativeProcessor`; its logs confirmed that processor served JP2 tiles. The image was built from the v5.0.7 tag with [this Dockerfile](benchmark/cantaloupe.Dockerfile) and its bundled Linux Kakadu libraries.
- **Host:** Apple M3 Pro, 12 logical CPUs, 36 GiB RAM, Docker 28.3.2. Both images ran as Linux `amd64` under emulation on the macOS `arm64` host, without container CPU or memory limits. Servers ran one at a time, against the same local Moto S3 service. The HTTP client also ran on this host.
- **Sources:** private Moto S3 bucket, two synthetic RGB JP2s made with Kakadu 8.6.2: 4096 × 4096, 8,276,106 bytes; and 16384 × 16384, 99,418,876 bytes. Generation commands are in [LOAD.md](LOAD.md). Both servers looked up `identifier.jp2` and used separate, initially empty S3 derivative buckets.
- **Caches and output:** Cantaloupe used `S3Source`, local `FilesystemCache` for sources, `S3Cache` for derivatives, IIIF v3, `processor.jpg.quality=75`, and `processor.jpg.progressive=false`. Persimmon used its local source cache and S3 derivative cache with default settings. Both served JPEG tiles over HTTP. Cantaloupe required `/iiif/3`, Persimmon `/v3`; both received the same region, size, rotation, quality and format suffixes.
- **Load:** [load_tiles.py](load_tiles.py), one server at a time, 16 persistent connections, first 64 adjacent 256 × 256 tiles, 10 second request window. A separate small JP2 warmed each process before its cold run. For each image, the cold phase began with its source absent from the local source cache and its derivatives absent from S3. The warm phase explicitly fetched all 64 derivatives before timing. The 8 MB image ran before the 100 MB image in each server. Completed in-flight requests can make the reported elapsed time exceed 10 seconds. Docker CPU and memory were sampled with `docker stats --no-stream` during each timed phase; percentages above 100% mean multiple CPU cores.

The local S3 endpoint was `http://host.docker.internal:5001`. Both servers used source bucket `persimmon-bench-source`, region `us-east-1`, and disposable Moto credentials. Cantaloupe used derivative bucket `persimmon-bench-cantaloupe-cache` with prefix `cantaloupe/`; Persimmon used `persimmon-bench-persimmon-cache`. Persimmon used its default 2 GB local source-cache target and 10 GB derivative-cache target; pruning was not running during the test. Cantaloupe's source cache path was `/tmp/cantaloupe-cache`; its source and derivative TTLs were 2,592,000 seconds. No TLS proxy or CDN was in the measured path.

Example timed commands (replace the URL with `http://127.0.0.1:8182/iiif/3` and add `--no-metrics --docker-container cantaloupe-bench` for Cantaloupe):

```sh
python3 tests/load_tiles.py --url http://127.0.0.1:3001/v3 \
  --identifier synthetic8 --width 4096 --height 4096 \
  --tile-size 256 --max-tiles 64 --connections 16 --seconds 10 \
  --docker-container persimmon-bench
# Repeat with --warm, then repeat both phases using synthetic100,
# --width 16384 and --height 16384.
```

## Results

The unrounded tool output for all eight runs is retained in [benchmark/results](benchmark/results).

| Source, phase | Server | Successful requests/s | Requests, errors | p50 / p95 / p99 latency | Docker CPU avg / peak | Docker memory sampled peak |
| --- | --- | ---: | ---: | --- | ---: | ---: |
| 8 MB, cold | Cantaloupe | 28.41 | 288, 0 | 334 / 1729 / 1944 ms | 263% / 497% | 692 MiB |
| 8 MB, cold | Persimmon | 338.31 | 3391, 0 | 39 / 77 / 123 ms | 111% / 178% | 74 MiB |
| 8 MB, warm | Cantaloupe | 102.80 | 1176, 0 | 53 / 239 / 2114 ms | 101% / 236% | 889 MiB |
| 8 MB, warm | Persimmon | 402.82 | 4042, 0 | 39 / 45 / 60 ms | 88% / 141% | 46 MiB |
| 100 MB, cold | Cantaloupe | 30.35 | 328, 0 | 546 / 1107 / 2531 ms | 1033% / 1244% | 1602 MiB |
| 100 MB, cold | Persimmon | 134.67 | 1829, 0 | 40 / 120 / 836 ms | 151% / 526% | 269 MiB |
| 100 MB, warm | Cantaloupe | 154.31 | 2162, 0 | 41 / 132 / 2088 ms | 175% / 268% | 1436 MiB |
| 100 MB, warm | Persimmon | 296.48 | 3856, 0 | 39 / 52 / 66 ms | 60% / 87% | 106 MiB |

Persimmon delivered 11.9× and 4.4× Cantaloupe's observed request rate in the 8 MB and 100 MB cold phases, and 3.9× and 1.9× in the warm phases. Persimmon's cold phases downloaded each source once and generated 64 derivatives with the native adapter. Its warm phases generated none and issued about two S3 operations per request (purge-marker HEAD and derivative GET). Cantaloupe did not expose equivalent per-request metrics in this setup.

## Interpretation and limits

The request sequence and S3 emulator were shared, but this does **not** isolate Rust from Java or one server implementation from another. Kakadu versions differed (8.6.2 versus 8.4.1), JPEG encoders can produce different byte sizes at the same nominal quality, cache lookup behavior differs, and all processes competed with the local Moto service and an emulated CPU. The synthetic random-noise JP2s are useful for decode and download stress, but do not represent archival imagery. CPU and memory numbers are a handful of Docker samples, not continuous peaks; the Cantaloupe JVM also retained memory between phases. Cold throughput includes the transition from misses to hits during the timed run, so it is not uncached decode capacity.

Repeat this comparison on native Linux `amd64` with representative JP2s, AWS S3, several concurrency levels and runs, and matched Kakadu versions before making a production capacity or efficiency claim.
