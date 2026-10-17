#!/usr/bin/env bash
# Provision one Secret Manager payload, its narrow reader identity, and the
# Helm values required by AgentVault's GCP Secrets Store CSI integration.

set -Eeuo pipefail
source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/setup-secret.sh"

GCP_PROJECT_ID="${GCP_PROJECT_ID:-}"
GCP_ACCOUNT="${GCP_ACCOUNT:-}"
GCP_SECRET_ID=""
GCP_GSA_NAME="agentvault-secrets"
GCP_VALUES_FILE="${PWD}/agentvault-gcp-values.yaml"
GCP_KMS_KEY=""
GCP_REPLICATION_POLICY_FILE=""
GCP_REPLICATION_TEMPLATE_OUTPUT=""
ENABLE_WORKLOAD_IDENTITY=true

usage() {
  cat <<'EOF'
Usage: setup-secret-gcp.sh --project PROJECT_ID --account ACCOUNT [OPTIONS]

Create or update one GCP Secret Manager secret whose value is the AgentVault
KEY=VALUE env-file. The script creates a least-privilege Google service account
and the GKE Workload Identity binding unless that binding is platform-managed.

GCP options:
  --project PROJECT_ID       Secret Manager project (or set GCP_PROJECT_ID).
  --account ACCOUNT          Authenticated gcloud account (or set GCP_ACCOUNT).
  --secret-id ID             Secret Manager ID (default: --secret-name).
  --gsa-name NAME            Reader service-account local name (default: agentvault-secrets).
  --values-file PATH         Generated non-secret Helm values file.
  --kms-key RESOURCE         Existing CMEK resource for a newly created secret.
  --replication-policy-file FILE
                            Existing Secret Manager replica policy JSON for a new secret.
  --write-replication-policy FILE
                            Write a copy-and-edit regional CMEK policy template and exit.
  --skip-workload-identity   Do not create the GSA/KSA IAM binding or annotation.

KMS keys and replica policy are normally provisioned by central IaC. `--kms-key`
is valid only with automatic replication; use `--replication-policy-file` for
regional replica/CMEK policies. Existing secrets retain their replication and
encryption settings. The generated policy template uses one regional replica;
each replica's CMEK key must be in that replica's location, and the Secret
Manager service agent needs roles/cloudkms.cryptoKeyEncrypterDecrypter.

EOF
  print_common_options
}

while (($#)); do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --project) need_value --project "${2:-}"; GCP_PROJECT_ID="$2"; shift 2 ;;
    --account) need_value --account "${2:-}"; GCP_ACCOUNT="$2"; shift 2 ;;
    --secret-id) need_value --secret-id "${2:-}"; GCP_SECRET_ID="$2"; shift 2 ;;
    --gsa-name) need_value --gsa-name "${2:-}"; GCP_GSA_NAME="$2"; shift 2 ;;
    --values-file) need_value --values-file "${2:-}"; GCP_VALUES_FILE="$2"; shift 2 ;;
    --kms-key) need_value --kms-key "${2:-}"; GCP_KMS_KEY="$2"; shift 2 ;;
    --replication-policy-file)
      need_value --replication-policy-file "${2:-}"
      GCP_REPLICATION_POLICY_FILE="$2"
      shift 2
      ;;
    --write-replication-policy)
      need_value --write-replication-policy "${2:-}"
      GCP_REPLICATION_TEMPLATE_OUTPUT="$2"
      shift 2
      ;;
    --skip-workload-identity) ENABLE_WORKLOAD_IDENTITY=false; shift ;;
    *)
      if parse_common_option "$@"; then shift "$COMMON_ARGC"; else die "unknown option: $1 (use --help)"; fi
      ;;
  esac
done

if [[ -n "$GCP_REPLICATION_TEMPLATE_OUTPUT" ]]; then
  cat >"$GCP_REPLICATION_TEMPLATE_OUTPUT" <<'EOF'
{
  "userManaged": {
    "replicas": [{
      "location": "REPLACE_REGION",
      "customerManagedEncryption": {
        "kmsKeyName": "projects/REPLACE_KMS_PROJECT/locations/REPLACE_REGION/keyRings/REPLACE_KEYRING/cryptoKeys/REPLACE_KEY"
      }
    }]
  }
}
EOF
  log "wrote GCP Secret Manager replication/CMEK template: ${GCP_REPLICATION_TEMPLATE_OUTPUT}"
  exit 0
fi

[[ -n "$GCP_PROJECT_ID" ]] || die "--project is required"
[[ -n "$GCP_ACCOUNT" ]] || die "--account is required"
GCP_SECRET_ID="${GCP_SECRET_ID:-$AGENTVAULT_SECRET_NAME}"
[[ -z "$GCP_KMS_KEY" || -z "$GCP_REPLICATION_POLICY_FILE" ]] \
  || die "--kms-key and --replication-policy-file cannot be combined"
[[ -z "$GCP_REPLICATION_POLICY_FILE" || -f "$GCP_REPLICATION_POLICY_FILE" ]] \
  || die "replication policy file does not exist: $GCP_REPLICATION_POLICY_FILE"
if [[ -n "$GCP_REPLICATION_POLICY_FILE" ]] \
  && grep -q 'REPLACE_' "$GCP_REPLICATION_POLICY_FILE"; then
  die "replace every REPLACE_ value in ${GCP_REPLICATION_POLICY_FILE}"
fi
validate_secret_file

GCP_GSA_EMAIL="${GCP_GSA_NAME}@${GCP_PROJECT_ID}.iam.gserviceaccount.com"
if [[ "$DRY_RUN" == true ]]; then
  log "would create/update GCP Secret Manager secret ${GCP_PROJECT_ID}/${GCP_SECRET_ID} as ${GCP_ACCOUNT}"
  [[ "$ENABLE_WORKLOAD_IDENTITY" == true ]] && log "would bind ${AGENTVAULT_NAMESPACE}/${AGENTVAULT_KSA} to ${GCP_GSA_EMAIL}"
else
  require_command gcloud
  gcloud auth list --filter="account=${GCP_ACCOUNT}" --format='value(account)' | grep -Fx "$GCP_ACCOUNT" >/dev/null \
    || die "gcloud account is not authenticated: ${GCP_ACCOUNT}"
  gcloud services enable secretmanager.googleapis.com iamcredentials.googleapis.com \
    --project="$GCP_PROJECT_ID" --account="$GCP_ACCOUNT"
  if ! gcloud secrets describe "$GCP_SECRET_ID" --project="$GCP_PROJECT_ID" \
    --account="$GCP_ACCOUNT" >/dev/null 2>&1; then
    create_args=(secrets create "$GCP_SECRET_ID" --project="$GCP_PROJECT_ID" --account="$GCP_ACCOUNT")
    if [[ -n "$GCP_REPLICATION_POLICY_FILE" ]]; then
      create_args+=(--replication-policy-file="$GCP_REPLICATION_POLICY_FILE")
    else
      create_args+=(--replication-policy=automatic)
      [[ -n "$GCP_KMS_KEY" ]] && create_args+=(--kms-key-name="$GCP_KMS_KEY")
    fi
    gcloud "${create_args[@]}"
  else
    log "GCP secret already exists; retaining its replication and encryption policy"
  fi
  gcloud secrets versions add "$GCP_SECRET_ID" --data-file="$AGENTVAULT_SECRET_FILE" \
    --project="$GCP_PROJECT_ID" --account="$GCP_ACCOUNT"
  if [[ "$ENABLE_WORKLOAD_IDENTITY" == true ]]; then
    if ! gcloud iam service-accounts describe "$GCP_GSA_EMAIL" \
      --project="$GCP_PROJECT_ID" --account="$GCP_ACCOUNT" >/dev/null 2>&1; then
      gcloud iam service-accounts create "$GCP_GSA_NAME" --project="$GCP_PROJECT_ID" \
        --account="$GCP_ACCOUNT" --display-name='AgentVault Secret Manager reader'
    fi
    gcloud secrets add-iam-policy-binding "$GCP_SECRET_ID" \
      --member="serviceAccount:${GCP_GSA_EMAIL}" --role='roles/secretmanager.secretAccessor' \
      --project="$GCP_PROJECT_ID" --account="$GCP_ACCOUNT"
    gcloud iam service-accounts add-iam-policy-binding "$GCP_GSA_EMAIL" \
      --member="serviceAccount:${GCP_PROJECT_ID}.svc.id.goog[${AGENTVAULT_NAMESPACE}/${AGENTVAULT_KSA}]" \
      --role='roles/iam.workloadIdentityUser' --project="$GCP_PROJECT_ID" --account="$GCP_ACCOUNT"
  fi
fi

sync_kubernetes_secret

values="secrets:
  provider: gcp
  kubernetes:
    existingSecret: ${AGENTVAULT_SECRET_NAME}
  gcp:
    secretResourceName: projects/${GCP_PROJECT_ID}/secrets/${GCP_SECRET_ID}/versions/latest"
if [[ "$ENABLE_WORKLOAD_IDENTITY" == true ]]; then
  values+="
  serviceAccount:
    annotations:
      iam.gke.io/gcp-service-account: ${GCP_GSA_EMAIL}"
fi
write_values_file "$GCP_VALUES_FILE" "$values"
