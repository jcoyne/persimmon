# syntax=docker/dockerfile:1.7
FROM rust:1.99-bookworm AS builder
ARG TARGETARCH
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,id=persimmon-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=persimmon-cargo-target-$TARGETARCH,target=/build/target,sharing=locked \
    cargo build --locked --release \
    && cp /build/target/release/persimmon /build/persimmon

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home persimmon \
    && mkdir -p /var/cache/persimmon \
    && chown persimmon:persimmon /var/cache/persimmon
COPY --from=builder /build/persimmon /usr/local/bin/persimmon
USER persimmon
ENV PERSIMMON_LISTEN=0.0.0.0:3000 \
    PERSIMMON_LOCAL_CACHE_DIR=/var/cache/persimmon
EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD if [ -n "${PERSIMMON_HEALTH_URL:-}" ]; then \
        health_url="$PERSIMMON_HEALTH_URL"; \
      elif [ -n "${PERSIMMON_TLS_CERT:-}" ]; then \
        health_url="https://127.0.0.1:3000/healthz"; \
      else \
        health_url="http://127.0.0.1:3000/healthz"; \
      fi; \
      curl -kfsS "$health_url" >/dev/null || exit 1
ENTRYPOINT ["/usr/local/bin/persimmon"]
