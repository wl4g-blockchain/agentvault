# syntax=docker/dockerfile:1.7
ARG E2E_MARKETPULSE_RUST_BUILD_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/authguard-rust-builder:1.94-bookworm
FROM ${E2E_MARKETPULSE_RUST_BUILD_IMAGE} AS builder
WORKDIR /build
COPY src/adapters/rust /build/src/adapters/rust
COPY src/adapters/proto /build/src/adapters/proto
COPY use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-rust-service /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-rust-service
WORKDIR /build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-rust-service
RUN --mount=type=cache,id=marketpulse-rust-cargo-registry,sharing=locked,target=/usr/local/cargo/registry \
    --mount=type=cache,id=marketpulse-rust-cargo-git,sharing=locked,target=/usr/local/cargo/git \
    --mount=type=cache,id=marketpulse-rust-target,sharing=locked,target=/build/use-cases/marketpulse-feed-subscription/e2e/deploy/mock-payee-marketpulse-rust-service/target \
    cargo build --release --locked \
    && install -m 0755 target/release/mock-payee-marketpulse-rust-service /mock-payee-marketpulse-rust-service

FROM ${E2E_MARKETPULSE_RUST_BUILD_IMAGE}
COPY --from=builder /mock-payee-marketpulse-rust-service /usr/local/bin/mock-payee-marketpulse-rust-service
RUN install -d -o 65532 -g 65532 -m 0700 /var/lib/marketpulse
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/mock-payee-marketpulse-rust-service"]
