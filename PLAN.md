# Persimmon implementation plan

Persimmon is a Rust IIIF Image API 3.0 server for private JPEG 2000 images in S3. The first release aims for minimum Level 2 compliance, with WebP as an extra output format and Kakadu 8.6 or newer for decoding. It runs as a Linux `amd64` Docker image. The public IIIF prefix defaults to `/v3`; routing is structured so v2 can be added later.

This is a progress checklist. A checked item means the feature is implemented and has the local verification described below. It does **not** imply production acceptance. Unchecked items are still required before claiming the release or throughput target is complete.

The test deployment and its first known image URL are recorded in [tests/DEPLOYMENT.md](tests/DEPLOYMENT.md). The completed deployment checks below ran against image tag `7db27dc4246e`, confirmed on the running web and pruner containers by `kamal details` on 2026-10-06. The deployment has not yet cleared the unchecked release items.

## IIIF API

- [x] Parse v3 image and `info.json` routes, including percent-encoded slashes in identifiers, with a configurable prefix and public base URL. Verify both `/images/v3` and an empty IIIF prefix under `/images` in Docker; see [tests/ACCEPTANCE.md](tests/ACCEPTANCE.md).
- [x] Implement Level 2 region and size forms, right-angle rotations, and `default`, `color`, and `gray` qualities. Apply transformations in IIIF order. Reject unsupported optional operations without advertising them.
- [x] Encode JPEG and PNG as required by Level 2, plus WebP; return matching MIME types and dimensions.
- [x] Generate `info.json` with dimensions, profile, capabilities, `sizes`, and `tiles`. Make the minimum advertised size (64 px) and tile size (1024 px) configurable. Verify all advertised sizes and edge tiles at each scale on 1000 px and 4096 px fixtures; see [tests/ACCEPTANCE.md](tests/ACCEPTANCE.md).
- [x] Implement base URI redirects, CORS, JSON-LD information responses, `GET`, `HEAD`, `OPTIONS`, cache headers, and IIIF error responses.
- [x] Run the official IIIF Image API 3.0 Level 2 validator against the deployed CC0 fixture: 31/31 tests passed with zero failures; see [tests/VALIDATOR.md](tests/VALIDATOR.md).
- [x] Verify one deployed archival image through the public route: redirect, `info.json`, JPEG/PNG/WebP, `HEAD`, cache headers, CORS, invalid-identifier rejection, all 6 advertised sizes, and 7 selected tiles passed for the 6048 × 4024 source; see [tests/DEPLOYMENT.md](tests/DEPLOYMENT.md).
- [x] Validate Level 2 behavior for the currently available color-source scope: the deployed CC0 fixture passed 31/31 official checks, and the 6048 × 4024 archival image passed the public output checks above. A representative archival grayscale source is not currently available; the synthetic grayscale check remains documented in [tests/ACCEPTANCE.md](tests/ACCEPTANCE.md).

## Kakadu and image processing

- [x] Build `kdu_expand` and a C++ C-ABI regional decoder from a privately supplied licensed Kakadu 8.6+ SDK. The Docker build uses a named `kakadu` build context; no SDK files are in the repository. The available local SDK is Kakadu 8.6.2 at `/Users/jcoyne85/Desktop/kakadu-v8_6_2-02138L`.
- [x] Decode JP2 regions at reduced resolution in memory with the native adapter, then perform final transforms and output encoding in Rust. Bound concurrent decode and memory use. Use `kdu_expand` as a separate fallback with its own decoded-pixel and temporary-bitmap limits.
- [x] Compare native output against Kakadu command output on local fixtures and verify native rendering in a Linux `amd64` Docker container. Benchmark adjacent tiles with synthetic 8 MB and 100 MB JP2 files; see [tests/LOAD.md](tests/LOAD.md).
- [x] Check a synthetic one-channel JP2 through native Linux Docker decoding: `info.json`, JPEG, WebP, and a rotated grayscale PNG returned correctly with zero command fallbacks; see [tests/ACCEPTANCE.md](tests/ACCEPTANCE.md).
- [x] Observe native decoding on the Weka-backed test deployment: `/metrics` reported 32 native renders, zero command fallbacks, and zero errors after the public and validator checks; see [tests/DEPLOYMENT.md](tests/DEPLOYMENT.md).
- [ ] Validate concurrency, memory peaks, and fallback behavior with the **exact Kakadu SDK chosen for release** and representative archival color JP2 files. Synthetic sources are useful load fixtures but do not represent archival compression or component layouts.

## S3 sources and caches

- [x] Read private S3 source objects through the AWS credential chain. Decode an identifier once and use it unchanged as its S3 key; `a%2Fb.jp2` resolves to `a/b.jp2`. Never use the identifier as a local path.
- [x] Keep a per-instance source cache with a configurable 2 GB default, LRU eviction, coalesced simultaneous downloads, and source leases that prevent eviction while in use. An active lease can temporarily put the cache over its target; eviction runs when it is released.
- [x] Store generated images in a shared private S3 cache keyed by API version, identifier, request, format, and purge generation. Coalesce duplicate rendering within an instance and avoid overwriting another instance's result with conditional writes.
- [x] Stream cache-hit derivatives from S3 to HTTP clients without buffering the full object. Verify cold and warm bytes match and `HEAD` retains the correct `Content-Length` locally; see [tests/ACCEPTANCE.md](tests/ACCEPTANCE.md).
- [x] Keep each newly rendered image's memory reservation until its S3 write and HTTP response release their final reference to the encoded bytes. Test permit lifetime across cloned response bytes.
- [x] Provide a designated `prune-cache` command and `prune-cache-loop`. They delete oldest derivatives until usage is below the configurable 10 GB target. The target is soft between cleanup runs; purge markers are outside the pruned prefix.
- [x] Implement generation markers in S3. Every request checks the marker before its cache lookup, and rendering rechecks before writing. An authenticated purge changes the generation across instances and removes old derivatives in the background. Source replacement at an existing key requires a purge.
- [x] Allow downstream HTTP caches to retain old responses until their normal one-day expiry after purge.
- [x] Test concurrent source reuse, local LRU eviction, source replacement and shared derivative invalidation across two instances, and cache pruning against a local S3-compatible service.
- [x] Observe deployed Weka-backed source and derivative use: after public and validator requests, `/metrics` reported 2 source downloads, 32 derivative writes, 23 derivative hits, and zero errors; see [tests/DEPLOYMENT.md](tests/DEPLOYMENT.md).
- [x] Verify authenticated purge on the deployed Weka-backed instance with the disposable `validator` key: no-auth returned 401; authenticated purge changed generation; the next image request regenerated a derivative; errors stayed at zero; see [tests/DEPLOYMENT.md](tests/DEPLOYMENT.md).
- [x] Verify the deployed Weka cache-pruner loop runs hourly: structured logs showed successful passes at 17:36 and 18:36 UTC, each counting 5,010,356 bytes and deleting 0 objects under the 200 GB target; see [tests/DEPLOYMENT.md](tests/DEPLOYMENT.md).
- [ ] Verify cross-instance purge propagation, over-limit cache deletion, and permission/failure behavior against Stanford Weka's S3-compatible service. The deployment currently has one configured web host; the successful under-limit cleanup passes do not exercise deletion.

## Operations and security

- [x] Provide `POST /admin/purge` with HTTP Basic authentication, constant-time credential comparison, and runtime-supplied credentials. Require native TLS or an HTTPS public base URL behind a trusted TLS proxy before starting with admin auth.
- [x] Support native TLS via certificate and key files. Provide `GET /healthz` with plain-text `OK` when Kakadu and required S3 buckets are available, plus a Docker `HEALTHCHECK`.
- [x] Expose `/metrics` and structured request logs covering latency, S3/cache activity, decode backend, rendering, evictions, errors, and purges. Document environment variables and minimal S3 permissions in [README.md](README.md).
- [x] Build and run a Linux `amd64` Docker image with the licensed Kakadu 8.6.2 SDK supplied as a private build context.
- [x] Run and document an end-to-end, two-container check of native TLS, private S3 lookup for an encoded-slash identifier, admin authentication, and purge propagation against local Moto. See [tests/ACCEPTANCE.md](tests/ACCEPTANCE.md).
- [x] Check the deployed public `/healthz` endpoint: HTTP/2 200 with `OK` and `Cache-Control: no-store` on 2026-10-06; see [tests/DEPLOYMENT.md](tests/DEPLOYMENT.md).
- [x] Review the test deployment configuration in `persimmon-kamal` at commit `ec08cca`: Kamal proxy TLS and `/healthz` probe, Vault-sourced S3 secrets, and a designated `prune-cache-loop` role are configured; see [tests/DEPLOYMENT.md](tests/DEPLOYMENT.md).
- [x] Confirm the deployed Kamal roles are running: `kamal details` showed the proxy on ports 80 and 443 and healthy web and pruner containers on image tag `7db27dc4246e`; see [tests/DEPLOYMENT.md](tests/DEPLOYMENT.md).
- [x] Verify deployment operations in the test environment: public HTTPS, `/healthz`, Vault-backed S3 and admin secret delivery, healthy Kamal web/pruner roles, hourly cleanup logs, and structured web request logs with status and latency were observed; see [tests/DEPLOYMENT.md](tests/DEPLOYMENT.md). External log-sink delivery and certificate renewal remain outside this check.

## Performance and release acceptance

- [x] Add a repeatable adjacent-tile load tool and run two-container cold and warm tests with synthetic 8 MB and 100 MB JP2s. The emulated `amd64` Docker/Moto runs achieved 276–407 requests/s across two containers with zero observed errors; details and limits are in [tests/LOAD.md](tests/LOAD.md).
- [x] Run Weka-backed, single-instance archival color baselines: at 8 connections, initially uncached derivatives served 113.45 requests/s and warm derivatives 130.54 requests/s; at 32 and 64 connections, warm derivatives served 520.84 and 1013.51 requests/s. All had zero errors and zero command fallbacks. After the 64-connection run, the web process reported 63,444 kB peak RSS since startup; see [tests/LOAD.md](tests/LOAD.md).
- [ ] Repeat cold and warm tests on native Linux `amd64` with the release Kakadu SDK, representative JP2s, Weka S3, and several server instances. Record request rate, p50/p95/p99 latency, error rate, S3 calls per request, cache hit rate, CPU, and peak memory at multiple concurrency levels.
- [ ] Demonstrate the target of **thousands of requests per second across several instances** before treating it as achieved. The local Moto and emulation results are a development baseline only.
- [x] Benchmark Persimmon against the latest Cantaloupe release with Kakadu enabled. Record each server's exact Cantaloupe/Kakadu version, and use the same source images, tile requests, S3 setup, hardware, concurrency, and warm/cold phases. Publish throughput, latency, error rate, CPU, memory, and configuration in a Markdown comparison file in this repository. The local emulated comparison is in [tests/CANTALOUPE_COMPARISON.md](tests/CANTALOUPE_COMPARISON.md); native Linux and production S3 validation remain open above. The [latest Cantaloupe release](https://github.com/cantaloupe-project/cantaloupe/releases/latest) on 2026-09-30 was v5.0.7, which bundles Kakadu 8.4.1.
- [ ] Complete release validation of Level 2 behavior, outputs, private S3 access, purge, TLS, health, cleanup, and resource limits; document any remaining optional IIIF features.

## References

- [IIIF Image API 3.0 specification](https://iiif.io/api/image/3.0/)
- [IIIF Image API 3.0 compliance requirements](https://iiif.io/api/image/3.0/compliance/)
- [Cantaloupe Kakadu 8.7 pull request](https://github.com/cantaloupe-project/cantaloupe/pull/985)
- [Cantaloupe Kakadu dependency notes](https://github.com/cantaloupe-project/cantaloupe/blob/develop/dist/deps/README.md)
