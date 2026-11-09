# syntax=docker/dockerfile:1.7
ARG E2E_QUANTSTREAM_RUST_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/rust:1.99.1-slim
ARG E2E_QUANTSTREAM_RUST_RUNTIME_IMAGE=debian:bookworm-slim

# Local manual acceptance can package a separately verified host binary.
# The default final stage continues to compile the complete source below.
FROM ${E2E_QUANTSTREAM_RUST_RUNTIME_IMAGE} AS runtime-base
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system nonroot \
    && useradd --system --gid nonroot --no-create-home nonroot

FROM runtime-base AS prebuilt
COPY --chown=65532:65532 use-cases/marketpulse-feed-subscription/e2e/reports/.runtime/quantstream-ingestor-rust /usr/local/bin/quantstream-ingestor-rust
RUN install -d -m 0700 -o nonroot -g nonroot /var/lib/quantstream
USER nonroot:nonroot
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

FROM runtime-base AS runtime
COPY --from=builder --chown=nonroot:nonroot /quantstream-ingestor-rust /usr/local/bin/quantstream-ingestor-rust
COPY --from=builder --chown=nonroot:nonroot /runtime/var/lib/quantstream /var/lib/quantstream
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/quantstream-ingestor-rust"]
