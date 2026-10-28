# syntax=docker/dockerfile:1.7
ARG BUILDER_IMAGE=docker.io/library/rust:1.95.0-bookworm
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot
ARG PREBUILT_RUNTIME_IMAGE=docker.io/library/python:3.11-slim

FROM ${BUILDER_IMAGE} AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml build.rs ./
COPY src ./src
COPY migrations ./migrations
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked --bin agentvault \
    && install -m 0755 target/release/agentvault /agentvault \
    && install -d -m 0700 /runtime/var/lib/agentvault

# Optional host build: use e2e/reports/.runtime/vault-runtime containing the tested
# agentvault binary, agentvault.yaml and an empty data directory. Debian 13 also
# supports the GLIBC symbols required by this host's Rust toolchain.
FROM ${PREBUILT_RUNTIME_IMAGE} AS prebuilt
COPY --chown=65532:65532 agentvault /usr/local/bin/agentvault
COPY --chown=65532:65532 --chmod=0700 data/ /var/lib/agentvault/
COPY --chown=65532:65532 agentvault.yaml /etc/agentvault/agentvault.yaml
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/agentvault"]
CMD ["--help"]

FROM ${RUNTIME_IMAGE} AS runtime
COPY --from=builder /agentvault /usr/local/bin/agentvault
COPY --from=builder --chown=65532:65532 /runtime/var/lib/agentvault /var/lib/agentvault
COPY --chown=65532:65532 etc/agentvault.yaml /etc/agentvault/agentvault.yaml
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/agentvault"]
CMD ["--help"]
