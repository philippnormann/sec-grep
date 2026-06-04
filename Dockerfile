# syntax=docker/dockerfile:1
# Stage 1: Build all Rust binaries
FROM docker.io/library/rust:1-slim-bookworm AS rust-builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev ca-certificates sqlite3 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock rustfmt.toml ./
COPY crates ./crates
RUN cargo build --release -p sec-grep -p sec-grep-web

# Stage 2: Runtime image with Rust binaries only
FROM docker.io/library/debian:bookworm-slim AS runner
RUN apt-get update && apt-get install -y --no-install-recommends \
    sqlite3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=rust-builder /src/target/release/sec-grep /usr/local/bin/sec-grep
COPY --from=rust-builder /src/target/release/sec-grep-web /usr/local/bin/sec-grep-web
VOLUME ["/data"]
EXPOSE 5002
CMD ["sec-grep-web", "--port", "5002", "--db", "/data/papers.db"]
