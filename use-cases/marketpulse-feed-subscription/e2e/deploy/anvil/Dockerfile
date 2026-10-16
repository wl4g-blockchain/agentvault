# Anvil (Foundry) — Local EVM Node for Development
# Builds from official Foundry release binaries.
#
# Anvil provides a local Ethereum JSON-RPC endpoint with full eth_* API.
# Much lighter than geth+lighthouse: single binary, instant startup, ~50MB.
#
# Build:
#   curl -sSLo foundry.tar.gz \
#     https://github.com/foundry-rs/foundry/releases/download/v1.7.1/foundry_v1.7.1_alpine_amd64.tar.gz
#   docker build -t foundry_anvil:1.7.1 .
#   docker tag foundry_anvil:1.7.1 registry.cn-shenzhen.aliyuncs.com/wl4g/foundry_anvil:1.7.1
#   docker push registry.cn-shenzhen.aliyuncs.com/wl4g/foundry_anvil:1.7.1
#
# Run:
#   docker run -p 8545:8545 foundry_anvil:1.7.1

FROM registry.cn-shenzhen.aliyuncs.com/wl4g/rust:1.93-slim

COPY foundry.tar.gz /tmp/foundry.tar.gz
RUN mkdir -p /opt/foundry \
    && tar -xzf /tmp/foundry.tar.gz -C /opt/foundry \
    && chmod +x /opt/foundry/* \
    && rm /tmp/foundry.tar.gz

ENV PATH="/opt/foundry:${PATH}"

EXPOSE 8545

# Default: spin up anvil with 10 test accounts, 10000 ETH each
ENTRYPOINT ["anvil"]
CMD ["--host", "0.0.0.0", "--port", "8545", "--accounts", "10", "--balance", "10000"]
