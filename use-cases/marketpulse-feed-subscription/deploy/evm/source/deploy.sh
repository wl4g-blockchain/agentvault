#!/usr/bin/env bash
set -euo pipefail

RPC_URL=${E2E_EVM_RPC_URL:-http://anvil:8545}
DEPLOYER_PRIVATE_KEY=${E2E_EVM_DEPLOYER_PRIVATE_KEY:-0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80}
TOKEN_ADDRESS=${E2E_EVM_USDC_ADDRESS:-0x5FbDB2315678afecb367f032d93F642f64180aa3}
INITIAL_SUPPLY=${E2E_EVM_PAYER_INITIAL_USDC:-1000000000000}
BYTECODE_FILE=${E2E_EVM_USDC_BYTECODE:-/fixture/artifacts/E2EUSDC.bin}
ARTIFACT_MANIFEST=${E2E_EVM_ARTIFACT_MANIFEST:-/fixture/artifacts/contracts.sha256}

die() {
  printf 'EVM fixture error: %s\n' "$*" >&2
  exit 1
}

test -r "$BYTECODE_FILE" || die "missing compiled E2EUSDC bytecode: $BYTECODE_FILE"
test -r "$ARTIFACT_MANIFEST" || die "missing EVM artifact checksum manifest"
(cd "$(dirname "$ARTIFACT_MANIFEST")" && sha256sum --check "$(basename "$ARTIFACT_MANIFEST")") \
  >/dev/null || die "EVM fixture artifact checksum verification failed"
test "$(cast chain-id --rpc-url "$RPC_URL")" = "31337" || die "unexpected EVM chain id"

PAYER_PRIVATE_KEYS=(
  "${E2E_EVM_RUST_PRIMARY_PRIVATE_KEY:-59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d}"
  "${E2E_EVM_RUST_SECONDARY_PRIVATE_KEY:-0000000000000000000000000000000000000000000000000000000000000002}"
  "${E2E_EVM_GO_PRIMARY_PRIVATE_KEY:-0000000000000000000000000000000000000000000000000000000000000003}"
  "${E2E_EVM_GO_SECONDARY_PRIVATE_KEY:-0000000000000000000000000000000000000000000000000000000000000004}"
  "${E2E_EVM_PYTHON_PRIMARY_PRIVATE_KEY:-0000000000000000000000000000000000000000000000000000000000000005}"
  "${E2E_EVM_PYTHON_SECONDARY_PRIVATE_KEY:-0000000000000000000000000000000000000000000000000000000000000006}"
  "${E2E_EVM_TYPESCRIPT_PRIMARY_PRIVATE_KEY:-0000000000000000000000000000000000000000000000000000000000000007}"
  "${E2E_EVM_TYPESCRIPT_SECONDARY_PRIVATE_KEY:-0000000000000000000000000000000000000000000000000000000000000008}"
  "${E2E_EVM_JAVA_PRIMARY_PRIVATE_KEY:-0000000000000000000000000000000000000000000000000000000000000009}"
  "${E2E_EVM_JAVA_SECONDARY_PRIVATE_KEY:-000000000000000000000000000000000000000000000000000000000000000a}"
)
declare -A SEEN_PAYER_ADDRESSES=()
PAYER_ADDRESSES=()
for private_key in "${PAYER_PRIVATE_KEYS[@]}"; do
  private_key=${private_key#0x}
  [[ "$private_key" =~ ^[0-9a-fA-F]{64}$ ]] || die "payer private key must be 32-byte hex"
  address=$(cast wallet address --private-key "0x$private_key")
  test -n "$address" || die "payer private key has no address"
  normalized=${address,,}
  test -z "${SEEN_PAYER_ADDRESSES[$normalized]:-}" || die "payer wallet addresses must be distinct"
  SEEN_PAYER_ADDRESSES[$normalized]=1
  PAYER_ADDRESSES+=("$address")
done
test "${#PAYER_ADDRESSES[@]}" = "10" || die "exactly ten payer wallets are required"

# x402-rs 1.4.9 eagerly verifies these three canonical deployments when its
# EVM provider starts, even for an EIP-3009-only payment. Reproduce the
# canonical Base runtime bytecode at the same addresses on the ephemeral Anvil
# chain. The payment below never grants or uses Permit2 allowances.
while IFS='|' read -r address artifact; do
  runtime=$(tr -d '\r\n' <"/fixture/artifacts/$artifact")
  test -n "$runtime" || die "empty canonical x402 dependency: $artifact"
  cast rpc --rpc-url "$RPC_URL" anvil_setCode "$address" "0x${runtime#0x}" >/dev/null
  test "$(cast code --rpc-url "$RPC_URL" "$address")" != "0x" || \
    die "canonical x402 dependency was not installed at $address"
done <<'CONTRACTS'
0xdAcD51A54883eb67D95FAEb2BBfdC4a9a6BD2a3B|EIP6492Validator.runtime.bin
0x000000000022D473030F116dDEE9F6B43aC78BA3|Permit2.runtime.bin
0x402085c248EeA27D92E8b30b2C58ed07f9E20001|ExactPermit2Proxy.runtime.bin
CONTRACTS

code=$(cast code --rpc-url "$RPC_URL" "$TOKEN_ADDRESS")
if test "$code" = "0x"; then
  bytecode=$(tr -d '\r\n' <"$BYTECODE_FILE")
  test -n "$bytecode" || die "compiled E2EUSDC bytecode is empty"
  cast send --rpc-url "$RPC_URL" --private-key "$DEPLOYER_PRIVATE_KEY" \
    --create "0x${bytecode#0x}" 'constructor(address,uint256)' "${PAYER_ADDRESSES[0]}" "$INITIAL_SUPPLY" \
    >/tmp/agentvault-e2e-usdc-deploy.log
fi

code=$(cast code --rpc-url "$RPC_URL" "$TOKEN_ADDRESS")
test "$code" != "0x" || die "E2EUSDC deployment did not create code"
# Every language owns two EOAs. Give all ten native gas headroom and the same
# E2EUSDC balance so no payer can borrow another SDK's wallet pool.
for address in "${PAYER_ADDRESSES[@]}"; do
  cast rpc --rpc-url "$RPC_URL" anvil_setBalance "$address" 0x3635C9ADC5DEA00000 >/dev/null
  balance=$(cast call --rpc-url "$RPC_URL" "$TOKEN_ADDRESS" \
    'balanceOf(address)(uint256)' "$address" | awk '{print $1}')
  if test "$balance" = 0; then
    cast send --rpc-url "$RPC_URL" --private-key "$DEPLOYER_PRIVATE_KEY" \
      "$TOKEN_ADDRESS" 'mint(address,uint256)' "$address" "$INITIAL_SUPPLY" >/dev/null
  fi
done

name=$(cast call --rpc-url "$RPC_URL" "$TOKEN_ADDRESS" 'name()(string)')
version=$(cast call --rpc-url "$RPC_URL" "$TOKEN_ADDRESS" 'version()(string)')
decimals=$(cast call --rpc-url "$RPC_URL" "$TOKEN_ADDRESS" 'decimals()(uint8)' | awk '{print $1}')
test "$name" = '"USDC"' || die "unexpected token name: $name"
test "$version" = '"2"' || die "unexpected EIP-712 version: $version"
test "$decimals" = "6" || die "unexpected token decimals: $decimals"
printf '{"network":"eip155:31337","token":"%s","payers":[' "$TOKEN_ADDRESS"
separator=""
for address in "${PAYER_ADDRESSES[@]}"; do
  balance=$(cast call --rpc-url "$RPC_URL" "$TOKEN_ADDRESS" \
    'balanceOf(address)(uint256)' "$address" | awk '{print $1}')
  test "$balance" = "$INITIAL_SUPPLY" || die "unexpected payer balance for $address: $balance"
  printf '%s{"address":"%s","balance":"%s"}' "$separator" "$address" "$balance"
  separator=,
done
printf '],"decimals":6}\n'
