# Build stage
FROM rust:1.75-slim AS builder

WORKDIR /app

# Install dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Copy source code
COPY src ./src

# Build release binary
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

WORKDIR /app

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN groupadd -r pekobot && useradd -r -g pekobot pekobot

# Create data directory
RUN mkdir -p /data/pekobot && chown pekobot:pekobot /data/pekobot

# Copy binary from builder
COPY --from=builder /app/target/release/pekobot /usr/local/bin/pekobot

# Switch to non-root user
USER pekobot

# Set environment variables
ENV PEKOBOT_DATA_DIR=/data/pekobot

# Expose default HTTP port (if using HTTP channel)
EXPOSE 3000

# Default command
ENTRYPOINT ["pekobot"]
CMD ["--help"]
