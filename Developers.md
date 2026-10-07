# Developer notes

## Project status

**Persimmon is still under development.** It passes all 31 IIIF Image API v3 Level 2 checks from the [IIIF validator](https://github.com/IIIF/image-validator), both locally and on the test deployment, which runs on Weka S3. Details are in [tests/VALIDATOR.md](tests/VALIDATOR.md). However, it hasn't yet been tested with real archival images at scale, under production load across several instances, or for cross-instance purges and cache deletion on Weka. Until those checks pass, don't treat it as production-ready. The remaining work is the unchecked items in [PLAN.md](PLAN.md).

## How rendering works

Cached derivatives stream from S3 to the client without being collected into one in-memory buffer. Newly rendered outputs are encoded in memory before their S3 write and HTTP response.

`kdu_expand` uses Kakadu's core shared library (`libkdu_v*.so`); it does not use Persimmon's `libpersimmon_kakadu.so` adapter. The adapter includes Kakadu core code from `libkdu.a` and caps its own in-memory decode buffer at `PERSIMMON_MAX_NATIVE_DECODE_PIXELS`. The separate `kdu_expand` program writes an intermediate bitmap to disk and applies `PERSIMMON_MAX_DECODE_PIXELS`. The fallback can therefore handle some larger regions, at the cost of process startup and temporary disk use. Requests beyond both limits fail; `/metrics` exposes how often the fallback is used.

Local source files in active use are retained until their requests finish. If this briefly takes the source cache over its limit, release of the last active lease triggers LRU eviction back toward the configured limit. A source larger than the limit can still be served, then is removed after its request finishes.

## Running locally with Moto S3

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

To upload your own image, replace the first `upload_file` argument with its local JP2 path and the third argument with the identifier. The identifier is the complete S3 key, so key `my-image.jp2` is requested using identifier `my-image.jp2`.

For a Persimmon container on Docker Desktop, set `PERSIMMON_S3_ENDPOINT=http://host.docker.internal:5001`, `PERSIMMON_SOURCE_BUCKET=persimmon-local-source`, `PERSIMMON_CACHE_BUCKET=persimmon-local-cache`, `AWS_REGION=us-east-1`, and dummy `AWS_ACCESS_KEY_ID=test` and `AWS_SECRET_ACCESS_KEY=test`. Also set the public URL and admin credentials described in [Configuring.md](Configuring.md), and configure native TLS or a trusted HTTPS proxy. On Linux Docker, add `--add-host=host.docker.internal:host-gateway` to `docker run` to make the same endpoint name available. The uploaded image is requested as identifier `example.jp2`, for example `/v3/example.jp2/info.json`; the identifier is used unchanged as its S3 key. For a key containing a slash, such as `a/b.jp2`, request `a%2Fb.jp2`.

Moto keeps this test data only while that server process runs. Stop it with Ctrl-C when finished. Binding Moto to `0.0.0.0` allows the container to reach it, so use this setup on a trusted local machine.

## Tests

The optional S3 integration test covers concurrent reuse of one local source, local LRU eviction, source replacement after purge on two instances, shared derivative invalidation, and cache pruning. Run a local S3-compatible test server such as Moto (see above), then:

```sh
PERSIMMON_TEST_S3_ENDPOINT=http://127.0.0.1:5001 cargo test --test cache_integration -- --ignored
```

The test creates its own uniquely named buckets.

Other test and evidence documents:

- [tests/LOAD.md](tests/LOAD.md): the reproducible adjacent-tile benchmark and two-instance development baselines, including native Kakadu 8.6.2 Docker runs with synthetic 8 MB and 100 MB JP2 files and local Moto S3. Production performance remains unverified.
- [tests/ACCEPTANCE.md](tests/ACCEPTANCE.md): the two-container TLS, authentication, and purge check.
- [tests/DEPLOYMENT.md](tests/DEPLOYMENT.md): the read-only smoke check and release evidence checklist for a deployed instance.
- [tests/VALIDATOR.md](tests/VALIDATOR.md): IIIF validator revisions and results.

## Kakadu SDK

The Docker build compiles `kdu_expand` and the native adapter for Linux `amd64`. Development and testing so far have used Kakadu 8.6.2. Before a release, re-run the validation in `tests/` with the exact SDK build you plan to ship. The build container requires production-licensed Kakadu libraries.
