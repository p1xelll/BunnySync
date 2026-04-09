# Stage 1: Build the application using Alpine (musl) for true static linking
FROM rust:1-alpine AS builder

# Install build dependencies
RUN apk add --no-cache \
    musl-dev \
    openssl-dev \
    openssl-libs-static \
    libssh2-static \
    zlib-static \
    pkgconfig \
    cmake \
    git \
    perl \
    make \
    gcc \
    g++ \
    linux-headers

WORKDIR /app

# Copy Cargo files first for better caching
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# Set environment for static OpenSSL linking
ENV OPENSSL_STATIC=1
ENV OPENSSL_DIR=/usr
ENV PKG_CONFIG_ALLOW_STATIC=1
ENV PKG_CONFIG_ALL_STATIC=1

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
