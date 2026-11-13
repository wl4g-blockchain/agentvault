# syntax=docker/dockerfile:1.7

ARG RUST_BUILDER_IMAGE=registry.cn-shenzhen.aliyuncs.com/wl4g/rust:1.99.1-slim
ARG RUNTIME_IMAGE=debian:trixie-slim

FROM ${RUST_BUILDER_IMAGE} AS builder
WORKDIR /workspace
COPY Cargo.toml Cargo.lock rust-toolchain.toml build.rs ./
RUN cargo --version
COPY src ./src
COPY migrations ./migrations
RUN --mount=type=cache,sharing=locked,target=/usr/local/cargo/registry \
    --mount=type=cache,sharing=locked,target=/usr/local/cargo/git \
    --mount=type=cache,sharing=locked,target=/workspace/target \
    if [ "$(du -sk /workspace/target | cut -f1)" -gt 20971520 ]; then cargo clean --target-dir /workspace/target; fi \
    && CARGO_BUILD_JOBS=2 cargo build --release --locked --bin agentvault \
    && install -m 0755 target/release/agentvault /agentvault

FROM ${RUNTIME_IMAGE} AS runtime-base
RUN apt-get -o Acquire::Retries=3 update \
    && apt-get -o Acquire::Retries=3 install -y --no-install-recommends ca-certificates libssl3t64 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system nonroot \
    && useradd --system --gid nonroot --no-create-home nonroot

FROM runtime-base AS runtime
COPY --from=builder --chown=nonroot:nonroot /agentvault /usr/local/bin/agentvault
RUN ["/usr/local/bin/agentvault", "--version"]
RUN install -d -o nonroot -g nonroot -m 0700 /var/lib/agentvault
COPY --chown=nonroot:nonroot etc/agentvault.yaml /etc/agentvault/agentvault.yaml
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/agentvault"]
CMD ["--help"]
