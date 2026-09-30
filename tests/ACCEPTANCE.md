# Local two-container acceptance check

On 2026-09-30, two Linux `amd64` Docker containers built with licensed Kakadu 8.6.2 served HTTPS on localhost ports 3001 and 3002. Both used native Kakadu decoding and shared two private, disposable Moto S3 buckets. The TLS certificate was self-signed and accepted only for this local test.

The source bucket initially contained the CC0 validator fixture as `a/b.jp2` (1000×1000). Both containers returned `OK` from `/healthz`, gave `https://127.0.0.1:<port>/v3/a%2Fb` as the information `id`, and returned nonempty JPEG, PNG, and WebP images with the expected content types. The encoded slash in the URL resolved to `a/b.jp2` in S3.

The purge endpoint returned 401 with no credentials and with an incorrect password. The source object was then replaced with a synthetic 64×64 JP2. Both containers continued to report the cached 1000×1000 source before purge. One authenticated `POST /admin/purge` for `a/b` returned 200. Subsequent information and JPEG requests on **both** containers reported or served the new 64×64 source. The script finished with `ACCEPTANCE PASS`.

This verifies local behavior across instances. Moto does not verify AWS IAM, AWS S3 failure handling, production TLS certificate management, or throughput on native Linux hardware; those remain unchecked in [PLAN.md](../PLAN.md).

## Synthetic grayscale JP2

On 2026-09-30, `generate_synthetic_jp2.py --channels 1 --width 512 --height 512 --rate 4` produced a 131,088-byte one-channel JP2 using Kakadu 8.6.2. A Linux `amd64` Docker container with the native adapter read it through Moto S3. `info.json` reported 512×512 and `extraQualities: ["gray"]`. JPEG and WebP requests succeeded. A `128,64,256,128/128,/90/gray.png` request returned a one-channel 64×128 PNG. Metrics recorded three native renders and zero command fallbacks. This covers a synthetic grayscale case; representative archival grayscale files are still needed for release validation.

## Streamed derivative response

On 2026-09-30, the updated Docker image served a 500 px wide JPEG from the CC0 validator fixture. A cold render and an S3 cache hit returned identical 29,568-byte images with matching SHA-256 hashes. A `HEAD` request returned an empty body and the same `Content-Length` as both `GET` responses. Metrics showed one native render and two derivative cache hits (the warm `GET` and `HEAD`). The cache-hit HTTP body now forwards S3 chunks as they arrive. This local check does not measure production memory use or throughput.

## Configurable IIIF prefix

On 2026-09-30, two Linux `amd64` Docker containers used the same Moto JP2 source with `PERSIMMON_PUBLIC_BASE_URL` ending in `/images`. One used the default `/v3` IIIF prefix; the other used an empty prefix. Both returned correct base-URI redirects, `info.json` IDs, and JPEG images for the encoded-slash identifier `a%2Fb`. The neighboring paths `/images/v30/...` and `/imagesx/...` returned 404. This also verifies that a prefix matches whole path segments rather than merely the start of a string.
