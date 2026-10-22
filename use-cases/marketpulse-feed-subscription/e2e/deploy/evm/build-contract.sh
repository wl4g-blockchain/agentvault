#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SOLC_VERSION=0.8.24
SOLC_FILE=solc-linux-amd64-v0.8.24+commit.e11b9ed9
SOLC_SHA256=fb03a29a517452b9f12bcf459ef37d0a543765bb3bbc911e70a87d6a37c30d5f
SOLC_URL="https://binaries.soliditylang.org/linux-amd64/${SOLC_FILE}"

temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/agentvault-solc.XXXXXX")
trap 'rm -rf -- "$temporary_dir"' EXIT
solc_path="$temporary_dir/solc"

curl --fail --silent --show-error --location "$SOLC_URL" --output "$solc_path"
printf '%s  %s\n' "$SOLC_SHA256" "$solc_path" | sha256sum --check --status
chmod 0755 "$solc_path"
mkdir -p "$SCRIPT_DIR/artifacts"
"$solc_path" \
  --optimize \
  --optimize-runs 200 \
  --metadata-hash none \
  --bin \
  --abi \
  --overwrite \
  --output-dir "$SCRIPT_DIR/artifacts" \
  "$SCRIPT_DIR/src/E2EUSDC.sol"

(cd "$SCRIPT_DIR/artifacts" && sha256sum --check contracts.sha256) >/dev/null

printf 'Built E2EUSDC with solc %s (%s)\n' "$SOLC_VERSION" "$SOLC_SHA256"
