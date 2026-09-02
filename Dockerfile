# syntax=docker/dockerfile:1

# ---- Build stage ----
# The code compiles on stable Rust (edition 2024, mwbot requires 1.88+).
# The official image no longer publishes nightly tags, so a pinned stable
# toolchain is used for reproducible builds.
FROM rust:1.98-slim-bookworm AS builder
WORKDIR /app

# Cache the dependency build: compile with a stub source first so the heavy
# dependency compilation is baked into this layer and reused on subsequent
# builds. This layer is only invalidated when Cargo.toml or Cargo.lock
# changes. The cargo registry is kept in a BuildKit cache mount to avoid
# re-downloading crates when the toolchain or dependencies change.
COPY Cargo.toml Cargo.lock ./
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    mkdir -p src \
    && printf 'fn main() {}\n' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src

# Build the real crate. With the dependency layer cached, only the local
# crate is recompiled here.
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --locked

# ---- Runtime stage ----
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 szbot \
    && mkdir -p /data \
    && chown szbot:szbot /data

WORKDIR /data
COPY --from=builder /app/target/release/wikipedia_sz_bot /usr/local/bin/wikipedia_sz_bot
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

USER szbot
ENV SZ_BOT_PORT=8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]