# syntax=docker/dockerfile:1
ARG BUILDER_IMAGE
ARG RUNTIME_IMAGE
FROM ${BUILDER_IMAGE} AS builder

RUN apk add --no-cache build-base cargo cmake perl rust
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked && cp target/release/walletd /out-walletd

FROM ${RUNTIME_IMAGE}

RUN apk add --no-cache ca-certificates libgcc tzdata \
    && addgroup -S wallet \
    && adduser -S -D -H -G wallet wallet \
    && install -d -m 0700 /var/lib/wallet /run/wallet \
    && chown -R wallet:wallet /var/lib/wallet /run/wallet
COPY --from=builder /out-walletd /usr/local/bin/walletd
USER wallet:wallet
ENTRYPOINT ["/usr/local/bin/walletd"]
CMD ["--help"]
