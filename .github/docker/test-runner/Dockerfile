FROM rust:1.82-bookworm

# Install system deps
RUN apt-get update && apt-get install -y \
    pkg-config libssl-dev cmake curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace/peko-runtime

# Pre-build dependencies for caching
COPY peko-runtime/Cargo.toml peko-runtime/Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo fetch

# Copy full source
COPY peko-runtime/ .

# Build all integration tests (but don't run yet)
RUN cargo test --no-run --test pekohub_integration
RUN cargo test --no-run --test tunnel_integration
RUN cargo test --no-run --test tunnel_e2e
RUN cargo test --no-run --test packaging_integration
RUN cargo test --no-run --test registry_integration
