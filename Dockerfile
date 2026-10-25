# syntax=docker/dockerfile:1

# Builder and runtime share a Debian release so the binary links against the
# glibc it was built with. The Rust version tracks the toolchain in
# `nix develop`.
ARG RUST_VERSION=1.99
ARG DEBIAN_RELEASE=trixie

# ==============================================================================
# Build
# ==============================================================================

# cargo-chef splits the build in two: `cook` compiles only the dependencies
# from a recipe of the manifests, so that layer stays cached until
# Cargo.toml or Cargo.lock change, and a source edit rebuilds only our crates.
FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS chef
RUN cargo install cargo-chef --version 0.1.78 --locked
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
# The query macros read the committed .sqlx/ cache, so the build needs no
# database. migrate!() embeds crates/pod-server/migrations into the binary.
ENV SQLX_OFFLINE=true
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --locked --bin pod-server --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked --bin pod-server

# ==============================================================================
# Runtime
# ==============================================================================

FROM debian:${DEBIAN_RELEASE}-slim AS runtime
# reqwest verifies TLS against the system trust store, which feed and
# directory fetches need. The package is left unpinned so a rebuild picks up
# trust store updates; Debian drops superseded versions from its mirrors.
# hadolint ignore=DL3008
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin pod
COPY --from=builder /app/target/release/pod-server /usr/local/bin/pod-server
USER pod
# Configuration comes from POD_* variables. A pod.toml mounted here (or
# named by POD_CONFIG) is read as well.
WORKDIR /app
EXPOSE 3000
ENTRYPOINT ["/usr/local/bin/pod-server"]
