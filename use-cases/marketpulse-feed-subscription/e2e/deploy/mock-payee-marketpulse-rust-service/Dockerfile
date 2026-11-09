# syntax=docker/dockerfile:1.7
ARG E2E_MARKETPULSE_RUST_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/rust:1.99.1-slim
ARG E2E_MARKETPULSE_RUST_RUNTIME_IMAGE=debian:bookworm-slim

# Local manual acceptance can package a separately verified host binary.
# The default final stage continues to compile the complete source below.
FROM ${E2E_MARKETPULSE_RUST_RUNTIME_IMAGE} AS runtime-base
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system nonroot \
    && useradd --system --gid nonroot --no-create-home nonroot

FROM runtime-base AS prebuilt
COPY --chown=65532:65532 use-cases/marketpulse-feed-subscription/e2e/reports/.runtime/mock-payee-marketpulse-rust-service /usr/local/bin/mock-payee-marketpulse-rust-service
RUN install -d -m 0700 -o nonroot -g nonroot /var/lib/marketpulse
USER nonroot:nonroot
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

FROM runtime-base AS runtime
COPY --from=builder --chown=nonroot:nonroot /mock-payee-marketpulse-rust-service /usr/local/bin/mock-payee-marketpulse-rust-service
COPY --from=builder --chown=nonroot:nonroot /runtime/var/lib/marketpulse /var/lib/marketpulse
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/mock-payee-marketpulse-rust-service"]
