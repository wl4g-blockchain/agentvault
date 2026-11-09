# syntax=docker/dockerfile:1.7
ARG RUST_BUILDER_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/rust:1.99.1-slim
ARG RUNTIME_IMAGE=debian:trixie-slim
FROM ${RUST_BUILDER_IMAGE} AS builder
WORKDIR /build
COPY . .
RUN --mount=type=cache,id=manual-provider-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=manual-provider-target,target=/build/target \
    CARGO_BUILD_JOBS=1 cargo build --release --locked \
      --manifest-path use-cases/marketpulse-feed-subscription/e2e/deploy/mock-mpp-tempo-service/Cargo.toml \
      --target-dir /build/target \
    && install /build/target/release/manual-payment-provider /manual-payment-provider

# Host builds can reuse the existing Cargo cache without recompiling in a container.
# Build context for this target is e2e/reports/.runtime, containing the tested binary.
FROM ${RUNTIME_IMAGE} AS runtime-base
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3t64 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system nonroot \
    && useradd --system --gid nonroot --no-create-home nonroot

FROM runtime-base AS prebuilt
COPY --chown=65532:65532 manual-payment-provider /usr/local/bin/manual-payment-provider
ENV TOKIO_WORKER_THREADS=2
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/manual-payment-provider"]

FROM runtime-base AS runtime
COPY --from=builder --chown=nonroot:nonroot /manual-payment-provider /usr/local/bin/manual-payment-provider
ENV TOKIO_WORKER_THREADS=2
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/manual-payment-provider"]
