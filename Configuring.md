# Configuring Persimmon

Persimmon is configured entirely with environment variables.

## Required

| Variable | Purpose |
| --- | --- |
| `PERSIMMON_PUBLIC_BASE_URL` | Public origin and optional deployment path, for example `https://images.example.edu` or `https://images.example.edu/iiif`. |
| `PERSIMMON_SOURCE_BUCKET` | Private S3 bucket containing sources. Identifier `a/b.jp2` resolves to key `a/b.jp2`; no extension is added. Identifiers with empty, `.`, or `..` path segments are rejected with `400`. |
| `PERSIMMON_CACHE_BUCKET` | Private S3 bucket for derivatives and purge markers. May be the same bucket as the source if prefixes and IAM policy keep them separate. |
| `PERSIMMON_ADMIN_USER` | Basic-auth admin username for `POST /admin/purge`. |
| `PERSIMMON_ADMIN_PASSWORD` | Basic-auth admin password, supplied as a runtime secret. |

The AWS SDK default credential chain supplies S3 credentials and region. An instance role or workload identity is preferred over static keys.

## Optional

| Variable | Default | Purpose |
| --- | --- | --- |
| `PERSIMMON_LISTEN` | `0.0.0.0:3000` | Listener address. |
| `PERSIMMON_IIIF_V3_PREFIX` | `/v3` | Path appended to the public base URL for all IIIF v3 endpoints. Set to `/` or an empty string to serve IIIF at the base path. |
| `PERSIMMON_TLS_CERT`, `PERSIMMON_TLS_KEY` | Unset | PEM certificate and key for native TLS; set both. Without them, put the server behind a trusted TLS proxy and use an HTTPS public base URL. |
| `PERSIMMON_CACHE_PREFIX` | `persimmon/` | S3 namespace for derivatives and purge markers. |
| `PERSIMMON_S3_ENDPOINT` | Unset | S3-compatible endpoint, including the release Weka service. Forces path-style requests. |
| `PERSIMMON_LOCAL_CACHE_DIR` | `/var/cache/persimmon` | Per-instance source cache directory. |
| `PERSIMMON_LOCAL_CACHE_BYTES` | `2000000000` | Local source cache target in bytes. Set to `0` to keep sources only while requests use them. |
| `PERSIMMON_DERIVATIVE_CACHE_BYTES` | `10000000000` | Shared S3 derivative cache target in bytes. |
| `PERSIMMON_PRUNE_INTERVAL_SECONDS` | `3600` | Seconds between passes when running `prune-cache-loop`. |
| `PERSIMMON_MAX_SOURCE_BYTES` | `250000000` | Largest accepted source file in bytes. Requests for a larger source fail with `500`, and the error message gives the source size. |
| `PERSIMMON_MAX_TEMP_BITMAP_BYTES` | `2000000000` | Per-instance budget for concurrent decode working memory, intermediate bitmaps, and newly encoded response buffers. A response retains its reservation until its final byte buffer is released. |
| `PERSIMMON_MAX_OUTPUT_PIXELS` | `100000000` | Largest output image area. Reported as `maxArea` in `info.json`. |
| `PERSIMMON_MAX_DECODE_PIXELS` | `100000000` | Largest decoded region area, after discarding JP2 resolution levels, accepted by the JP2 and JPEG XL decoders. Must be at least the output pixel limit. |
| `PERSIMMON_MAX_PARALLEL_DECODES` | `8` | Per-instance limit on concurrent decodes. Each decode uses one thread. |
| `PERSIMMON_MAX_PARALLEL_DOWNLOADS` | `8` | Per-instance source download limit. |
| `PERSIMMON_MAX_IN_FLIGHT_REQUESTS` | `64` | Per-instance limit on requests performing S3 or image work. Excess requests fail immediately with `503` and `Retry-After: 1`; liveness, metrics, and IIIF base redirects do not consume a slot. |
| `PERSIMMON_MIN_SIZE` | `64` | Minimum advertised `sizes` dimension. |
| `PERSIMMON_MIN_TILE_SIZE` | `1024` | Advertised tile width. |
| `PERSIMMON_JPEG_QUALITY` | `75` | JPEG quality, from `1` (smallest files) to `100` (best image). |
| `PERSIMMON_AVIF_QUALITY` | `80` | AVIF quality, from `1` (smallest files) to `100` (best image). |
| `PERSIMMON_AVIF_SPEED` | `10` | AVIF encoder speed, from `1` (slowest, smallest files) to `10` (fastest). Images are encoded on request, so slower settings add latency to every derivative cache miss. |
| `PERSIMMON_HEALTH_URL` | Selected from TLS settings | Optional URL override for the Docker health probe. With the default listener, the probe uses HTTPS when `PERSIMMON_TLS_CERT` is set and HTTP otherwise. Set this variable if the listener port changes. |

## Output encoding

PNG, WebP, and JPEG XL output is always lossless and has no quality setting. Changing a quality or speed setting does not re-encode derivatives already in the S3 cache; purge the affected images or change `PERSIMMON_CACHE_PREFIX` to start a fresh cache.

## URLs and identifiers

`PERSIMMON_IIIF_V3_PREFIX` changes only the IIIF routes; `/healthz`, `/livez`, `/metrics`, and `/admin/purge` stay at the root. Encode any slash inside an identifier as `%2F`. For example, with `PERSIMMON_PUBLIC_BASE_URL=https://images.example.edu/iiif` and the default prefix, the identifier `a/b` has information URL `/iiif/v3/a%2Fb/info.json`. When the public base URL includes a deployment path, forward that full path to the server.

## S3 permissions

Grant the server `s3:GetObject` on source objects and `s3:ListBucket` on the source bucket for health checks. Grant `s3:GetObject`, `s3:PutObject`, `s3:DeleteObject`, and `s3:ListBucket` on the configured cache prefix. The cleanup command needs the same cache permissions. Keep buckets private.

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

`prune-cache` and `prune-cache-loop` need only `PERSIMMON_CACHE_BUCKET` from the required variables, plus AWS region and credentials. They also honor the optional S3 endpoint, cache prefix, cache byte limit, and prune interval. The cleanup job does not need the source bucket, public URL, admin password, or TLS files.

## Caching and purges

Image and `info.json` responses allow downstream HTTP caches to keep them for one day. Serialized `info.json` documents share the S3 derivative cache and are scoped to the source generation and information-related configuration. An admin purge invalidates server caches; browsers and proxies may continue serving the previous response until their cached copy expires.

## Monitoring

`GET /healthz` returns `200` with the plain-text body `OK` when both S3 buckets can be queried. Otherwise it returns `503` with JSON giving an overall `status` and a result for each of `cache_bucket` and `source_bucket`; a failed check includes a short `error` such as an S3 error code (`AccessDenied`, `NoSuchBucket`), `could not connect to S3`, or `timed out`. Full error details are logged at `WARN` with the check name. Each check times out after three seconds, and results are cached for five seconds.

`GET /livez` returns `200` with the plain-text body `OK` whenever the server is accepting requests. It does not contact S3, so use it for liveness probes and `/healthz` for readiness: an S3 outage then takes instances out of the load balancer without restarting them.

`GET /metrics` returns per-instance Prometheus counters for S3 calls, source and derivative cache activity, rendering time, purges, and errors. It separates JP2 renders (`persimmon_j2k_renders_total`) from JPEG XL renders (`persimmon_jxl_renders_total`). The S3 request counter counts SDK calls made by this service; retries inside the SDK are not counted. Restrict this route at the load balancer if the metrics should remain internal.

Structured access logs at the `info` level include request method, URI, response status, and latency. Valid IIIF requests also include an `identifier` field containing the decoded source key (for example, `a%2Fb.jp2` logs as `a/b.jp2`).

## Error reporting

Builds with the optional `honeybadger` Cargo feature report server errors (`5xx` responses other than `501`) and panics to [Honeybadger](https://www.honeybadger.io). Reporting starts only when `HONEYBADGER_API_KEY` is set, so the same binary also runs without it. Each notice includes the request method, path, and IIIF identifier in its context. Notices are sent from a background thread and never delay a response.

| Variable | Default | Purpose |
| --- | --- | --- |
| `HONEYBADGER_API_KEY` | Unset | Project API key. Reporting is off without it. |
| `HONEYBADGER_ENV` | Unset | Environment name shown in Honeybadger. The SDK does not report from `development` or `test` unless `HONEYBADGER_ENABLED=true`. |
| `HONEYBADGER_REVISION` | Build argument `PERSIMMON_REVISION` | Source revision attached to each notice. |

The SDK reads its other `HONEYBADGER_*` settings, such as `HONEYBADGER_ENDPOINT`, from the environment as well. Requests shed under load (`503` from the in-flight request or temporary bitmap limit) are not reported.
