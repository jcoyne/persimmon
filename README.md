# Persimmon

Persimmon is an image server that implements the [IIIF Image API 3.0](https://iiif.io/api/image/3.0/). It serves private JPEG 2000 (JP2) images stored in S3, using Kakadu to decode them. It can also serve JPEG XL source images.

## Features

- IIIF Image API 3.0 regions, sizes, rotations, and qualities
- Output as JPEG, PNG, AVIF, lossless WebP, or lossless JPEG XL
- Reads JP2 or JPEG XL source images from any S3-compatible store
- Caches source files on local disk and rendered images in S3, shared across instances
- Authenticated purge endpoint to clear the caches for an image
- Native TLS, a health check, and Prometheus metrics

## Endpoints

| Route | Description |
| --- | --- |
| `GET /v3/{identifier}/info.json` | Image information |
| `GET /v3/{identifier}/{region}/{size}/{rotation}/{quality}.{format}` | An image (`jpg`, `png`, `webp`, `avif`, or `jxl`) |
| `GET /v3/{identifier}` | Redirects to `info.json` |
| `POST /admin/purge` | Clears cached images for `{"identifier":"a/b"}` (Basic auth) |
| `GET /healthz` | Health check |
| `GET /metrics` | Prometheus metrics |

The identifier is the S3 key of the source image. Encode any slash in it as `%2F`, so key `a/b.jp2` is requested as `/v3/a%2Fb.jp2/info.json`.

## Building

Run the tests and build the binary:

```sh
cargo test
cargo build --release
```

The Docker image needs a licensed Kakadu 8.6 or newer SDK. Point the build at your SDK source tree:

```sh
docker buildx build --platform linux/amd64 \
  --build-context kakadu=/path/to/licensed-kakadu-8.6-or-newer-sdk \
  --load -t persimmon:dev .
```

Don't commit Kakadu SDK binaries, headers, or credentials to this repository.

## Configuring

Persimmon is configured with environment variables. See [Configuring.md](Configuring.md).

## More information

- [Developers.md](Developers.md): project status, local testing, and internals
- [PLAN.md](PLAN.md): design goals and acceptance criteria
- [JPEGXL_NOTES.md](JPEGXL_NOTES.md): how JPEG XL sources are handled and their performance trade-offs
