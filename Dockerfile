# syntax=docker/dockerfile:1.7
ARG BUILDER_IMAGE=docker.io/library/rust:1.91.1-bookworm
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot

FROM ${BUILDER_IMAGE} AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY use-cases ./use-cases
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked --bin walletd \
    && install -m 0755 target/release/walletd /walletd \
    && install -d -m 0700 /runtime/var/lib/agentvault /runtime/run/agentvault

FROM ${RUNTIME_IMAGE}
COPY --from=builder /walletd /usr/local/bin/walletd
COPY --from=builder --chown=65532:65532 /runtime/var/lib/agentvault /var/lib/agentvault
COPY --from=builder --chown=65532:65532 /runtime/run/agentvault /run/agentvault
COPY --chown=65532:65532 etc/agentvault.yaml /etc/agentvault.yaml
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/walletd"]
CMD ["--help"]
