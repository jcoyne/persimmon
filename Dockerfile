# syntax=docker/dockerfile:1.7
FROM --platform=linux/amd64 rust:1.98-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,id=persimmon-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=persimmon-cargo-target-amd64,target=/build/target,sharing=locked \
    cargo build --locked --release \
    && cp /build/target/release/persimmon /build/persimmon

FROM --platform=linux/amd64 debian:bookworm AS kakadu-builder
RUN apt-get update && apt-get install -y --no-install-recommends build-essential \
    && rm -rf /var/lib/apt/lists/*
# "kakadu" is the local SDK context supplied with --build-context kakadu=/path/to/sdk.
COPY --from=kakadu / /opt/kakadu/
RUN make -C /opt/kakadu/coresys/make -f Makefile-Linux-x86-64-gcc -j4 all \
    && make -C /opt/kakadu/apps/make -f Makefile-Linux-x86-64-gcc -j4 \
       CXXFLAGS=-fPIC kdu_expand kdu_render
COPY native/kakadu_adapter.cpp /build/kakadu_adapter.cpp
RUN g++ -std=c++17 -O2 -fPIC -shared -Wl,-z,defs \
       -I/opt/kakadu/coresys/common \
       -I/opt/kakadu/apps/compressed_io \
       -I/opt/kakadu/apps/support \
       /build/kakadu_adapter.cpp \
       /opt/kakadu/apps/make/kdu_region_decompressor.o \
       /opt/kakadu/apps/make/ssse3_region_decompressor.o \
       /opt/kakadu/apps/make/sse4_region_decompressor.o \
       /opt/kakadu/apps/make/avx2_region_decompressor.o \
       /opt/kakadu/apps/make/supp_local.o \
       /opt/kakadu/apps/make/jp2.o \
       /opt/kakadu/apps/make/file_io.o \
       /opt/kakadu/lib/Linux-x86-64-gcc/libkdu.a \
       -lpthread -ldl -lm -o /build/libpersimmon_kakadu.so

FROM --platform=linux/amd64 debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl libstdc++6 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home persimmon \
    && mkdir -p /var/cache/persimmon \
    && chown persimmon:persimmon /var/cache/persimmon
COPY --from=builder /build/persimmon /usr/local/bin/persimmon
# Supply this named build context from Stanford's licensed Kakadu 8.6+ SDK.
COPY --from=kakadu-builder /opt/kakadu/bin/Linux-x86-64-gcc/kdu_expand /usr/local/bin/kdu_expand
COPY --from=kakadu-builder /opt/kakadu/lib/Linux-x86-64-gcc/libkdu_v*.so /usr/local/lib/
COPY --from=kakadu-builder /build/libpersimmon_kakadu.so /usr/local/lib/
RUN chmod 755 /usr/local/bin/kdu_expand && ldconfig
USER persimmon
ENV PERSIMMON_LISTEN=0.0.0.0:3000 \
    PERSIMMON_LOCAL_CACHE_DIR=/var/cache/persimmon \
    PERSIMMON_KDU_EXPAND=/usr/local/bin/kdu_expand \
    PERSIMMON_KAKADU_NATIVE=/usr/local/lib/libpersimmon_kakadu.so
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
