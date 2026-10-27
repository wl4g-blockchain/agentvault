# syntax=docker/dockerfile:1.7
ARG E2E_MARKETPULSE_RUST_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/authguard-rust-builder:1.94-bookworm
ARG E2E_MARKETPULSE_RUST_RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot
ARG E2E_MARKETPULSE_RUST_PREBUILT_RUNTIME_IMAGE=docker.io/library/python:3.11-slim

# Local manual acceptance can package a separately verified host binary.
# The default final stage continues to compile the complete source below.
FROM ${E2E_MARKETPULSE_RUST_PREBUILT_RUNTIME_IMAGE} AS prebuilt
COPY --chown=65532:65532 use-cases/marketpulse-feed-subscription/e2e/reports/.runtime/mock-payee-marketpulse-rust-service /usr/local/bin/mock-payee-marketpulse-rust-service
RUN install -d -m 0700 -o 65532 -g 65532 /var/lib/marketpulse
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/mock-payee-marketpulse-rust-service"]

FROM ${E2E_MARKETPULSE_RUST_BUILD_IMAGE} AS builder
WORKDIR /build
COPY src/adapters/rust /build/src/adapters/rust
COPY src/adapters/proto /build/src/adapters/proto
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-rust-service /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-rust-service
WORKDIR /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-rust-service
RUN --mount=type=cache,id=marketpulse-rust-cargo-registry,sharing=locked,target=/usr/local/cargo/registry \
    --mount=type=cache,id=marketpulse-rust-cargo-git,sharing=locked,target=/usr/local/cargo/git \
    --mount=type=cache,id=marketpulse-rust-target,sharing=locked,target=/build/target \
    CARGO_BUILD_JOBS=1 cargo build --release --locked --target-dir /build/target \
    && install -m 0755 /build/target/release/mock-payee-marketpulse-rust-service /mock-payee-marketpulse-rust-service \
    && install -d -m 0700 /runtime/var/lib/marketpulse

FROM ${E2E_MARKETPULSE_RUST_RUNTIME_IMAGE}
COPY --from=builder /mock-payee-marketpulse-rust-service /usr/local/bin/mock-payee-marketpulse-rust-service
COPY --from=builder --chown=65532:65532 /runtime/var/lib/marketpulse /var/lib/marketpulse
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/mock-payee-marketpulse-rust-service"]
