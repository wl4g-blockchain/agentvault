#!/usr/bin/env bash
set -euo pipefail

RPC_URL=${E2E_EVM_RPC_URL:-http://anvil:8545}
DEPLOYER_PRIVATE_KEY=${E2E_EVM_DEPLOYER_PRIVATE_KEY:-0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80}
PAYER_ADDRESS=${E2E_EVM_PAYER_ADDRESS:-0x70997970C51812dc3A010C7d01b50e0d17dc79C8}
SECONDARY_PAYER_PRIVATE_KEY=${E2E_EVM_SECONDARY_PAYER_PRIVATE_KEY:-0000000000000000000000000000000000000000000000000000000000000002}
TOKEN_ADDRESS=${E2E_EVM_USDC_ADDRESS:-0x5FbDB2315678afecb367f032d93F642f64180aa3}
INITIAL_SUPPLY=${E2E_EVM_PAYER_INITIAL_USDC:-1000000000000}
BYTECODE_FILE=${E2E_EVM_USDC_BYTECODE:-/fixture/artifacts/TestUSDC.bin}
ARTIFACT_MANIFEST=${E2E_EVM_ARTIFACT_MANIFEST:-/fixture/artifacts/contracts.sha256}

die() {
  printf 'EVM fixture error: %s\n' "$*" >&2
  exit 1
}

test -r "$BYTECODE_FILE" || die "missing compiled TestUSDC bytecode: $BYTECODE_FILE"
test -r "$ARTIFACT_MANIFEST" || die "missing EVM artifact checksum manifest"
(cd "$(dirname "$ARTIFACT_MANIFEST")" && sha256sum --check "$(basename "$ARTIFACT_MANIFEST")") \
  >/dev/null || die "EVM fixture artifact checksum verification failed"
test "$(cast chain-id --rpc-url "$RPC_URL")" = "31337" || die "unexpected EVM chain id"
SECONDARY_PAYER_ADDRESS=$(cast wallet address --private-key "0x${SECONDARY_PAYER_PRIVATE_KEY#0x}")
test -n "$SECONDARY_PAYER_ADDRESS" || die "secondary payer private key has no address"
test "${SECONDARY_PAYER_ADDRESS,,}" != "${PAYER_ADDRESS,,}" || die "payer wallet addresses must be distinct"

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
  test -n "$bytecode" || die "compiled TestUSDC bytecode is empty"
  cast send --rpc-url "$RPC_URL" --private-key "$DEPLOYER_PRIVATE_KEY" \
    --create "0x${bytecode#0x}" 'constructor(address,uint256)' "$PAYER_ADDRESS" "$INITIAL_SUPPLY" \
    >/tmp/agentvault-test-usdc-deploy.log
fi

code=$(cast code --rpc-url "$RPC_URL" "$TOKEN_ADDRESS")
test "$code" != "0x" || die "TestUSDC deployment did not create code"
# The second local EOA is deliberately outside Anvil's prefunded mnemonic.
# Give it native gas headroom for AgentVault's live balance safety check and
# mint the same TestUSDC balance so wallet-pool selection is independently
# signable whichever member wins the stable tie-break.
cast rpc --rpc-url "$RPC_URL" anvil_setBalance "$SECONDARY_PAYER_ADDRESS" \
  0x3635C9ADC5DEA00000 >/dev/null
secondary_balance=$(cast call --rpc-url "$RPC_URL" "$TOKEN_ADDRESS" \
  'balanceOf(address)(uint256)' "$SECONDARY_PAYER_ADDRESS" | awk '{print $1}')
if test "$secondary_balance" = 0; then
  cast send --rpc-url "$RPC_URL" --private-key "$DEPLOYER_PRIVATE_KEY" \
    "$TOKEN_ADDRESS" 'mint(address,uint256)' "$SECONDARY_PAYER_ADDRESS" "$INITIAL_SUPPLY" >/dev/null
fi

name=$(cast call --rpc-url "$RPC_URL" "$TOKEN_ADDRESS" 'name()(string)')
version=$(cast call --rpc-url "$RPC_URL" "$TOKEN_ADDRESS" 'version()(string)')
decimals=$(cast call --rpc-url "$RPC_URL" "$TOKEN_ADDRESS" 'decimals()(uint8)' | awk '{print $1}')
balance=$(cast call --rpc-url "$RPC_URL" "$TOKEN_ADDRESS" 'balanceOf(address)(uint256)' "$PAYER_ADDRESS" | awk '{print $1}')
secondary_balance=$(cast call --rpc-url "$RPC_URL" "$TOKEN_ADDRESS" \
  'balanceOf(address)(uint256)' "$SECONDARY_PAYER_ADDRESS" | awk '{print $1}')
test "$name" = '"USDC"' || die "unexpected token name: $name"
test "$version" = '"2"' || die "unexpected EIP-712 version: $version"
test "$decimals" = "6" || die "unexpected token decimals: $decimals"
test "$balance" = "$INITIAL_SUPPLY" || die "unexpected payer balance: $balance"
test "$secondary_balance" = "$INITIAL_SUPPLY" || die "unexpected secondary payer balance: $secondary_balance"

printf '{"network":"eip155:31337","token":"%s","payers":[{"address":"%s","balance":"%s"},{"address":"%s","balance":"%s"}],"decimals":6}\n' \
  "$TOKEN_ADDRESS" "$PAYER_ADDRESS" "$balance" "$SECONDARY_PAYER_ADDRESS" "$secondary_balance"
