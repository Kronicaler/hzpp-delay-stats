FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder 
WORKDIR /app

RUN apt-get update && apt-get install --no-install-recommends -y cmake;
ENV SQLX_OFFLINE=true

COPY --from=planner /app/recipe.json recipe.json

RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim as release

RUN apt-get update && apt-get install --no-install-recommends -y libssl3 ca-certificates;

WORKDIR /app
COPY --from=builder /app/target/release/hzpp_delays /app/hzpp_delays

ENTRYPOINT ["/app/hzpp_delays"]
