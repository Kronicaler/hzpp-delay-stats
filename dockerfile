FROM rust:slim-bookworm AS builder
WORKDIR /app

RUN apt-get update && apt-get install --no-install-recommends -y cmake pkg-config openssl libssl-dev build-essential wget && rm -rf /var/lib/apt/lists/*;

ENV SCCACHE_VERSION=0.5.0

RUN ARCH= && alpineArch="$(dpkg --print-architecture)" \
      && case "${alpineArch##*-}" in \
        amd64) \
          ARCH='x86_64' \
          ;; \
        arm64) \
          ARCH='aarch64' \
          ;; \
        *) ;; \
      esac \
    && wget -O sccache.tar.gz https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-${ARCH}-unknown-linux-musl.tar.gz \
    && tar xzf sccache.tar.gz \
    && mv sccache-v*/sccache /usr/local/bin/sccache \
    && chmod +x /usr/local/bin/sccache

ENV RUSTC_WRAPPER=/usr/local/bin/sccache

# Pre-compile dependencies
WORKDIR /build

RUN cargo init --name rust-docker

COPY Cargo.toml Cargo.lock ./
COPY rust-toolchain.toml ./

RUN --mount=type=cache,target=/root/.cache cargo fetch && \
    cargo build && \
    cargo build --release && \
    rm src/*.rs

# Build the project
COPY src src
COPY migrations migrations 
COPY .sqlx .sqlx

ENV SQLX_OFFLINE=true
RUN --mount=type=cache,target=/root/.cache touch src/main.rs && \
    cargo build --release

FROM debian:bookworm-slim as release

RUN apt-get update && apt-get install --no-install-recommends -y libssl3 ca-certificates && rm -rf /var/lib/apt/lists/*;

WORKDIR /app
COPY --link --from=builder /build/target/release/hzpp_delay_stats /app/hzpp_delay_stats

ENTRYPOINT ["/app/hzpp_delay_stats"]
