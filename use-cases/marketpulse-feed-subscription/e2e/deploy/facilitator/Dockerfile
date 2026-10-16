# x402 Facilitator — from official x402-rs/x402-rs v1.4.9
# Fork: https://github.com/wl4g-blockchain/x402-rs (mirror of x402-rs/x402-rs)
# Source: https://github.com/x402-rs/x402-rs
#
# Pre-built binary copied from host build:
#   cd deploy/docker/facilitator/x402-rs
#   cargo build --package x402-facilitator --features chain-eip155 --release
#   cp target/release/x402-facilitator deploy/docker/facilitator/
#
# Build:
#   docker build -t x402_facilitator:1.4.9 deploy/docker/facilitator/
#   docker tag x402_facilitator:1.4.9 registry.cn-shenzhen.aliyuncs.com/wl4g/x402_facilitator:1.4.9
#   docker push registry.cn-shenzhen.aliyuncs.com/wl4g/x402_facilitator:1.4.9
#
# Run:
#   docker run -p 8085:8080 -v $(pwd)/deploy/docker/facilitator/config.json:/app/config.json:ro \
#     registry.cn-shenzhen.aliyuncs.com/wl4g/x402_facilitator:1.4.9

FROM registry.cn-shenzhen.aliyuncs.com/wl4g/rust:1.93-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY x402-facilitator /usr/local/bin/x402-facilitator

EXPOSE 8080
ENV RUST_LOG=info

ENTRYPOINT ["x402-facilitator"]
CMD ["--config", "/app/config.json"]
