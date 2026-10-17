# syntax=docker/dockerfile:1.7
ARG E2E_QUANTSTREAM_RUST_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/authguard-rust-builder:1.94-bookworm
FROM ${E2E_QUANTSTREAM_RUST_BUILD_IMAGE} AS builder
WORKDIR /build
COPY src/adapters/rust /build/src/adapters/rust
COPY src/adapters/proto /build/src/adapters/proto
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-rust /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-rust
WORKDIR /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-rust
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payer-quantstream-ingest-service-rust/target \
    cargo build --release --locked \
    && install -m 0755 target/release/quantstream-ingestor-rust /quantstream-ingestor-rust

FROM ${E2E_QUANTSTREAM_RUST_BUILD_IMAGE}
COPY --from=builder /quantstream-ingestor-rust /usr/local/bin/quantstream-ingestor-rust
RUN install -d -o 65532 -g 65532 -m 0700 /var/lib/quantstream
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/quantstream-ingestor-rust"]
