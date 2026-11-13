ARG RUST_BUILDER_IMAGE
ARG RUNTIME_IMAGE
FROM ${RUST_BUILDER_IMAGE} AS builder
WORKDIR /build
COPY . .
RUN --mount=type=cache,sharing=locked,target=/usr/local/cargo/registry \
    --mount=type=cache,id=manual-provider-target,sharing=locked,target=/build/target \
    if [ "$(du -sk /build/target | cut -f1)" -gt 20971520 ]; then cargo clean --target-dir /build/target; fi \
    && CARGO_BUILD_JOBS=1 cargo build --release --locked \
      --manifest-path use-cases/marketpulse-feed-subscription/deploy/mock-mpp-tempo-service/source/Cargo.toml \
      --target-dir /build/target \
    && install /build/target/release/manual-payment-provider /manual-payment-provider

# Host builds can reuse the existing Cargo cache without recompiling in a container.
# Build context for this target is reports/.runtime, containing the tested binary.
FROM ${RUNTIME_IMAGE} AS runtime-base
RUN apt-get -o Acquire::Retries=3 update \
    && apt-get -o Acquire::Retries=3 install -y --no-install-recommends ca-certificates libssl3t64 \
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
