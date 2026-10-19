#!/usr/bin/env bash
# Shared, intentionally small CLI foundation for AgentVault Helm secret bootstrap.
# Shellcheck source=/dev/null

set -Eeuo pipefail

readonly BOOTSTRAP_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly SECRET_TEMPLATE="${BOOTSTRAP_DIR}/agentvault-secrets.env.example"

AGENTVAULT_RELEASE="${AGENTVAULT_RELEASE:-agentvault}"
AGENTVAULT_NAMESPACE="${AGENTVAULT_NAMESPACE:-agentvault}"
AGENTVAULT_SECRET_NAME="${AGENTVAULT_SECRET_NAME:-agentvault-secrets}"
AGENTVAULT_SECRET_FILE="${AGENTVAULT_SECRET_FILE:-${PWD}/agentvault-secrets.env}"
AGENTVAULT_KSA="${AGENTVAULT_KSA:-${AGENTVAULT_RELEASE}-agentvault}"
DRY_RUN=false
SKIP_KUBERNETES_MIRROR=false
COMMON_ARGC=0

log() {
  printf '%s\n' "[agentvault-bootstrap] $*" >&2
}

die() {
  log "error: $*"
  exit 1
}

need_value() {
  [[ $# -eq 2 && -n "$2" && "$2" != --* ]] || die "$1 requires a value"
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command is not installed: $1"
}

print_common_options() {
  cat <<'EOF'
Common options:
  --release NAME          Helm release name (default: agentvault).
  --namespace NAME        Kubernetes namespace (default: agentvault).
  --service-account NAME  Chart ServiceAccount name (default: <release>-agentvault).
  --secret-name NAME      Logical Secret name (default: agentvault-secrets).
  --secret-file PATH      Filled KEY=VALUE env-file (default: ./agentvault-secrets.env).
  --skip-kubernetes-mirror
                          Do not create a Kubernetes mirror of a managed secret.
  --dry-run               Print intended operations without changing state.
  -h, --help              Show this help.
EOF
}

# Parses one common option. Callers shift COMMON_ARGC on success.
parse_common_option() {
  COMMON_ARGC=0
  case "${1:-}" in
    --release)
      need_value --release "${2:-}"
      AGENTVAULT_RELEASE="$2"
      AGENTVAULT_KSA="${AGENTVAULT_RELEASE}-agentvault"
      COMMON_ARGC=2
      ;;
    --namespace)
      need_value --namespace "${2:-}"
      AGENTVAULT_NAMESPACE="$2"
      COMMON_ARGC=2
      ;;
    --service-account)
      need_value --service-account "${2:-}"
      AGENTVAULT_KSA="$2"
      COMMON_ARGC=2
      ;;
    --secret-name)
      need_value --secret-name "${2:-}"
      AGENTVAULT_SECRET_NAME="$2"
      COMMON_ARGC=2
      ;;
    --secret-file)
      need_value --secret-file "${2:-}"
      AGENTVAULT_SECRET_FILE="$2"
      COMMON_ARGC=2
      ;;
    --skip-kubernetes-mirror)
      SKIP_KUBERNETES_MIRROR=true
      COMMON_ARGC=1
      ;;
    --dry-run)
      DRY_RUN=true
      COMMON_ARGC=1
      ;;
    *)
      return 1
      ;;
  esac
}

validate_secret_file() {
  if [[ ! -f "$AGENTVAULT_SECRET_FILE" ]]; then
    die "secret file is missing: ${AGENTVAULT_SECRET_FILE}; start with: cp ${SECRET_TEMPLATE} ${AGENTVAULT_SECRET_FILE}"
  fi
  if grep -nE '^[[:space:]]*[^#].*=<[^>]+>' "$AGENTVAULT_SECRET_FILE"; then
    die "replace every active <PLACEHOLDER> in ${AGENTVAULT_SECRET_FILE}"
  fi
  if grep -nEv '^[[:space:]]*(#.*)?$|^[A-Za-z0-9_]+=[^[:cntrl:]]*$' "$AGENTVAULT_SECRET_FILE"; then
    die "every active line in ${AGENTVAULT_SECRET_FILE} must use KEY=VALUE"
  fi
  duplicate_key="$(sed -nE 's/^([A-Za-z0-9_]+)=.*/\1/p' "$AGENTVAULT_SECRET_FILE" | sort | uniq -d | head -n 1)"
  [[ -z "$duplicate_key" ]] || die "secret file defines ${duplicate_key} more than once"
}

sync_kubernetes_secret() {
  if [[ "$SKIP_KUBERNETES_MIRROR" == true ]]; then
    log "Kubernetes Secret mirror skipped"
    return
  fi
  if [[ "$DRY_RUN" == true ]]; then
    log "would apply namespace ${AGENTVAULT_NAMESPACE} and Secret ${AGENTVAULT_SECRET_NAME} from ${AGENTVAULT_SECRET_FILE}"
    return
  fi
  require_command kubectl
  kubectl create namespace "$AGENTVAULT_NAMESPACE" --dry-run=client -o yaml | kubectl apply -f -
  kubectl -n "$AGENTVAULT_NAMESPACE" create secret generic "$AGENTVAULT_SECRET_NAME" \
    --from-env-file="$AGENTVAULT_SECRET_FILE" \
    --dry-run=client -o yaml | kubectl apply -f -
  log "Kubernetes Secret mirror is ready: ${AGENTVAULT_NAMESPACE}/${AGENTVAULT_SECRET_NAME}"
}

write_values_file() {
  local path="$1"
  local contents="$2"
  if [[ "$DRY_RUN" == true ]]; then
    log "would write Helm values override: ${path}"
    return
  fi
  umask 077
  printf '%s\n' "$contents" >"$path"
  log "wrote Helm values override: ${path}"
}
