FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

# ---------------------------------------------------------------------------
# Frontend build (Leptos / wasm32) — produces /app/frontend/dist
# ---------------------------------------------------------------------------
FROM rust:1 AS web-builder
WORKDIR /app
RUN rustup target add wasm32-unknown-unknown && \
    cargo install --locked trunk
COPY crates/pod-model crates/pod-model
COPY crates/pod-web crates/pod-web
RUN cd crates/pod-web && trunk build --release

# ---------------------------------------------------------------------------
# Server build
# ---------------------------------------------------------------------------
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies - this is the caching Docker layer!
RUN cargo chef cook --release --recipe-path recipe.json
# Build application
COPY . .
ENV SQLX_OFFLINE=true
RUN cargo build --release --bin pod-server

# ---------------------------------------------------------------------------
# Runtime image
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
WORKDIR /app
COPY --from=builder /app/target/release/pod-server /usr/local/bin
COPY --from=web-builder /app/frontend/dist /app/frontend/dist
ENTRYPOINT ["/usr/local/bin/pod-server"]
