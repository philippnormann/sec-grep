# syntax=docker/dockerfile:1
# Stage 1: Build the Rust CLI / TUI binary
FROM docker.io/library/rust:1-slim-bookworm AS rust-builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev ca-certificates sqlite3 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock rustfmt.toml ./
COPY crates ./crates
RUN cargo build --release -p sec-grep

# Stage 2: Build the React web UI
FROM docker.io/library/node:22-bookworm AS web-builder
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

# Stage 3: Runtime image with Node + sec-grep binary
FROM docker.io/library/node:22-bookworm AS runner
RUN apt-get update && apt-get install -y --no-install-recommends \
    sqlite3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=rust-builder /src/target/release/sec-grep /usr/local/bin/sec-grep
COPY --from=web-builder /web/dist ./dist
COPY --from=web-builder /web/node_modules ./node_modules
COPY --from=web-builder /web/package.json ./
COPY web/server.js ./
ENV SEC_GREP_DB=/data/papers.db
EXPOSE 5002
# Default: start the web UI.
# To run the TUI instead, override the command, e.g.:
#   podman run --rm -it localhost/sec-grep:latest /usr/local/bin/sec-grep --tui
CMD ["node", "server.js"]
