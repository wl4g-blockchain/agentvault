#!/usr/bin/env bash
# Provision the application-scoped Vault KV v2 payload, policy, Kubernetes auth
# role, and Helm values. Cluster-wide Vault auth configuration stays platform-owned.

set -Eeuo pipefail
source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/setup-secret.sh"

VAULT_KV_MOUNT="${VAULT_KV_MOUNT:-kv}"
VAULT_SECRET_PATH="${VAULT_SECRET_PATH:-agentvault-secrets}"
VAULT_POLICY_NAME="${VAULT_POLICY_NAME:-agentvault-secrets-read}"
VAULT_ROLE_NAME="${VAULT_ROLE_NAME:-agentvault}"
VAULT_AUTH_MOUNT="${VAULT_AUTH_MOUNT:-kubernetes}"
VAULT_VALUES_FILE="${PWD}/agentvault-vault-values.yaml"
ENABLE_KV_MOUNT=true

usage() {
  cat <<'EOF'
Usage: setup-secret-vault.sh [OPTIONS]

Write the AgentVault KEY=VALUE env-file to a Vault KV v2 secret, grant a
least-privilege policy, create the Kubernetes auth role, and write a non-secret
Helm values override. VAULT_ADDR and a privileged VAULT_TOKEN must already be
configured; set VAULT_NAMESPACE too when using Vault Enterprise namespaces.

Vault options:
  --kv-mount PATH          KV v2 mount (default: $VAULT_KV_MOUNT or kv).
  --secret-path PATH       KV secret path (default: agentvault-secrets).
  --policy NAME            Vault policy name (default: agentvault-secrets-read).
  --role NAME              Kubernetes auth role (default: agentvault).
  --auth-mount PATH        Kubernetes auth mount (default: kubernetes).
  --values-file PATH       Generated non-secret Helm values file.
  --existing-kv-mount      Fail if the KV v2 mount does not already exist.

Prerequisite: the Vault platform must configure auth/<auth-mount>/config and
install the Vault Agent Injector for this Kubernetes cluster. This script does
not alter that cluster-wide authentication trust.

EOF
  print_common_options
}

while (($#)); do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --kv-mount) need_value --kv-mount "${2:-}"; VAULT_KV_MOUNT="$2"; shift 2 ;;
    --secret-path) need_value --secret-path "${2:-}"; VAULT_SECRET_PATH="$2"; shift 2 ;;
    --policy) need_value --policy "${2:-}"; VAULT_POLICY_NAME="$2"; shift 2 ;;
    --role) need_value --role "${2:-}"; VAULT_ROLE_NAME="$2"; shift 2 ;;
    --auth-mount) need_value --auth-mount "${2:-}"; VAULT_AUTH_MOUNT="$2"; shift 2 ;;
    --values-file) need_value --values-file "${2:-}"; VAULT_VALUES_FILE="$2"; shift 2 ;;
    --existing-kv-mount) ENABLE_KV_MOUNT=false; shift ;;
    *)
      if parse_common_option "$@"; then shift "$COMMON_ARGC"; else die "unknown option: $1 (use --help)"; fi
      ;;
  esac
done

validate_secret_file
if [[ "$DRY_RUN" == true ]]; then
  log "would create/update Vault KV v2 ${VAULT_KV_MOUNT}/${VAULT_SECRET_PATH} and role ${VAULT_ROLE_NAME}"
else
  require_command vault
  vault status >/dev/null
  if ! vault secrets list -format=json | grep -q "\"${VAULT_KV_MOUNT}/\""; then
    [[ "$ENABLE_KV_MOUNT" == true ]] || die "KV v2 mount does not exist: ${VAULT_KV_MOUNT}"
    vault secrets enable -path="$VAULT_KV_MOUNT" kv-v2
  fi
  vault kv put -mount="$VAULT_KV_MOUNT" "$VAULT_SECRET_PATH" \
    "env=@${AGENTVAULT_SECRET_FILE}" >/dev/null
  vault policy write "$VAULT_POLICY_NAME" - <<EOF
path "${VAULT_KV_MOUNT}/data/${VAULT_SECRET_PATH}" {
  capabilities = ["read"]
}
EOF
  vault write "auth/${VAULT_AUTH_MOUNT}/role/${VAULT_ROLE_NAME}" \
    bound_service_account_names="$AGENTVAULT_KSA" \
    bound_service_account_namespaces="$AGENTVAULT_NAMESPACE" \
    policies="$VAULT_POLICY_NAME" ttl=1h >/dev/null
fi

sync_kubernetes_secret
write_values_file "$VAULT_VALUES_FILE" "agentvault:
  backend:
    secrets:
      provider: vault
      kubernetes:
        existingSecret: ${AGENTVAULT_SECRET_NAME}
      vault:
        vaultRole: ${VAULT_ROLE_NAME}
        secretPath: ${VAULT_KV_MOUNT}/data/${VAULT_SECRET_PATH}"
