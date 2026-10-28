# syntax=docker/dockerfile:1.7
ARG BUILDER_IMAGE=docker.io/library/rust:1.95.0-bookworm
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot
FROM ${BUILDER_IMAGE} AS builder
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
FROM ${RUNTIME_IMAGE} AS prebuilt
COPY --chown=65532:65532 manual-payment-provider /usr/local/bin/manual-payment-provider
ENV TOKIO_WORKER_THREADS=2
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/manual-payment-provider"]

FROM ${RUNTIME_IMAGE} AS runtime
COPY --from=builder --chown=65532:65532 /manual-payment-provider /usr/local/bin/manual-payment-provider
ENV TOKIO_WORKER_THREADS=2
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/manual-payment-provider"]
