# Adjacent tile load test

`load_tiles.py` sends concurrent GET requests for adjacent IIIF tiles to every supplied server instance. It reports request rate, error count, p50/p95/p99 latency, per-instance `/metrics` deltas, and optional process RSS samples. It uses only the Python standard library.

Run it against an already configured service and an image with known dimensions:

```sh
python3 tests/load_tiles.py \
  --url http://127.0.0.1:3001/v3 \
  --url http://127.0.0.1:3002/v3 \
  --identifier 'a/b' --width 1000 --height 1000 \
  --tile-size 256 --connections 16 --seconds 30 --warm \
  --output load-report.json
```

`--warm` requests each tile on each instance before starting the timed phase. Omit it to include source download and derivative generation in the measurement. `--pid` may be repeated to sample the server processes' resident memory. Metrics access must be available from the test host. The reported request rate measures this test client and environment; increase `--connections` and use more load-generator hosts if the client itself becomes the limit.

For a large source, add `--max-tiles 64` to keep the test focused on a repeated adjacent group and make the warm phase practical. Without it, the script covers the entire image grid.

## Local development baseline, 2026-09-29

Two Rust server processes on one macOS host, 16 persistent HTTP/1.1 connections, a 1000×1000 CC0 JP2 validator fixture, 16 adjacent 256 px tiles, a local Moto S3 service, and a Kakadu 8.4.1 command-line development stand-in were used for five-second measurements. These numbers do **not** validate the Linux image, the release Kakadu native adapter, AWS S3 behavior, or the production throughput target.

| Phase | Requests/s | Errors | p50 | p95 | p99 | S3 calls/request | Derivative hit rate per lookup | Source downloads | Renders | Peak server RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Warm derivatives | 556 | 0 | 28 ms | 31 ms | 41 ms | 2.00 | 100% | 0 | 0 | 37 and 36 MiB |
| Cold derivatives | 388 | 0 | 28 ms | 40 ms | 545 ms | 2.05 | 97% | 2 | 24 | 36 and 36 MiB |

The warm phase made one S3 marker HEAD and one derivative GET per image request. The cold phase rendered some of the same 16 tiles on both instances before either result appeared in the shared cache. The local Moto server and command-line decoder are part of this measurement, so this baseline cannot establish how many production instances are needed for thousands of requests per second.

## Linux native development baseline, 2026-09-29

Two Linux `amd64` Docker containers built with the licensed Kakadu 8.6.2 SDK ran on a macOS `arm64` host through emulation. Both used the native adapter, local Moto S3, the 1000×1000 CC0 validator JP2 (39,760 bytes), 16 adjacent 256 px tiles, and 16 persistent HTTP/1.1 client connections. Each timed phase lasted 10 seconds. Docker memory samples peaked at 43.15 and 41.2 MiB during the cold phase. The report files from this run are local development artifacts, not committed test fixtures.

| Phase | Requests/s | Errors | p50 | p95 | p99 | S3 calls/request | Derivative hit rate per lookup | Source downloads | Native renders | Command fallbacks |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Cold derivatives | 372 | 0 | 33 ms | 37 ms | 45 ms | 2.01 | 99.3% | 2 | 16 | 0 |
| Warm derivatives | 475 | 0 | 33 ms | 36 ms | 46 ms | 2.00 | 100% | 0 | 0 | 0 |

This test checks that two Linux containers share the derivative cache and use the native decoder. Its small JP2, local Moto service, and `amd64` emulation do not predict native Linux performance with 8 MB or 100 MB sources and AWS S3.

## Synthetic larger sources

Use the licensed Kakadu `kdu_compress` tool to create disposable JP2s without copying private source images into the repository:

```sh
python3 tests/generate_synthetic_jp2.py --width 4096 --height 4096 \
  --rate 4 --output /tmp/persimmon-synthetic-8mb.jp2 \
  --compressor /path/to/kdu_compress --workdir /path/to/kakadu/apps/make
python3 tests/generate_synthetic_jp2.py --width 16384 --height 16384 \
  --rate 3 --output /tmp/persimmon-synthetic-100mb.jp2 \
  --compressor /path/to/kdu_compress --workdir /path/to/kakadu/apps/make
```

The script streams random RGB pixels into a temporary PPM, compresses them, and removes the PPM. The two PPMs need about 50 MB and 805 MB of temporary disk space. A local Kakadu 8.6.2 run produced JP2s of 8,276,106 bytes and 99,418,876 bytes. These are useful for source download and tile decode stress, but random noise does not represent the compression behavior of archival images.

For a one-channel grayscale fixture, add `--channels 1`; the script uses a temporary PGM instead of a PPM. The grayscale Docker check is recorded in [ACCEPTANCE.md](ACCEPTANCE.md).

Two emulated Linux `amd64` containers on a macOS `arm64` host served these JP2s from local Moto S3. Each test used 16 persistent client connections and the first 64 adjacent 256 px tiles (`--max-tiles 64`). Cold phases started with empty derivative caches; the 100 MB cold phase used fresh containers and a new cache prefix so both source caches were empty too. The 8 MB cold phase also downloaded the source once per container. Timed phases lasted 10 seconds, except the fully cold 100 MB phase, which lasted 15 seconds.

| Source and phase | Requests/s | Errors | p50 | p95 | p99 | S3 calls/request | Derivative hit rate per lookup | Source downloads | Native renders | Command fallbacks |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 8 MB, cold | 302 | 0 | 38 ms | 89 ms | 118 ms | 2.32 | 77.4% | 2 | 120 | 0 |
| 8 MB, warm | 325 | 0 | 40 ms | 49 ms | 59 ms | 2.00 | 100% | 0 | 0 | 0 |
| 100 MB, fully cold | 276 | 0 | 38 ms | 121 ms | 143 ms | 2.24 | 82.2% | 2 | 117 | 0 |
| 100 MB, warm | 407 | 0 | 37 ms | 47 ms | 65 ms | 2.00 | 100% | 0 | 0 | 0 |

A Docker memory spot sample during an earlier 100 MB cold pass was 142 and 174 MiB for the two containers; it is not a peak measurement. These rates are dominated by the local test client, Moto, and `amd64` emulation and do not establish the production throughput target. The consistently two S3 calls per warm request come from the purge-generation HEAD and derivative GET.

Repeat the benchmark with the release Kakadu SDK (8.6 or newer), the native adapter, Linux `amd64` containers, representative 8 MB and 100 MB JP2 files, and AWS S3. Run both cold and warm phases at multiple concurrency levels, record server CPU and memory peaks, and retain the reports before accepting the performance target.
