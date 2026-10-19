# Solana Test Validator — Local Development Image
# Fork: https://github.com/wl4g-blockchain/solana (mirror of anza-xyz/agave)
# Builds from official agave release binaries.
# Download the release tarball to the build context before building:
#   curl -sSLo solana-release.tar.bz2 \
#     https://github.com/anza-xyz/agave/releases/download/v3.1.14/solana-release-x86_64-unknown-linux-gnu.tar.bz2
#
# Build:
#   docker build -t anza_solana:3.1.14 .
#   docker tag anza_solana:3.1.14 registry.cn-shenzhen.aliyuncs.com/wl4g/anza_solana:3.1.14
#   docker push registry.cn-shenzhen.aliyuncs.com/wl4g/anza_solana:3.1.14
#
# Run:
#   docker run -p 8899:8899 anza_solana:3.1.14

FROM registry.cn-shenzhen.aliyuncs.com/wl4g/rust:1.93-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    bzip2 && rm -rf /var/lib/apt/lists/*

COPY solana-release.tar.bz2 /tmp/solana.tar.bz2
RUN mkdir -p /opt/solana \
    && tar -xjf /tmp/solana.tar.bz2 -C /opt/solana \
    && rm /tmp/solana.tar.bz2

ENV PATH="/opt/solana/solana-release/bin:${PATH}"

EXPOSE 8899 8900

ENTRYPOINT ["solana-test-validator"]
CMD ["--rpc-port", "8899", "--faucet-port", "8900", "--ledger", "/tmp/solana-ledger"]
