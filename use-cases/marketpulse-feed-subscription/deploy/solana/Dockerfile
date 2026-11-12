# Build from a clean official Agave checkout, including its .git directory.
# Image arguments and the output tag are supplied by util/images.py.
# From the E2E root:
#   E2E_IMAGE_NETWORK=NON_GFW python util/images.py -- docker buildx build \
#     --platform linux/arm64 --load -f deploy/solana/Dockerfile /path/to/agave
# Upgrades require matching AGAVE_VERSION and AGAVE_REVISION build arguments.

ARG RUST_VERSION=1.99.1
ARG RUST_BUILDER_IMAGE
ARG RUNTIME_IMAGE
ARG AGAVE_VERSION=3.1.14
ARG AGAVE_REVISION=3134055b562e95902233be308453fffa1c4a8902

FROM ${RUST_BUILDER_IMAGE} AS builder
ARG TARGETARCH
RUN sed -i 's|http://deb.debian.org|https://deb.debian.org|g' /etc/apt/sources.list.d/debian.sources \
    && apt-get -o Acquire::Retries=3 update \
    && apt-get -o Acquire::Retries=3 install -y --no-install-recommends \
      build-essential clang libclang-dev lld cmake pkg-config git \
      libssl-dev libudev-dev zlib1g-dev protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*
ARG RUST_VERSION
ENV RUSTUP_TOOLCHAIN=${RUST_VERSION} CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/build/target \
    CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_LTO=false \
    CARGO_HTTP_MULTIPLEXING=false CARGO_HTTP_TIMEOUT=600 \
    CARGO_NET_GIT_FETCH_WITH_CLI=false RUSTFLAGS="-C link-arg=-fuse-ld=lld"
COPY . /build/agave
WORKDIR /build/agave
ARG AGAVE_VERSION
ARG AGAVE_REVISION
ARG SPL_TOKEN_CLI_VERSION=5.5.0
# Keep compiler-version warnings visible when using a newer Rust toolchain.
RUN test "$(git rev-parse HEAD)" = "${AGAVE_REVISION}" \
    && test "$(git rev-parse "v${AGAVE_VERSION}^{commit}")" = "${AGAVE_REVISION}" \
    && git diff --quiet && git diff --cached --quiet \
    && sed -i 's/^warnings = "deny"/warnings = "warn"/' Cargo.toml \
    && mkdir -p /runtime/bin
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,id=solana-source-${TARGETARCH}-target,sharing=locked,target=/build/target \
    check_target() { if [ "$(du -sk /build/target | cut -f1)" -gt 20971520 ]; then cargo clean; fi; }; \
    check_target \
    && cargo build --release --locked --bin solana-test-validator --bin solana --bin solana-keygen \
    && install -m 0755 /build/target/release/solana-test-validator /build/target/release/solana /build/target/release/solana-keygen /runtime/bin/ \
    && check_target \
    && cargo install --locked --root /runtime --version "${SPL_TOKEN_CLI_VERSION}" spl-token-cli \
    && find /usr/local/cargo/registry/src -path "*/spl-token-cli-${SPL_TOKEN_CLI_VERSION}/Cargo.lock" \
      -exec cp {} /runtime/spl-token-cli-Cargo.lock \; \
    && test -s /runtime/spl-token-cli-Cargo.lock \
    && strip /runtime/bin/*

FROM ${RUNTIME_IMAGE} AS runtime
ARG AGAVE_VERSION
ARG AGAVE_REVISION
LABEL org.opencontainers.image.source="https://github.com/anza-xyz/agave" \
      org.opencontainers.image.revision="${AGAVE_REVISION}" \
      org.opencontainers.image.version="${AGAVE_VERSION}"
RUN apt-get -o Acquire::Retries=3 update \
    && apt-get -o Acquire::Retries=3 install -y --no-install-recommends ca-certificates libssl3t64 libudev1 libstdc++6 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /runtime/bin /opt/solana/solana-release/bin
COPY --from=builder /build/agave/LICENSE /usr/share/doc/solana/LICENSE
COPY --from=builder /build/agave/Cargo.lock /usr/share/doc/solana/Cargo.lock
COPY --from=builder /runtime/spl-token-cli-Cargo.lock /usr/share/doc/solana/spl-token-cli/Cargo.lock
ENV PATH="/opt/solana/solana-release/bin:${PATH}"
RUN solana-test-validator --version && solana --version && solana-keygen --version && spl-token --version
EXPOSE 8899 8900
ENTRYPOINT ["solana-test-validator"]
CMD ["--rpc-port", "8899", "--faucet-port", "8900", "--ledger", "/tmp/solana-ledger"]
