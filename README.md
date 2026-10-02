# Persimmon

Persimmon is a Rust IIIF Image API 3.0 server for private JP2 images in S3. The intended behavior and acceptance criteria are in [PLAN.md](PLAN.md).

## Current implementation

The server parses IIIF Image API 3.0 routes, reads JP2 sources from S3, caches local source files and S3 derivatives, handles generation-based cache purges, and encodes JPEG, PNG, and WebP. Cached derivatives stream from S3 to the client without being collected into one in-memory buffer. Newly rendered outputs are encoded in memory before their S3 write and HTTP response. The server has a TLS listener, an authenticated purge endpoint, and `/healthz`.

**The project is still under development.** It passed the upstream IIIF v3 Level 2 validator against one small JP2 fixture, but has not completed release conformance validation or a production multi-instance load test. The native adapter decodes regions at Kakadu resolution levels before Rust applies final scaling and encoding. Requests whose reduced decode exceeds the native limit, or whose component geometry is unsupported by the adapter, attempt the Kakadu `kdu_expand` command fallback. The fallback has its own decoded-pixel limit and can also reject a large request. The adapter still needs validation with the release Kakadu SDK and concurrent Linux load tests. Do not claim production Level 2 compliance or the throughput target from the current code alone.

The [IIIF Image API validator](https://github.com/IIIF/image-validator) ran against the local server with its [CC0 JP2 test image](https://iiif.io/api/image/validator/download/), supplied in [tests/fixtures/iiif-validator.jp2](tests/fixtures/iiif-validator.jp2). The updated upstream validator passed all 31 selected v3 Level 2 checks against the Linux `amd64` Docker image with the Kakadu 8.6.2 native adapter. The exact revision and output are documented in [tests/VALIDATOR.md](tests/VALIDATOR.md); validation with representative source images and the final release build is still required.

The optional S3 integration test covers concurrent reuse of one local source, local LRU eviction, source replacement after purge on two instances, shared derivative invalidation, and cache pruning. Run a local S3-compatible test server such as Moto, then use `PERSIMMON_TEST_S3_ENDPOINT=http://127.0.0.1:5001 cargo test --test cache_integration -- --ignored`. The test creates its own uniquely named buckets.

Local source files in active use are retained until their requests finish. If this briefly takes the source cache over its limit, release of the last active lease triggers LRU eviction back toward the configured limit. A source larger than the limit can still be served, then is removed after its request finishes. Set `PERSIMMON_LOCAL_CACHE_BYTES=0` to keep sources only while requests use them.

The reproducible adjacent-tile benchmark and two-instance development baselines are in [tests/LOAD.md](tests/LOAD.md). They include native Kakadu 8.6.2 Docker runs with synthetic 8 MB and 100 MB JP2 files and local Moto S3. Production performance remains unverified. The two-container TLS, authentication, and purge check is in [tests/ACCEPTANCE.md](tests/ACCEPTANCE.md).

## Build

For local Rust checks:

```sh
cargo test
cargo build --release
```

The Docker image requires a private BuildKit context pointing to the root of a licensed Kakadu 8.6 or newer SDK source tree. The build compiles `kdu_expand` and the native adapter for Linux `amd64`. The available local 8.6.2 SDK is supported for development and runtime use; validate the exact SDK build chosen for release.

Build the image with:

```sh
docker buildx build --platform linux/amd64 \
  --build-context kakadu=/path/to/licensed-kakadu-8.6-or-newer-sdk \
  --load -t persimmon:dev .
```

No Kakadu SDK binaries, headers, or credentials belong in this repository. The build container requires production-licensed Stanford assets.

## Local Moto S3

Use [Moto server mode](https://docs.getmoto.org/en/5.1.3/docs/server_mode.html) to try the image server without AWS. From the repository root, install Moto and Boto3 in a disposable virtual environment, then start Moto in one terminal:

```sh
python3 -m venv /tmp/persimmon-moto
/tmp/persimmon-moto/bin/python -m pip install 'moto[server]' boto3
/tmp/persimmon-moto/bin/moto_server -H 0.0.0.0 -p 5001
```

In another terminal, still at the repository root, create separate source and derivative-cache buckets and upload the included CC0 JP2 test image:

```sh
/tmp/persimmon-moto/bin/python - <<'PY'
import boto3

s3 = boto3.client(
    "s3",
    endpoint_url="http://127.0.0.1:5001",
    region_name="us-east-1",
    aws_access_key_id="test",
    aws_secret_access_key="test",
)
for bucket in ("persimmon-local-source", "persimmon-local-cache"):
    s3.create_bucket(Bucket=bucket)
s3.upload_file(
    "tests/fixtures/iiif-validator.jp2",
    "persimmon-local-source",
    "example.jp2",
)
print("Uploaded example.jp2")
PY
```

To upload your own image, replace the first `upload_file` argument with its local JP2 path and the third argument with `<identifier>.jp2`. For example, key `my-image.jp2` is requested using identifier `my-image`.

For a Persimmon container on Docker Desktop, set `PERSIMMON_S3_ENDPOINT=http://host.docker.internal:5001`, `PERSIMMON_SOURCE_BUCKET=persimmon-local-source`, `PERSIMMON_CACHE_BUCKET=persimmon-local-cache`, `AWS_REGION=us-east-1`, and dummy `AWS_ACCESS_KEY_ID=test` and `AWS_SECRET_ACCESS_KEY=test`. Also set the public URL and admin credentials described below, and configure native TLS or a trusted HTTPS proxy. On Linux Docker, add `--add-host=host.docker.internal:host-gateway` to `docker run` to make the same endpoint name available. The uploaded image is requested as identifier `example`, for example `/v3/example/info.json`; Persimmon appends `.jp2` when finding its S3 object. For an identifier containing a slash, upload key `a/b.jp2` and request `a%2Fb`.

Moto keeps this test data only while that server process runs. Stop it with Ctrl-C when finished. Binding Moto to `0.0.0.0` allows the container to reach it, so use this setup on a trusted local machine.

## Configuration

Required environment variables:

| Variable | Purpose |
| --- | --- |
| `PERSIMMON_PUBLIC_BASE_URL` | Public origin and optional deployment path, for example `https://images.example.edu` or `https://images.example.edu/iiif`. |
| `PERSIMMON_SOURCE_BUCKET` | Private S3 bucket containing sources. Identifier `a/b` resolves to `a/b.jp2`. |
| `PERSIMMON_CACHE_BUCKET` | Private S3 bucket for derivatives and purge markers. May be the same bucket as the source if prefixes and IAM policy keep them separate. |
| `PERSIMMON_ADMIN_USER` | Basic-auth admin username for `POST /admin/purge`. |
| `PERSIMMON_ADMIN_PASSWORD` | Basic-auth admin password, supplied as a runtime secret. |

The AWS SDK default credential chain supplies S3 credentials and region. An instance role or workload identity is preferred over static keys.

`prune-cache` and `prune-cache-loop` need only `PERSIMMON_CACHE_BUCKET` from the required-variable table, plus AWS region/credentials. They also honor the optional S3 endpoint, cache prefix, cache byte limit, and prune interval. The cleanup job does not need the source bucket, public URL, admin password, TLS files, or Kakadu settings.

Optional environment variables:

| Variable | Default | Purpose |
| --- | --- | --- |
| `PERSIMMON_LISTEN` | `0.0.0.0:3000` | Listener address. |
| `PERSIMMON_IIIF_V3_PREFIX` | `/v3` | Path appended to the public base URL for all IIIF v3 endpoints. Set to `/` or an empty string to serve IIIF at the base path. |
| `PERSIMMON_TLS_CERT`, `PERSIMMON_TLS_KEY` | Unset | PEM certificate and key for native TLS; set both. Without them, put the server behind a trusted TLS proxy and use an HTTPS public base URL. |
| `PERSIMMON_CACHE_PREFIX` | `persimmon/` | S3 namespace for derivatives and purge markers. |
| `PERSIMMON_S3_ENDPOINT` | Unset | Optional S3-compatible endpoint for local tests. Forces path-style requests. |
| `PERSIMMON_LOCAL_CACHE_DIR` | `/var/cache/persimmon` | Per-instance JP2 cache and temporary bitmap directory. |
| `PERSIMMON_LOCAL_CACHE_BYTES` | `2000000000` | Local source cache target in bytes. |
| `PERSIMMON_DERIVATIVE_CACHE_BYTES` | `10000000000` | Shared S3 derivative cache target in bytes. |
| `PERSIMMON_PRUNE_INTERVAL_SECONDS` | `3600` | Seconds between passes when running `prune-cache-loop`. |
| `PERSIMMON_MAX_SOURCE_BYTES` | `250000000` | Largest accepted JP2 file in bytes. |
| `PERSIMMON_MAX_TEMP_BITMAP_BYTES` | `2000000000` | Per-instance budget for concurrent decode, intermediate bitmap, and newly encoded response buffers. A response retains its reservation until its final byte buffer is released. |
| `PERSIMMON_MAX_OUTPUT_PIXELS` | `100000000` | Largest output image area. Reported as `maxArea` in `info.json`. |
| `PERSIMMON_MAX_DECODE_PIXELS` | `100000000` | Largest decoded region area accepted by the `kdu_expand` fallback. Must be at least the output pixel limit. |
| `PERSIMMON_MAX_NATIVE_DECODE_PIXELS` | `10000000` | Largest in-memory regional decode buffer in pixels after Kakadu resolution reduction. The server attempts `kdu_expand` if this limit or the adapter's supported component geometry is exceeded. |
| `PERSIMMON_MAX_PARALLEL_DECODES` | `8` | Per-instance Kakadu process limit. |
| `PERSIMMON_MAX_PARALLEL_DOWNLOADS` | `8` | Per-instance source download limit. |
| `PERSIMMON_MIN_SIZE` | `64` | Minimum advertised `sizes` dimension. |
| `PERSIMMON_MIN_TILE_SIZE` | `1024` | Advertised tile width. |
| `PERSIMMON_KDU_EXPAND` | `kdu_expand` | Path to Kakadu 8.6 or newer executable used by the command backend or fallback. |
| `PERSIMMON_KAKADU_NATIVE` | Unset | Path to the Persimmon adapter shared library linked against Kakadu 8.6 or newer. When set, eligible regional requests use the adapter. |
| `PERSIMMON_HEALTH_URL` | Selected from TLS settings | Optional URL override for the Docker health probe. With the default listener, the probe uses HTTPS when `PERSIMMON_TLS_CERT` is set and HTTP otherwise. Set this variable if the listener port changes. |

Grant the server `s3:GetObject` on source objects and `s3:ListBucket` on the source bucket for health checks. Grant `s3:GetObject`, `s3:PutObject`, `s3:DeleteObject`, and `s3:ListBucket` on the configured cache prefix. The cleanup command needs the same cache permissions. Keep buckets private.

`kdu_expand` uses Kakadu's core shared library (`libkdu_v*.so`); it does not use Persimmon's `libpersimmon_kakadu.so` adapter. The adapter includes Kakadu core code from `libkdu.a` and caps its own in-memory decode buffer at `PERSIMMON_MAX_NATIVE_DECODE_PIXELS`. The separate `kdu_expand` program writes an intermediate bitmap to disk and applies `PERSIMMON_MAX_DECODE_PIXELS`. The fallback can therefore handle some larger regions, at the cost of process startup and temporary disk use. Requests beyond both limits fail; `/metrics` exposes how often the fallback is used.

## Endpoints

- `GET /healthz` returns `OK` when both S3 buckets can be queried and Kakadu is available. Dependency checks are cached for five seconds.
- `GET /metrics` returns per-instance Prometheus counters for S3 calls, source and derivative cache activity, rendering time, purges, and errors. Restrict this route at the load balancer if the metrics should remain internal.
- `GET /v3/{identifier}/info.json` returns IIIF image information. It serves JSON-LD by default and `application/json` when requested with `Accept`; responses include `Vary: Accept`.
- `GET /v3/{identifier}/{region}/{size}/{rotation}/{quality}.{format}` returns an image.
- `GET /v3/{identifier}` redirects to its `info.json`.
- `POST /admin/purge` with Basic auth and JSON body `{"identifier":"a/b"}` changes the purge generation for that identifier. Old derivatives are deleted in the background.

The IIIF paths above use the default `/v3` prefix. `PERSIMMON_IIIF_V3_PREFIX` changes only the IIIF routes; `/healthz`, `/metrics`, and `/admin/purge` stay at the root. Encode any slash inside an identifier as `%2F`. For example, with `PERSIMMON_PUBLIC_BASE_URL=https://images.example.edu/iiif` and the default prefix, the identifier `a/b` has information URL `/iiif/v3/a%2Fb/info.json`. When the public base URL includes a deployment path, forward that full path to the server.

## Cache cleanup

Run one designated cleanup job periodically using the same image and cache-bucket configuration:

```sh
persimmon prune-cache
```

Or run one designated long-lived cleanup process. It prunes immediately, then repeats at `PERSIMMON_PRUNE_INTERVAL_SECONDS`:

```sh
persimmon prune-cache-loop
```

The job lists derivatives, totals their sizes, and deletes the oldest objects until the configured target is met. It is creation-time eviction, not LRU. The limit is soft: concurrent writes can temporarily exceed it between runs.

Image responses allow downstream HTTP caches to keep them for one day. An admin purge invalidates server caches; browsers and proxies may continue serving the previous response until their cached copy expires.

Request latency is recorded in structured access logs at the `info` level. `/metrics` separates native renders, direct command renders, and command fallback renders to support load analysis. The S3 request counter counts SDK calls made by this service; retries inside the SDK are not counted.
