#!/usr/bin/env bash
set -euo pipefail

# Explicit online provenance check for the immutable canonical runtimes used by
# x402-rs 1.4.9. The normal E2E never downloads contracts at runtime.
RPC_URL=${E2E_EVM_BASE_RPC_URL:-https://mainnet.base.org}
SOURCE_BLOCK=${E2E_EVM_BASE_SOURCE_BLOCK:-0x321faae}
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
(cd "$SCRIPT_DIR/artifacts" && sha256sum --check contracts.sha256) >/dev/null

verify_runtime() {
  local address=$1
  local artifact=$2
  local expected actual response runtime
  expected=$(tr -d '\r\n' <"$SCRIPT_DIR/artifacts/$artifact" | sha256sum | awk '{print $1}')
  response=$(curl --fail --silent --show-error --max-time 20 \
    -H 'content-type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_getCode\",\"params\":[\"$address\",\"$SOURCE_BLOCK\"]}" \
    "$RPC_URL")
  runtime=$(RESPONSE="$response" python3 -c \
    'import json,os; value=json.loads(os.environ["RESPONSE"]).get("result", ""); assert value.startswith("0x") and len(value)>4; print(value[2:])')
  actual=$(printf '%s' "$runtime" | sha256sum | awk '{print $1}')
  test "$actual" = "$expected" || {
    printf '%s runtime mismatch: expected %s, got %s\n' "$address" "$expected" "$actual" >&2
    return 1
  }
  printf '%s %s verified\n' "$address" "$artifact"
}

verify_runtime 0xdAcD51A54883eb67D95FAEb2BBfdC4a9a6BD2a3B EIP6492Validator.runtime.bin
verify_runtime 0x000000000022D473030F116dDEE9F6B43aC78BA3 Permit2.runtime.bin
verify_runtime 0x402085c248EeA27D92E8b30b2C58ed07f9E20001 ExactPermit2Proxy.runtime.bin
