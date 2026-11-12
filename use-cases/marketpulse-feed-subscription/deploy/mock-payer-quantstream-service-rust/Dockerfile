ARG E2E_QUANTSTREAM_RUST_BUILD_IMAGE
ARG E2E_QUANTSTREAM_RUST_RUNTIME_IMAGE

# Local manual acceptance can package a separately verified host binary.
# The default final stage continues to compile the complete source below.
FROM ${E2E_QUANTSTREAM_RUST_RUNTIME_IMAGE} AS runtime-base
RUN apt-get -o Acquire::Retries=3 update \
    && apt-get -o Acquire::Retries=3 install -y --no-install-recommends ca-certificates libssl3t64 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system nonroot \
    && useradd --system --gid nonroot --no-create-home nonroot

FROM runtime-base AS prebuilt
COPY --chown=65532:65532 use-cases/marketpulse-feed-subscription/reports/.runtime/quantstream-ingestor-rust /usr/local/bin/quantstream-ingestor-rust
RUN install -d -m 0700 -o nonroot -g nonroot /var/lib/quantstream
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/quantstream-ingestor-rust"]

FROM ${E2E_QUANTSTREAM_RUST_BUILD_IMAGE} AS builder
WORKDIR /build
COPY src/sdk/rust /build/src/sdk/rust
COPY src/sdk/proto /build/src/sdk/proto
COPY use-cases/marketpulse-feed-subscription/deploy/mock-payer-quantstream-service-rust/source /build/use-cases/marketpulse-feed-subscription/deploy/mock-payer-quantstream-service-rust/source
WORKDIR /build/use-cases/marketpulse-feed-subscription/deploy/mock-payer-quantstream-service-rust/source
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,id=quantstream-rust-target,sharing=locked,target=/build/target \
    CARGO_BUILD_JOBS=2 cargo build --release --locked --target-dir /build/target \
    && install -m 0755 /build/target/release/quantstream-ingestor-rust /quantstream-ingestor-rust \
    && install -d -m 0700 /runtime/var/lib/quantstream

FROM runtime-base AS runtime
COPY --from=builder --chown=nonroot:nonroot /quantstream-ingestor-rust /usr/local/bin/quantstream-ingestor-rust
COPY --from=builder --chown=nonroot:nonroot /runtime/var/lib/quantstream /var/lib/quantstream
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/quantstream-ingestor-rust"]
