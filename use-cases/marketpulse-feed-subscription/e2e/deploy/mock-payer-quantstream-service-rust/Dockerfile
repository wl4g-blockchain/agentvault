# syntax=docker/dockerfile:1.7
ARG E2E_QUANTSTREAM_RUST_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/authguard-rust-builder:1.94-bookworm
ARG E2E_QUANTSTREAM_RUST_RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot
ARG E2E_QUANTSTREAM_RUST_PREBUILT_RUNTIME_IMAGE=docker.io/library/python:3.11-slim

# Local manual acceptance can package a separately verified host binary.
# The default final stage continues to compile the complete source below.
FROM ${E2E_QUANTSTREAM_RUST_PREBUILT_RUNTIME_IMAGE} AS prebuilt
COPY --chown=65532:65532 use-cases/marketpulse-feed-subscription/e2e/reports/.runtime/quantstream-ingestor-rust /usr/local/bin/quantstream-ingestor-rust
RUN install -d -m 0700 -o 65532 -g 65532 /var/lib/quantstream
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/quantstream-ingestor-rust"]

FROM ${E2E_QUANTSTREAM_RUST_BUILD_IMAGE} AS builder
WORKDIR /build
COPY src/adapters/rust /build/src/adapters/rust
COPY src/adapters/proto /build/src/adapters/proto
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-rust /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-rust
WORKDIR /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-service-rust
RUN --mount=type=cache,id=quantstream-rust-cargo-registry,sharing=locked,target=/usr/local/cargo/registry \
    --mount=type=cache,id=quantstream-rust-cargo-git,sharing=locked,target=/usr/local/cargo/git \
    --mount=type=cache,id=quantstream-rust-target,sharing=locked,target=/build/target \
    CARGO_BUILD_JOBS=2 cargo build --release --locked --target-dir /build/target \
    && install -m 0755 /build/target/release/quantstream-ingestor-rust /quantstream-ingestor-rust \
    && install -d -m 0700 /runtime/var/lib/quantstream

FROM ${E2E_QUANTSTREAM_RUST_RUNTIME_IMAGE}
COPY --from=builder /quantstream-ingestor-rust /usr/local/bin/quantstream-ingestor-rust
COPY --from=builder --chown=65532:65532 /runtime/var/lib/quantstream /var/lib/quantstream
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/quantstream-ingestor-rust"]
