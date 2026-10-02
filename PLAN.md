# Persimmon implementation plan

Persimmon is a Rust IIIF Image API 3.0 server for private JPEG 2000 images in S3. The first release aims for minimum Level 2 compliance, with WebP as an extra output format and Kakadu 8.6 or newer for decoding. It runs as a Linux `amd64` Docker image. The public IIIF prefix defaults to `/v3`; routing is structured so v2 can be added later.

This is a progress checklist. A checked item means the feature is implemented and has the local verification described below. It does **not** imply production acceptance. Unchecked items are still required before claiming the release or throughput target is complete.

## IIIF API

- [x] Parse v3 image and `info.json` routes, including percent-encoded slashes in identifiers, with a configurable prefix and public base URL. Verify both `/images/v3` and an empty IIIF prefix under `/images` in Docker; see [tests/ACCEPTANCE.md](tests/ACCEPTANCE.md).
- [x] Implement Level 2 region and size forms, right-angle rotations, and `default`, `color`, and `gray` qualities. Apply transformations in IIIF order. Reject unsupported optional operations without advertising them.
- [x] Encode JPEG and PNG as required by Level 2, plus WebP; return matching MIME types and dimensions.
- [x] Generate `info.json` with dimensions, profile, capabilities, `sizes`, and `tiles`. Make the minimum advertised size (64 px) and tile size (1024 px) configurable. Verify all advertised sizes and edge tiles at each scale on 1000 px and 4096 px fixtures; see [tests/ACCEPTANCE.md](tests/ACCEPTANCE.md).
- [x] Implement base URI redirects, CORS, JSON-LD information responses, `GET`, `HEAD`, `OPTIONS`, cache headers, and IIIF error responses.
- [ ] Complete release conformance validation. The updated official validator passed 31/31 Level 2 checks with Kakadu 8.6.2 in the Linux Docker image and the CC0 fixture on 2026-09-30; see [tests/VALIDATOR.md](tests/VALIDATOR.md). Rerun with the chosen release build and representative archival color and grayscale sources before claiming production Level 2 compliance.

## Kakadu and image processing

- [x] Build `kdu_expand` and a C++ C-ABI regional decoder from a privately supplied licensed Kakadu 8.6+ SDK. The Docker build uses a named `kakadu` build context; no SDK files are in the repository. The available local SDK is Kakadu 8.6.2 at `/Users/jcoyne85/Desktop/kakadu-v8_6_2-02138L`.
- [x] Decode JP2 regions at reduced resolution in memory with the native adapter, then perform final transforms and output encoding in Rust. Bound concurrent decode and memory use. Use `kdu_expand` as a separate fallback with its own decoded-pixel and temporary-bitmap limits.
- [x] Compare native output against Kakadu command output on local fixtures and verify native rendering in a Linux `amd64` Docker container. Benchmark adjacent tiles with synthetic 8 MB and 100 MB JP2 files; see [tests/LOAD.md](tests/LOAD.md).
- [x] Check a synthetic one-channel JP2 through native Linux Docker decoding: `info.json`, JPEG, WebP, and a rotated grayscale PNG returned correctly with zero command fallbacks; see [tests/ACCEPTANCE.md](tests/ACCEPTANCE.md).
- [ ] Validate concurrency, pixel accuracy, memory peaks, and fallback behavior with the **exact Kakadu SDK chosen for release** and representative archival JP2 files, including color and grayscale sources. Synthetic sources are useful load fixtures but do not represent archival compression or component layouts.

## S3 sources and caches

- [x] Read private S3 source objects through the AWS credential chain. Decode an identifier once and append `.jp2` to form its S3 key; `a%2Fb` resolves to `a/b.jp2`. Never use the identifier as a local path.
- [x] Keep a per-instance source cache with a configurable 2 GB default, LRU eviction, coalesced simultaneous downloads, and source leases that prevent eviction while in use. An active lease can temporarily put the cache over its target; eviction runs when it is released.
- [x] Store generated images in a shared private S3 cache keyed by API version, identifier, request, format, and purge generation. Coalesce duplicate rendering within an instance and avoid overwriting another instance's result with conditional writes.
- [x] Stream cache-hit derivatives from S3 to HTTP clients without buffering the full object. Verify cold and warm bytes match and `HEAD` retains the correct `Content-Length` locally; see [tests/ACCEPTANCE.md](tests/ACCEPTANCE.md).
- [x] Keep each newly rendered image's memory reservation until its S3 write and HTTP response release their final reference to the encoded bytes. Test permit lifetime across cloned response bytes.
- [x] Provide a designated `prune-cache` command and `prune-cache-loop`. They delete oldest derivatives until usage is below the configurable 10 GB target. The target is soft between cleanup runs; purge markers are outside the pruned prefix.
- [x] Implement generation markers in S3. Every request checks the marker before its cache lookup, and rendering rechecks before writing. An authenticated purge changes the generation across instances and removes old derivatives in the background. Source replacement at an existing key requires a purge.
- [x] Allow downstream HTTP caches to retain old responses until their normal one-day expiry after purge.
- [x] Test concurrent source reuse, local LRU eviction, source replacement and shared derivative invalidation across two instances, and cache pruning against a local S3-compatible service.
- [ ] Verify source, derivative, purge, and cleanup behavior against **AWS S3** with production-equivalent IAM permissions and failure modes. Local Moto tests do not establish AWS behavior.

## Operations and security

- [x] Provide `POST /admin/purge` with HTTP Basic authentication, constant-time credential comparison, and runtime-supplied credentials. Require native TLS or an HTTPS public base URL behind a trusted TLS proxy before starting with admin auth.
- [x] Support native TLS via certificate and key files. Provide `GET /healthz` with plain-text `OK` when Kakadu and required S3 buckets are available, plus a Docker `HEALTHCHECK`.
- [x] Expose `/metrics` and structured request logs covering latency, S3/cache activity, decode backend, rendering, evictions, errors, and purges. Document environment variables and minimal S3 permissions in [README.md](README.md).
- [x] Build and run a Linux `amd64` Docker image with the licensed Kakadu 8.6.2 SDK supplied as a private build context.
- [x] Run and document an end-to-end, two-container check of native TLS, private S3 lookup for an encoded-slash identifier, admin authentication, and purge propagation against local Moto. See [tests/ACCEPTANCE.md](tests/ACCEPTANCE.md).
- [ ] Verify deployment health checks, TLS configuration, secret delivery, logging, and cleanup scheduling in the target environment.

## Performance and release acceptance

- [x] Add a repeatable adjacent-tile load tool and run two-container cold and warm tests with synthetic 8 MB and 100 MB JP2s. The emulated `amd64` Docker/Moto runs achieved 276–407 requests/s across two containers with zero observed errors; details and limits are in [tests/LOAD.md](tests/LOAD.md).
- [ ] Repeat cold and warm tests on native Linux `amd64` with the release Kakadu SDK, representative JP2s, AWS S3, and several server instances. Record request rate, p50/p95/p99 latency, error rate, S3 calls per request, cache hit rate, CPU, and peak memory at multiple concurrency levels.
- [ ] Demonstrate the target of **thousands of requests per second across several instances** before treating it as achieved. The local Moto and emulation results are a development baseline only.
- [x] Benchmark Persimmon against the latest Cantaloupe release with Kakadu enabled. Record each server's exact Cantaloupe/Kakadu version, and use the same source images, tile requests, S3 setup, hardware, concurrency, and warm/cold phases. Publish throughput, latency, error rate, CPU, memory, and configuration in a Markdown comparison file in this repository. The local emulated comparison is in [tests/CANTALOUPE_COMPARISON.md](tests/CANTALOUPE_COMPARISON.md); native Linux and production S3 validation remain open above. The [latest Cantaloupe release](https://github.com/cantaloupe-project/cantaloupe/releases/latest) on 2026-09-30 was v5.0.7, which bundles Kakadu 8.4.1.
- [ ] Complete release validation of Level 2 behavior, outputs, private S3 access, purge, TLS, health, cleanup, and resource limits; document any remaining optional IIIF features.

## References

- [IIIF Image API 3.0 specification](https://iiif.io/api/image/3.0/)
- [IIIF Image API 3.0 compliance requirements](https://iiif.io/api/image/3.0/compliance/)
- [Cantaloupe Kakadu 8.7 pull request](https://github.com/cantaloupe-project/cantaloupe/pull/985)
- [Cantaloupe Kakadu dependency notes](https://github.com/cantaloupe-project/cantaloupe/blob/develop/dist/deps/README.md)
