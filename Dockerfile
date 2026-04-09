# Stage 1: Install cargo-chef
FROM rust:1.85-slim-bookworm AS chef
RUN cargo install cargo-chef

# Stage 2: Prepare the recipe (dependency manifest)
FROM chef AS planner
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

# Stage 3: Build dependencies (cached layer)
FROM chef AS builder
WORKDIR /app

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    cmake \
    git \
    && rm -rf /var/lib/apt/lists/*

# Copy only the recipe - this layer is cached unless dependencies change
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Stage 4: Build the actual project
# This layer only rebuilds when source code changes
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

# Stage 5: Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    git \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -u 1000 appuser

WORKDIR /app

COPY --from=builder /app/target/release/bunnysync /usr/local/bin/bunnysync

USER appuser

EXPOSE 3000

CMD ["bunnysync"]
