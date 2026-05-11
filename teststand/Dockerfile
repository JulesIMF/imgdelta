# syntax=docker/dockerfile:1
# Build the teststand binary inside the full workspace.

FROM rust:1.77-slim-bookworm AS builder

WORKDIR /workspace

# Cache dependency downloads before copying source
COPY Cargo.toml Cargo.lock ./
COPY core/Cargo.toml     core/
COPY cli/Cargo.toml      cli/
COPY synthetic-fs/Cargo.toml synthetic-fs/
COPY teststand/Cargo.toml teststand/

# Stub source files so cargo fetch can resolve all deps
RUN mkdir -p core/src cli/src synthetic-fs/src teststand/src \
    && echo 'fn main(){}' > cli/src/main.rs \
    && echo 'fn main(){}' > teststand/src/main.rs \
    && echo '' > core/src/lib.rs \
    && echo '' > synthetic-fs/src/lib.rs
RUN cargo fetch

# Copy real source
COPY . .
RUN cargo build --release -p teststand

# ── Runtime image ─────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /workspace/target/release/teststand /usr/local/bin/teststand

# Default data dir; mount a volume here in production
RUN mkdir -p /data /config
WORKDIR /data

EXPOSE 8080

ENTRYPOINT ["teststand", "--config", "/config/teststand.toml", "--families", "/config/families.toml"]
