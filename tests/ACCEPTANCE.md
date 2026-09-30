# Local two-container acceptance check

On 2026-09-30, two Linux `amd64` Docker containers built with licensed Kakadu 8.6.2 served HTTPS on localhost ports 3001 and 3002. Both used native Kakadu decoding and shared two private, disposable Moto S3 buckets. The TLS certificate was self-signed and accepted only for this local test.

The source bucket initially contained the CC0 validator fixture as `a/b.jp2` (1000×1000). Both containers returned `OK` from `/healthz`, gave `https://127.0.0.1:<port>/v3/a%2Fb` as the information `id`, and returned nonempty JPEG, PNG, and WebP images with the expected content types. The encoded slash in the URL resolved to `a/b.jp2` in S3.

The purge endpoint returned 401 with no credentials and with an incorrect password. The source object was then replaced with a synthetic 64×64 JP2. Both containers continued to report the cached 1000×1000 source before purge. One authenticated `POST /admin/purge` for `a/b` returned 200. Subsequent information and JPEG requests on **both** containers reported or served the new 64×64 source. The script finished with `ACCEPTANCE PASS`.

This verifies local behavior across instances. Moto does not verify AWS IAM, AWS S3 failure handling, production TLS certificate management, or throughput on native Linux hardware; those remain unchecked in [PLAN.md](../PLAN.md).

## Docker health-probe URL selection

On 2026-09-30, the updated Linux `amd64` image built with Kakadu 8.6.2. Its actual Docker `HEALTHCHECK` ran in disposable containers with a stub `curl` that recorded the URL. Docker reported each container healthy. With `PERSIMMON_TLS_CERT` set and no URL override, the probe used `https://127.0.0.1:3000/healthz`; without TLS settings it used `http://127.0.0.1:3000/healthz`; and `PERSIMMON_HEALTH_URL=https://example.test/custom` took precedence over both defaults. This checks URL selection; the two-container check above exercised a real HTTPS health endpoint against local Moto.

## Synthetic grayscale JP2

On 2026-09-30, `generate_synthetic_jp2.py --channels 1 --width 512 --height 512 --rate 4` produced a 131,088-byte one-channel JP2 using Kakadu 8.6.2. A Linux `amd64` Docker container with the native adapter read it through Moto S3. `info.json` reported 512×512. JPEG and WebP requests succeeded. A `128,64,256,128/128,/90/gray.png` request returned a one-channel 64×128 PNG. Metrics recorded three native renders and zero command fallbacks. The server now advertises both supported qualities, `color` and `gray`, for this source. This covers a synthetic grayscale case; representative archival grayscale files are still needed for release validation.

The rebuilt image with the quality-advertisement fix served the same fixture from local Moto. `info.json` listed `extraQualities: ["color", "gray"]`; `/full/256,/0/color.jpg` returned a 256×256 three-component JPEG, and `/full/256,/0/gray.png` returned a 256×256 grayscale PNG. `/healthz` returned `OK`, Docker reported healthy, and metrics showed two native renders, zero command fallbacks, and zero errors.

## Cleanup job configuration

On 2026-09-30, `target/debug/persimmon prune-cache` ran against a disposable Moto bucket with the public URL, source bucket, admin credentials, TLS paths, and Kakadu paths explicitly unset. It needed only AWS region/test credentials, cache bucket, local S3 endpoint, and a 5-byte cache target. Of two 4-byte derivatives, it deleted the older one and retained the newer one; the purge marker remained. Its structured log reported `bytes=4` and `deleted=1`. This verifies the cleanup job can use a smaller runtime secret set than the HTTP server; AWS IAM behavior remains unverified.

## Streamed derivative response

On 2026-09-30, the updated Docker image served a 500 px wide JPEG from the CC0 validator fixture. A cold render and an S3 cache hit returned identical 29,568-byte images with matching SHA-256 hashes. A `HEAD` request returned an empty body and the same `Content-Length` as both `GET` responses. Metrics showed one native render and two derivative cache hits (the warm `GET` and `HEAD`). The cache-hit HTTP body now forwards S3 chunks as they arrive. This local check does not measure production memory use or throughput.

## Configurable IIIF prefix

On 2026-09-30, two Linux `amd64` Docker containers used the same Moto JP2 source with `PERSIMMON_PUBLIC_BASE_URL` ending in `/images`. One used the default `/v3` IIIF prefix; the other used an empty prefix. Both returned correct base-URI redirects, `info.json` IDs, and JPEG images for the encoded-slash identifier `a%2Fb`. The neighboring paths `/images/v30/...` and `/imagesx/...` returned 404. This also verifies that a prefix matches whole path segments rather than merely the start of a string.

## Advertised sizes and tiles

On 2026-09-30, `check_advertised.py` verified a synthetic 4096×4096 JP2 with the default 1024 px advertised tile width and the 1000×1000 validator JP2 with a configured 256 px tile width. All 7 and 4 advertised full-image sizes, respectively, returned JPEGs with the declared dimensions. For every listed tile scale factor (`1`, `2`, and `4`), a top-left and bottom-right edge tile returned the expected dimensions. The 4096 px test used the command fallback for its largest full-image request; metrics showed one fallback, nine native renders, and zero errors. The 1000 px test showed seven native renders and zero errors.

Run the checker against a service id from `info.json`:

```sh
python3 tests/check_advertised.py --service-url https://images.example.edu/v3/example
```

Add `--insecure` only for a local self-signed test certificate. The tool streams response bodies so a large advertised image does not fill client memory. These fixture checks do not replace validation with representative archival JP2s.
