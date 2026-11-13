ARG E2E_MARKETPULSE_RUST_BUILD_IMAGE
ARG E2E_MARKETPULSE_RUST_RUNTIME_IMAGE

# Local manual acceptance can package a separately verified host binary.
# The default final stage continues to compile the complete source below.
FROM ${E2E_MARKETPULSE_RUST_RUNTIME_IMAGE} AS runtime-base
RUN apt-get -o Acquire::Retries=3 update \
    && apt-get -o Acquire::Retries=3 install -y --no-install-recommends ca-certificates libssl3t64 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system nonroot \
    && useradd --system --gid nonroot --no-create-home nonroot

FROM runtime-base AS prebuilt
COPY --chown=65532:65532 use-cases/marketpulse-feed-subscription/reports/.runtime/mock-payee-marketpulse-rust-service /usr/local/bin/mock-payee-marketpulse-rust-service
RUN install -d -m 0700 -o nonroot -g nonroot /var/lib/marketpulse
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/mock-payee-marketpulse-rust-service"]

FROM ${E2E_MARKETPULSE_RUST_BUILD_IMAGE} AS builder
WORKDIR /build
COPY src/sdk/rust /build/src/sdk/rust
COPY src/sdk/proto /build/src/sdk/proto
COPY use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-rust-service/source /build/use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-rust-service/source
WORKDIR /build/use-cases/marketpulse-feed-subscription/deploy/mock-payee-marketpulse-rust-service/source
RUN --mount=type=cache,sharing=locked,target=/usr/local/cargo/registry \
    --mount=type=cache,sharing=locked,target=/usr/local/cargo/git \
    --mount=type=cache,id=marketpulse-rust-target,sharing=locked,target=/build/target \
    if [ "$(du -sk /build/target | cut -f1)" -gt 20971520 ]; then cargo clean --target-dir /build/target; fi \
    && CARGO_BUILD_JOBS=1 cargo build --release --locked --target-dir /build/target \
    && install -m 0755 /build/target/release/mock-payee-marketpulse-rust-service /mock-payee-marketpulse-rust-service \
    && install -d -m 0700 /runtime/var/lib/marketpulse

FROM runtime-base AS runtime
COPY --from=builder --chown=nonroot:nonroot /mock-payee-marketpulse-rust-service /usr/local/bin/mock-payee-marketpulse-rust-service
COPY --from=builder --chown=nonroot:nonroot /runtime/var/lib/marketpulse /var/lib/marketpulse
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/mock-payee-marketpulse-rust-service"]
