#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPOSITORY_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../../../.." && pwd)
COMPOSE_FILE="$SCRIPT_DIR/../docker-compose.yaml"
PROJECT_NAME=agentvault-e2e
RUNTIME_ROOT="$SCRIPT_DIR/../reports/.runtime"
AGENTVAULT_CHART="$REPOSITORY_ROOT/deploy/helm/agentvault"
AGENTVAULT_VALUES="$SCRIPT_DIR/../common/agentvault-values.yaml"
AGENTVAULT_CONFIG="$RUNTIME_ROOT/agentvault.yaml"

export E2E_POSTGRES_PORT=${E2E_POSTGRES_PORT:-25432}
export E2E_EMQX_MQTT_PORT=${E2E_EMQX_MQTT_PORT:-28883}
export E2E_EMQX_DASHBOARD_PORT=${E2E_EMQX_DASHBOARD_PORT:-38084}
export E2E_JAEGER_UI_PORT=${E2E_JAEGER_UI_PORT:-26686}
export E2E_JAEGER_OTLP_GRPC_PORT=${E2E_JAEGER_OTLP_GRPC_PORT:-24317}
export E2E_JAEGER_OTLP_HTTP_PORT=${E2E_JAEGER_OTLP_HTTP_PORT:-24318}
export E2E_ANVIL_PORT=${E2E_ANVIL_PORT:-28545}
export E2E_SOLANA_RPC_PORT=${E2E_SOLANA_RPC_PORT:-28899}
export E2E_SOLANA_FAUCET_PORT=${E2E_SOLANA_FAUCET_PORT:-28900}
export E2E_FACILITATOR_PORT=${E2E_FACILITATOR_PORT:-38085}
export E2E_AGENTVAULT_PORT=${E2E_AGENTVAULT_PORT:-38080}
export E2E_AGENTVAULT_MGMT_PORT=${E2E_AGENTVAULT_MGMT_PORT:-39091}
export E2E_MARKETPULSE_PORT=${E2E_MARKETPULSE_PORT:-38081}
export E2E_QUANTSTREAM_UI_PORT=${E2E_QUANTSTREAM_UI_PORT:-38083}
export E2E_EMQX_DASHBOARD_PASSWORD=${E2E_EMQX_DASHBOARD_PASSWORD:-agentvault-e2e-dashboard}
export E2E_AGENTVAULT_MQTT_PASSWORD=${E2E_AGENTVAULT_MQTT_PASSWORD:-agentvault-e2e-vault}
export E2E_QUANTSTREAM_MQTT_PASSWORD=${E2E_QUANTSTREAM_MQTT_PASSWORD:-agentvault-e2e-workload}
export E2E_QUANTSTREAM_UI_MQTT_PASSWORD=${E2E_QUANTSTREAM_UI_MQTT_PASSWORD:-agentvault-e2e-approver}
export E2E_EVM_PAYER_ADDRESS=${E2E_EVM_PAYER_ADDRESS:-0x70997970C51812dc3A010C7d01b50e0d17dc79C8}
export E2E_QUANTSTREAM_SECONDARY_PAYER_PRIVATE_KEY=${E2E_QUANTSTREAM_SECONDARY_PAYER_PRIVATE_KEY:-0000000000000000000000000000000000000000000000000000000000000002}

if docker info >/dev/null 2>&1; then
  DOCKER=(docker)
else
  DOCKER=(sudo -E docker)
fi
COMPOSE=("${DOCKER[@]}" compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE")

die() {
  printf 'AgentVault E2E error: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'USAGE'
Usage:
  deploy/e2e.sh config
  deploy/e2e.sh up payer|payee
  deploy/e2e.sh health payer|payee
  deploy/e2e.sh smoke payer|payee
  deploy/e2e.sh full <payer-language> <payee-language>
  deploy/e2e.sh matrix
  deploy/e2e.sh down

smoke checks real dependencies only. full runs AgentVault + one selected
MarketPulse Payee + one selected QuantStream Payer and asserts MCP delivery,
intent, receipt, ledger, and EVM balance evidence. Supported languages are
rust, go, python, typescript, and java. E2E_RUNNER_KEEP_STACK=1 leaves a
smoke/full stack running.
Stacks are retained by default; set E2E_RUNNER_KEEP_STACK=0 or use runner.py cleanup
only when explicit teardown is wanted.
USAGE
}

validate_component_manifests() {
  local compose_file chart index=0
  local compose_count=0 chart_count=0

  while IFS= read -r compose_file; do
    "${DOCKER[@]}" compose -f "$compose_file" config --quiet
    compose_count=$((compose_count + 1))
  done < <(find "$SCRIPT_DIR" -mindepth 2 -maxdepth 2 -name docker-compose.yaml -type f | sort)

  command -v helm >/dev/null 2>&1 || die "helm is required to validate component charts"
  while IFS= read -r chart; do
    index=$((index + 1))
    helm lint "$chart" >/dev/null
    helm template "agentvault-e2e-$index" "$chart" >/dev/null
    chart_count=$((chart_count + 1))
  done < <(find "$SCRIPT_DIR" -mindepth 2 -maxdepth 2 -type d -name helm | sort)

  test "$compose_count" -gt 0 || die "no component docker-compose.yaml manifests found"
  test "$compose_count" = "$chart_count" ||
    die "component manifest mismatch: $compose_count Compose files but $chart_count Helm charts"
  test "$compose_count" = 18 ||
    die "expected 18 dependency/application component manifests, found $compose_count"
  printf 'Validated %s component Compose manifests and %s Helm charts.\n' \
    "$compose_count" "$chart_count"
}

render_agentvault_config() {
  local rendered
  command -v helm >/dev/null 2>&1 || die "helm is required to render AgentVault configuration"
  test -f "$AGENTVAULT_CHART/Chart.yaml" || die "production AgentVault Helm chart is missing"
  test -f "$AGENTVAULT_VALUES" || die "AgentVault E2E values are missing"
  helm lint "$AGENTVAULT_CHART" -f "$AGENTVAULT_VALUES" >/dev/null
  mkdir -p "$RUNTIME_ROOT"
  rendered=$(mktemp "${TMPDIR:-/tmp}/agentvault-helm-config.XXXXXX")
  helm template agentvault-e2e "$AGENTVAULT_CHART" \
    -f "$AGENTVAULT_VALUES" --show-only templates/configmap.yaml >"$rendered"
  awk '
    /^  agentvault.yaml: \|$/ { active=1; next }
    active && /^    / { sub(/^    /, ""); print; next }
    active && /^$/ { print; next }
    active { exit }
  ' "$rendered" >"$AGENTVAULT_CONFIG"
  rm -f -- "$rendered"
  test -s "$AGENTVAULT_CONFIG" || die "production Helm chart rendered an empty AgentVault configuration"
  printf 'Rendered AgentVault runtime config from %s.\n' "$AGENTVAULT_CHART"
}

profiles() {
  local role=$1
  case "$role" in
    payer) printf '%s\n' --profile infra --profile payer ;;
    payee) printf '%s\n' --profile infra --profile payee ;;
    *) die "role must be payer or payee" ;;
  esac
}

compose_role() {
  local role=$1
  shift
  local -a selected=()
  mapfile -t selected < <(profiles "$role")
  "${COMPOSE[@]}" "${selected[@]}" "$@"
}

compose_full() {
  local payer_language=$1
  local payee_language=$2
  shift 2
  "${COMPOSE[@]}" --profile infra --profile payee --profile apps \
    --profile "payee-$payee_language" --profile "ingestor-$payer_language" "$@"
}

down_stack() {
  # Compose v1 with network_mode: service:<anchor> can stop the dependants on
  # its first pass and leave the already-exited namespace anchor behind.
  # A second idempotent pass removes that anchor and the project volumes.
  "${COMPOSE[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true
  "${COMPOSE[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true
}

wait_http() {
  local url=$1
  local attempts=${2:-60}
  local index
  for ((index = 1; index <= attempts; index++)); do
    if curl --fail --silent --show-error --max-time 2 "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  die "timed out waiting for $url"
}

wait_rpc_result() {
  local url=$1
  local method=$2
  local expected=$3
  local attempts=${4:-60}
  local response index
  for ((index = 1; index <= attempts; index++)); do
    response=$(curl --silent --show-error --max-time 2 \
      -H 'content-type: application/json' \
      --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$method\",\"params\":[]}" \
      "$url" 2>/dev/null || true)
    if RESPONSE="$response" EXPECTED="$expected" python3 - <<'PY'
import json, os, sys
try:
    value = json.loads(os.environ["RESPONSE"]).get("result")
except (ValueError, TypeError):
    sys.exit(1)
sys.exit(0 if value == os.environ["EXPECTED"] else 1)
PY
    then
      return 0
    fi
    sleep 1
  done
  die "timed out waiting for $method at $url"
}

emqx_token() {
  local response_file status token
  response_file=$(mktemp "${TMPDIR:-/tmp}/agentvault-emqx-login.XXXXXX")
  status=$(printf '{"username":"admin","password":"%s"}' "$E2E_EMQX_DASHBOARD_PASSWORD" | \
    curl --silent --show-error --output "$response_file" --write-out '%{http_code}' \
      -H 'content-type: application/json' --data-binary @- \
      "http://127.0.0.1:${E2E_EMQX_DASHBOARD_PORT}/api/v5/login" || true)
  if test "$status" != 200; then
    rm -f -- "$response_file"
    die "EMQX dashboard authentication failed with HTTP $status"
  fi
  token=$(python3 - "$response_file" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8")).get("token")
if not isinstance(value, str) or not value:
    raise SystemExit(1)
print(value)
PY
  )
  rm -f -- "$response_file"
  printf '%s' "$token"
}

emqx_api() {
  local method=$1
  local path=$2
  local body=${3:-}
  local output=$4
  local token=$5
  if test -n "$body"; then
    printf '%s' "$body" | curl --silent --show-error --output "$output" --write-out '%{http_code}' \
      -X "$method" -H "authorization: Bearer $token" -H 'content-type: application/json' \
      --data-binary @- "http://127.0.0.1:${E2E_EMQX_DASHBOARD_PORT}$path"
  else
    curl --silent --show-error --output "$output" --write-out '%{http_code}' \
      -X "$method" -H "authorization: Bearer $token" \
      "http://127.0.0.1:${E2E_EMQX_DASHBOARD_PORT}$path"
  fi
}

upsert_mqtt_user() {
  local token=$1
  local user=$2
  local password=$3
  local response_file status body path
  response_file=$(mktemp "${TMPDIR:-/tmp}/agentvault-emqx-user.XXXXXX")
  body=$(printf '{"user_id":"%s","password":"%s","is_superuser":false}' "$user" "$password")
  path='/api/v5/authentication/password_based%3Abuilt_in_database/users'
  status=$(emqx_api POST "$path" "$body" "$response_file" "$token")
  if test "$status" = 409; then
    body=$(printf '{"password":"%s","is_superuser":false}' "$password")
    status=$(emqx_api PUT "$path/$user" "$body" "$response_file" "$token")
  fi
  rm -f -- "$response_file"
  case "$status" in
    200|201|204) ;;
    *) die "EMQX user $user provisioning failed with HTTP $status" ;;
  esac
}

bootstrap_emqx() {
  wait_http "http://127.0.0.1:${E2E_EMQX_DASHBOARD_PORT}/status" 90
  local token response_file status body
  token=$(emqx_token)
  response_file=$(mktemp "${TMPDIR:-/tmp}/agentvault-emqx-authn.XXXXXX")
  status=$(emqx_api GET /api/v5/authentication '' "$response_file" "$token")
  test "$status" = 200 || {
    rm -f -- "$response_file"
    die "read EMQX authentication configuration failed with HTTP $status"
  }
  if ! python3 - "$response_file" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
raise SystemExit(0 if "password_based:built_in_database" in json.dumps(value) else 1)
PY
  then
    body='{"mechanism":"password_based","backend":"built_in_database","user_id_type":"username","password_hash_algorithm":{"name":"sha256","salt_position":"suffix"}}'
    status=$(emqx_api POST /api/v5/authentication "$body" "$response_file" "$token")
    case "$status" in
      200|201|204|409) ;;
      *) rm -f -- "$response_file"; die "create EMQX password authenticator failed with HTTP $status" ;;
    esac
  fi
  rm -f -- "$response_file"
  upsert_mqtt_user "$token" agentvault-vault "$E2E_AGENTVAULT_MQTT_PASSWORD"
  upsert_mqtt_user "$token" quantstream-workload "$E2E_QUANTSTREAM_MQTT_PASSWORD"
  upsert_mqtt_user "$token" quantstream-approver "$E2E_QUANTSTREAM_UI_MQTT_PASSWORD"
  MQTT_PORT="$E2E_EMQX_MQTT_PORT" \
    MQTT_WORKLOAD_PASSWORD="$E2E_QUANTSTREAM_MQTT_PASSWORD" \
    MQTT_APPROVER_PASSWORD="$E2E_QUANTSTREAM_UI_MQTT_PASSWORD" \
    MQTT_VAULT_PASSWORD="$E2E_AGENTVAULT_MQTT_PASSWORD" \
    python3 "$SCRIPT_DIR/emqx/verify_acl.py"
}

dependency_up() {
  local role=$1
  compose_role "$role" up -d anvil solana postgres emqx jaeger
  wait_rpc_result "http://127.0.0.1:${E2E_ANVIL_PORT}" eth_chainId 0x7a69 60
  wait_rpc_result "http://127.0.0.1:${E2E_SOLANA_RPC_PORT}" getHealth ok 90
  compose_role "$role" up --no-deps evm-fixture
  bootstrap_emqx
  if test "$role" = payee; then
    compose_role "$role" up -d --no-deps facilitator
    wait_http "http://127.0.0.1:${E2E_FACILITATOR_PORT}/health" 60
  fi
}

dependency_health() {
  local role=$1
  wait_rpc_result "http://127.0.0.1:${E2E_ANVIL_PORT}" eth_chainId 0x7a69 5
  wait_rpc_result "http://127.0.0.1:${E2E_SOLANA_RPC_PORT}" getHealth ok 5
  compose_role "$role" exec -T postgres pg_isready -U agentvault -d agentvault >/dev/null
  compose_role "$role" exec -T emqx /opt/emqx/bin/emqx ctl status >/dev/null
  wait_http "http://127.0.0.1:${E2E_JAEGER_UI_PORT}/" 5
  compose_role "$role" up --no-deps evm-fixture >/dev/null
  bootstrap_emqx
  if test "$role" = payee; then
    local supported
    wait_http "http://127.0.0.1:${E2E_FACILITATOR_PORT}/health" 5
    supported=$(curl --fail --silent --show-error \
      "http://127.0.0.1:${E2E_FACILITATOR_PORT}/supported")
    SUPPORTED="$supported" python3 - <<'PY'
import json, os
value = json.loads(os.environ["SUPPORTED"])
encoded = json.dumps(value, separators=(",", ":"))
if "eip155:31337" not in encoded or "exact" not in encoded:
    raise SystemExit("facilitator does not advertise x402 exact on eip155:31337")
PY
  elif compose_role payer ps facilitator 2>/dev/null | grep -q 'Up'; then
    die "payer profile unexpectedly started the facilitator"
  fi
  printf 'Dependency %s smoke passed (Solana node health only; no Solana payment asserted).\n' "$role"
}

check_target_budget() {
  local mebibytes=0
  if test -d "$REPOSITORY_ROOT/target"; then
    mebibytes=$(du -sm "$REPOSITORY_ROOT/target" | awk '{print $1}')
  fi
  if ((mebibytes >= 10240)); then
    printf 'target/ is %s MiB; cleaning before the Rust image build.\n' "$mebibytes" >&2
    (cd "$REPOSITORY_ROOT" && cargo clean)
  fi
}

wait_for_host_capacity() {
  local processors threshold current_load attempt
  processors=$(getconf _NPROCESSORS_ONLN 2>/dev/null || printf '1')
  case "$processors" in
    ''|*[!0-9]*) processors=1 ;;
  esac
  threshold=$((processors + processors / 2))
  if ((threshold < 1)); then
    threshold=1
  fi
  for ((attempt = 1; attempt <= 30; attempt++)); do
    read -r current_load _ </proc/loadavg
    if awk -v current_load="$current_load" -v threshold="$threshold" \
      'BEGIN { exit !(current_load <= threshold) }'; then
      return 0
    fi
    sleep 2
  done
  printf 'Host load remained above %s after 60 seconds; continuing serially.\n' \
    "$threshold" >&2
}

build_full_images() {
  local payer_language=$1
  local payee_language=$2
  local -a targets=(
    "marketpulse-$payee_language"
    admin-ui
    "quantstream-$payer_language"
  )
  if test "${E2E_RUNNER_SKIP_BUILD:-0}" = 1; then
    printf 'Skipping E2E image build by explicit request.\n'
    return 0
  fi
  check_target_budget
  if test "${E2E_RUNNER_PREBUILT_AGENTVAULT:-0}" = 1; then
    test -n "${E2E_AGENTVAULT_IMAGE:-}" || \
      die "E2E_RUNNER_PREBUILT_AGENTVAULT requires E2E_AGENTVAULT_IMAGE"
  else
    targets=(agentvault "${targets[@]}")
  fi
  if test "${E2E_RUNNER_REUSE_BUILD:-0}" = 1; then
    compose_full "$payer_language" "$payee_language" build "${targets[@]}"
  else
    compose_full "$payer_language" "$payee_language" build --pull "${targets[@]}"
  fi
}

preflight_mcp() {
  local free_body paid_headers paid_body status
  free_body=$(mktemp "${TMPDIR:-/tmp}/agentvault-free.XXXXXX")
  paid_headers=$(mktemp "${TMPDIR:-/tmp}/agentvault-paid-headers.XXXXXX")
  paid_body=$(mktemp "${TMPDIR:-/tmp}/agentvault-paid-body.XXXXXX")
  curl --fail --silent --show-error -H 'content-type: application/json' \
    -H 'x-run-id: preflight-free' \
    --data '{"jsonrpc":"2.0","id":"free","method":"tools/call","params":{"name":"marketpulse.yesterday_news","arguments":{}}}' \
    "http://127.0.0.1:${E2E_MARKETPULSE_PORT}/mcp" >"$free_body"
  status=$(curl --silent --show-error --output "$paid_body" --dump-header "$paid_headers" --write-out '%{http_code}' \
    -H 'content-type: application/json' -H 'x-run-id: preflight-paid' \
    --data '{"jsonrpc":"2.0","id":"paid","method":"tools/call","params":{"name":"marketpulse.realtime_news","arguments":{}}}' \
    "http://127.0.0.1:${E2E_MARKETPULSE_PORT}/mcp")
  if test "$status" != 402; then
    local error_body
    error_body=$(head -c 2048 "$paid_body" | tr '\r\n' ' ')
    die "paid MCP preflight returned HTTP $status instead of 402: $error_body"
  fi
  grep -qi '^payment-required:' "$paid_headers" || die "paid MCP 402 omitted PAYMENT-REQUIRED"
  python3 - "$free_body" "$paid_body" <<'PY'
import json, sys
free = json.load(open(sys.argv[1], encoding="utf-8"))
paid = json.load(open(sys.argv[2], encoding="utf-8"))
free_result = free.get("result", {})
free_delivery = free_result.get("structuredContent", {})
if (free_delivery.get("paid") is not False
        or not isinstance(free_delivery.get("news"), list)
        or not free_delivery["news"]):
    raise SystemExit("free MCP response is not a free business delivery")
if free_result.get("isError") is not False or not free_result.get("content"):
    raise SystemExit("free MCP response is not a valid MCP tool result")
if paid.get("x402Version") != 2 or not paid.get("accepts"):
    raise SystemExit("paid MCP response is not an x402 v2 challenge")
PY
  rm -f -- "$free_body" "$paid_headers" "$paid_body"
}

valid_evidence_id() {
  test -n "$1" && ! printf '%s' "$1" | grep -q '[^A-Za-z0-9._-]'
}

wait_financial_evidence() {
  test $# = 4 || die "four settled intents are required"
  local identifier quoted="" separator="" result='' index settled ledger outbound inbound events
  for identifier in "$@"; do
    valid_evidence_id "$identifier" || die "auditor returned an unsafe intent id"
    quoted="${quoted}${separator}'${identifier}'"
    separator=,
  done
  for ((index = 1; index <= 45; index++)); do
    result=$(compose_full "$E2E_AUDITOR_PAYER_LANGUAGE" "$E2E_AUDITOR_PAYEE_LANGUAGE" exec -T postgres \
      psql -U agentvault -d agentvault -Atc \
      "SELECT (SELECT count(*) FROM pay_intent WHERE id IN ($quoted) AND status='settled' AND settled_amount=1000000),(SELECT count(*) FROM pay_ledger WHERE intent_id IN ($quoted) AND entry_type='payment' AND amount=1000000 AND tx_hash IS NOT NULL),(SELECT count(*) FROM pay_ledger WHERE intent_id IN ($quoted) AND entry_type='payment' AND direction='outbound'),(SELECT count(*) FROM pay_ledger WHERE intent_id IN ($quoted) AND entry_type='payment' AND direction='inbound'),(SELECT count(*) FROM pay_event WHERE intent_id IN ($quoted) AND event_type IN ('settled','reconciled'));" \
      | tr -d '\r')
    IFS='|' read -r settled ledger outbound inbound events <<<"$result"
    if test "$settled" = 4 && test "$ledger" = 4 && test "$outbound" = 2 \
      && test "$inbound" = 2 && test "${events:-0}" -ge 4; then
      return 0
    fi
    sleep 1
  done
  die "financial evidence did not converge (last counts: $result)"
}

assert_receipt() {
  local scenario=$1
  local intent=$2
  local direction=$3
  local body
  body=$(curl --fail --silent --show-error \
    "http://127.0.0.1:${E2E_AGENTVAULT_PORT}/api/v1/payments/$intent/receipt?scenario_id=$scenario")
  RECEIPT="$body" INTENT="$intent" DIRECTION="$direction" python3 - <<'PY'
import json, os
value = json.loads(os.environ["RECEIPT"])
if value.get("intent_id") != os.environ["INTENT"] or value.get("status") != "settled":
    raise SystemExit("receipt is not bound to a settled intent")
asset = value.get("asset", {})
if value.get("direction") != os.environ["DIRECTION"] or value.get("protocol") != "x402":
    raise SystemExit("receipt direction or protocol is invalid")
if asset.get("network") != "eip155:31337" or asset.get("asset") != "USDC" or asset.get("decimals") != 6:
    raise SystemExit("receipt asset identity is invalid")
if str(asset.get("token_contract", "")).lower() != "0x5fbdb2315678afecb367f032d93f642f64180aa3":
    raise SystemExit("receipt token contract is invalid")
if str(value.get("recipient", "")).lower() != "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc":
    raise SystemExit("receipt recipient is invalid")
if not value.get("tx_hash") or not value.get("evidence_hash") or value.get("settled_amount") != "1000000":
    raise SystemExit("receipt has no confirmed EVM transfer")
PY
}

assert_token_balances() {
  local primary secondary secondary_address payee
  secondary_address=$(compose_full "$E2E_AUDITOR_PAYER_LANGUAGE" "$E2E_AUDITOR_PAYEE_LANGUAGE" exec -T anvil \
    cast wallet address --private-key "0x${E2E_QUANTSTREAM_SECONDARY_PAYER_PRIVATE_KEY#0x}" | tr -d '\r')
  primary=$(compose_full "$E2E_AUDITOR_PAYER_LANGUAGE" "$E2E_AUDITOR_PAYEE_LANGUAGE" exec -T anvil cast call \
    --rpc-url http://127.0.0.1:8545 0x5fbdb2315678afecb367f032d93f642f64180aa3 \
    'balanceOf(address)(uint256)' "$E2E_EVM_PAYER_ADDRESS" | awk '{print $1}')
  secondary=$(compose_full "$E2E_AUDITOR_PAYER_LANGUAGE" "$E2E_AUDITOR_PAYEE_LANGUAGE" exec -T anvil cast call \
    --rpc-url http://127.0.0.1:8545 0x5fbdb2315678afecb367f032d93f642f64180aa3 \
    'balanceOf(address)(uint256)' "$secondary_address" | awk '{print $1}')
  payee=$(compose_full "$E2E_AUDITOR_PAYER_LANGUAGE" "$E2E_AUDITOR_PAYEE_LANGUAGE" exec -T anvil cast call \
    --rpc-url http://127.0.0.1:8545 0x5fbdb2315678afecb367f032d93f642f64180aa3 \
    'balanceOf(address)(uint256)' 0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc | awk '{print $1}')
  test "$primary" = 999999000000 || die "unexpected primary payer TestUSDC balance: $primary"
  test "$secondary" = 999999000000 || die "unexpected secondary payer TestUSDC balance: $secondary"
  test "$payee" = 2000000 || die "unexpected payee TestUSDC balance: $payee"
}

assert_wallet_pool_selection() {
  local result intent_count wallet_count wallet_ids membership_count
  result=$(compose_full "$E2E_AUDITOR_PAYER_LANGUAGE" "$E2E_AUDITOR_PAYEE_LANGUAGE" exec -T postgres \
    psql -U agentvault -d agentvault -Atc \
    "SELECT count(*),count(DISTINCT wallet_id),string_agg(DISTINCT wallet_id, ',' ORDER BY wallet_id) FROM pay_intent WHERE scenario_id='scenario-quantstream-market-ingestion' AND direction='outbound' AND status='settled';" \
    | tr -d '\r')
  IFS='|' read -r intent_count wallet_count wallet_ids <<<"$result"
  test "$intent_count" = 2 || die "wallet-pool proof expected two settled outbound intents: $result"
  test "$wallet_count" = 2 || die "AgentVault did not distribute two payments across two wallets: $result"
  test "$wallet_ids" = "wallet-quantstream-primary,wallet-quantstream-secondary" || \
    die "AgentVault selected unexpected wallet-pool members: $wallet_ids"
  membership_count=$(compose_full "$E2E_AUDITOR_PAYER_LANGUAGE" "$E2E_AUDITOR_PAYEE_LANGUAGE" exec -T postgres \
    psql -U agentvault -d agentvault -Atc \
    "SELECT count(*) FROM pay_policy_wallet WHERE policy_id='policy-quantstream-market-data' AND wallet_id IN ('wallet-quantstream-primary','wallet-quantstream-secondary');" \
    | tr -d '\r')
  test "$membership_count" = 2 || die "QuantStream policy is not bound to both wallet-pool members"
  printf 'AgentVault selected wallet pool members: %s.\n' "$wallet_ids"
}

wait_and_approve() {
  local container_name=$1
  local response_file status approval_id='' intent_id='' requested='' index
  response_file=$(mktemp "${TMPDIR:-/tmp}/agentvault-approval.XXXXXX")
  for ((index = 1; index <= 45; index++)); do
    status=$(curl --silent --show-error --output "$response_file" --write-out '%{http_code}' \
      -H "authorization: Bearer ${E2E_QUANTSTREAM_UI_TOKEN:-quantstream-e2e-ui-token-0000000000000000001}" \
      "http://127.0.0.1:${E2E_QUANTSTREAM_UI_PORT}/api/approvals" || true)
    if test "$status" = 200; then
      mapfile -t approval_fields < <(python3 - "$response_file" <<'PY'
import json, sys
items = json.load(open(sys.argv[1], encoding="utf-8")).get("items", [])
if len(items) == 1:
    item = items[0]
    print(item.get("approvalId", ""))
    print(item.get("intentId", ""))
    print(item.get("requestedAmount", ""))
PY
      )
      if test ${#approval_fields[@]} = 3 && test -n "${approval_fields[0]}"; then
        approval_id=${approval_fields[0]}
        intent_id=${approval_fields[1]}
        requested=${approval_fields[2]}
        break
      fi
    fi
    sleep 1
  done
  valid_evidence_id "$approval_id" && valid_evidence_id "$intent_id" || die "Admin UI did not expose one pending approval"
  test "$requested" = 1000000 || die "approval requested an unexpected amount: $requested"
  status=$(curl --silent --show-error --output "$response_file" --write-out '%{http_code}' \
    -X POST -H 'content-type: application/json' \
    -H "authorization: Bearer ${E2E_QUANTSTREAM_UI_TOKEN:-quantstream-e2e-ui-token-0000000000000000001}" \
    --data '{"decision":"approve"}' \
    "http://127.0.0.1:${E2E_QUANTSTREAM_UI_PORT}/api/approvals/$approval_id/decision" || true)
  case "$status" in 200|201) ;; *) die "Admin UI approval failed with HTTP $status" ;; esac
  local exit_code
  exit_code=$("${DOCKER[@]}" wait "$container_name")
  if test "$exit_code" != 0; then
    "${DOCKER[@]}" logs "$container_name" >&2 || true
    die "approved ingestor exited with code $exit_code"
  fi
  "${DOCKER[@]}" logs "$container_name"
  "${DOCKER[@]}" rm "$container_name" >/dev/null
  printf 'Approval %s for intent %s was accepted through the business Admin UI.\n' "$approval_id" "$intent_id"
}

assert_consumed_approval() {
  local result
  result=$(compose_full "$E2E_AUDITOR_PAYER_LANGUAGE" "$E2E_AUDITOR_PAYEE_LANGUAGE" exec -T postgres \
    psql -U agentvault -d agentvault -Atc \
    "SELECT count(*) FROM pay_approval WHERE status='consumed' AND requested_amount=1000000 AND approved_amount=1000000 AND approved_by='principal-quantstream-admin' AND consumed_at IS NOT NULL AND length(grant_jti) > 0 AND grant_claims_hash ~ '^[0-9a-f]{64}$' AND (sealed_grant_token->>'version')::INTEGER > 0 AND sealed_grant_token->>'key_id'='recovery-v1' AND length(sealed_grant_token->>'nonce') > 16 AND length(sealed_grant_token->>'ciphertext') > 32;")
  test "$(printf '%s' "$result" | tr -d '\r')" = 1 || die "one-time encrypted approval grant was not atomically consumed"
}

cleanup_if_requested() {
  local status=$?
  if ((status != 0)); then
    printf '%s\n' '--- AgentVault E2E failure diagnostics ---' >&2
    "${COMPOSE[@]}" ps >&2 || true
    "${COMPOSE[@]}" logs --no-color --tail=200 \
      agentvault "marketpulse-${E2E_AUDITOR_PAYEE_LANGUAGE:-rust}" \
      facilitator emqx postgres anvil >&2 || true
  fi
  if test "${E2E_RUNNER_KEEP_STACK:-1}" != 1; then
    down_stack
  fi
  return "$status"
}

full_flow() {
  local payer_language=$1
  local payee_language=$2
  case "$payer_language" in rust|go|python|typescript|java) ;; *) die "unsupported payer language: $payer_language" ;; esac
  case "$payee_language" in rust|go|python|typescript|java) ;; *) die "unsupported payee language: $payee_language" ;; esac
  export E2E_AUDITOR_PAYER_LANGUAGE=$payer_language
  export E2E_AUDITOR_PAYEE_LANGUAGE=$payee_language
  render_agentvault_config
  down_stack
  trap cleanup_if_requested EXIT INT TERM
  # Build before starting the validator so the resource-intensive Rust image
  # compile and Solana's steady-state validator load never overlap.
  build_full_images "$payer_language" "$payee_language"
  dependency_up payee
  dependency_health payee
  compose_full "$payer_language" "$payee_language" up --no-deps agentvault-provision
  compose_full "$payer_language" "$payee_language" up -d --no-deps agentvault
  wait_http "http://127.0.0.1:${E2E_AGENTVAULT_MGMT_PORT}/readyz" 90
  compose_full "$payer_language" "$payee_language" up -d --no-deps \
    "marketpulse-$payee_language" admin-ui
  wait_http "http://127.0.0.1:${E2E_MARKETPULSE_PORT}/healthz" 60
  wait_http "http://127.0.0.1:${E2E_QUANTSTREAM_UI_PORT}/healthz" 60
  preflight_mcp
  compose_full "$payer_language" "$payee_language" run --rm --no-deps \
    -e QUANTSTREAM_E2E_FAIL_AFTER_SETTLEMENT=once "quantstream-$payer_language"
  # Workflow run ids include wall-clock seconds; force the second logical
  # operation into a distinct stable run_id namespace.
  sleep 2
  local approval_container="agentvault-e2e-${payer_language}-${payee_language}-approval"
  if "${DOCKER[@]}" ps -a --format '{{.Names}}' | grep -qx "$approval_container"; then
    "${DOCKER[@]}" rm -f "$approval_container" >/dev/null
  fi
  compose_full "$payer_language" "$payee_language" run -d --name "$approval_container" \
    --no-deps "quantstream-$payer_language" >/dev/null
  wait_and_approve "$approval_container"
  local audit_output audit_json
  audit_output=$(compose_full "$payer_language" "$payee_language" run -T --rm --no-deps e2e-auditor)
  audit_json=$(printf '%s\n' "$audit_output" | grep '^{' | tail -n 1)
  test -n "$audit_json" || die "business evidence auditor returned no JSON"
  local -a outbound_intents=() inbound_intents=()
  mapfile -t outbound_intents < <(AUDIT="$audit_json" python3 -c 'import json,os; print(*json.loads(os.environ["AUDIT"])["outbound_intent_ids"], sep="\n")')
  mapfile -t inbound_intents < <(AUDIT="$audit_json" python3 -c 'import json,os; print(*json.loads(os.environ["AUDIT"])["inbound_intent_ids"], sep="\n")')
  test ${#outbound_intents[@]} = 2 && test ${#inbound_intents[@]} = 2 || die "auditor did not return two bidirectional payment pairs"
  wait_financial_evidence "${outbound_intents[@]}" "${inbound_intents[@]}"
  local intent
  for intent in "${outbound_intents[@]}"; do
    assert_receipt scenario-quantstream-market-ingestion "$intent" outbound
  done
  for intent in "${inbound_intents[@]}"; do
    assert_receipt scenario-marketpulse-feed "$intent" inbound
  done
  assert_consumed_approval
  assert_wallet_pool_selection
  assert_token_balances
  printf '%s\n' "$audit_json"
  printf 'Full payer=%s/payee=%s x402 E2E passed: automatic payment + approved overage, receipts, ledgers, and delivery.\n' \
    "$payer_language" "$payee_language"
}

command=${1:-}
case "$command" in
  config)
    render_agentvault_config
    compose_role payer config --quiet
    compose_role payee config --quiet
    for payer_language in rust go python typescript java; do
      for payee_language in rust go python typescript java; do
        compose_full "$payer_language" "$payee_language" config --quiet
      done
    done
    validate_component_manifests
    ;;
  up)
    test $# = 2 || die "up requires payer or payee"
    dependency_up "$2"
    dependency_health "$2"
    ;;
  health)
    test $# = 2 || die "health requires payer or payee"
    dependency_health "$2"
    ;;
  smoke)
    test $# = 2 || die "smoke requires payer or payee"
    down_stack
    trap cleanup_if_requested EXIT INT TERM
    dependency_up "$2"
    dependency_health "$2"
    ;;
  full)
    test $# = 3 || die "full requires payer and payee languages"
    full_flow "$2" "$3"
    ;;
  matrix)
    test $# = 1 || die "matrix accepts no extra arguments"
    for payer_language in rust go python typescript java; do
      for payee_language in rust go python typescript java; do
        "$SCRIPT_DIR/e2e.sh" full "$payer_language" "$payee_language"
        wait_for_host_capacity
      done
    done
    ;;
  down)
    test $# = 1 || die "down accepts no extra arguments"
    down_stack
    ;;
  *) usage; exit 2 ;;
esac
