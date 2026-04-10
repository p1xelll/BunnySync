# Stage 1: Build the application using Alpine (musl) for true static linking
FROM rust:1-alpine AS builder

# Install minimal build dependencies (pure Rust build)
RUN apk add --no-cache musl-dev

WORKDIR /app

# Copy Cargo files first for better caching
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# Build for the native target (musl is static by default on Alpine)
RUN cargo build --release && \
    cp /app/target/release/bunnysync /app/bunnysync

# Stage 2: Runtime stage
FROM gcr.io/distroless/static-debian12:nonroot

WORKDIR /app

COPY --from=builder /app/bunnysync /bunnysync

USER nonroot:nonroot

EXPOSE 3000

ENTRYPOINT ["/bunnysync"]
