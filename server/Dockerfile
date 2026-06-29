# Rust 1.88+: the crate uses edition 2024 (>=1.85) and let-chains (>=1.88).
FROM rust:1.88-slim AS build
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets
COPY migrations ./migrations

RUN cargo build --release --locked --bin dullahan

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /app/target/release/dullahan /usr/local/bin/dullahan
COPY --from=build /app/migrations /app/migrations
WORKDIR /app

EXPOSE 3001
CMD ["dullahan"]
