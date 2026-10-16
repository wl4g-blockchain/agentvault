#!/usr/bin/env bash
# Provision one AWS Secrets Manager payload, a least-privilege EKS Pod Identity
# role, and the Helm values required by AgentVault's AWS CSI integration.

set -Eeuo pipefail
source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/setup-secret.sh"

AWS_PROFILE_NAME="${AWS_PROFILE:-default}"
AWS_REGION_NAME="${AWS_REGION:-ap-southeast-1}"
AWS_CLUSTER_NAME="${AWS_CLUSTER_NAME:-}"
AWS_SECRET_ID=""
AWS_ROLE_NAME="agentvault-secrets-reader"
AWS_VALUES_FILE="${PWD}/agentvault-aws-values.yaml"
AWS_KMS_KEY_ID=""
AWS_KMS_POLICY_TEMPLATE_OUTPUT=""
ENABLE_POD_IDENTITY=true

usage() {
  cat <<'EOF'
Usage: setup-secret-aws.sh [OPTIONS]

Create or update one AWS Secrets Manager secret whose value is the AgentVault
KEY=VALUE env-file. The script creates a least-privilege role and associates it
with the AgentVault Kubernetes ServiceAccount through EKS Pod Identity unless
that association is platform-managed.

AWS options:
  --profile NAME           AWS CLI profile (default: $AWS_PROFILE or default).
  --region REGION          AWS region (default: $AWS_REGION or ap-southeast-1).
  --cluster NAME           EKS cluster name; required for Pod Identity.
  --secret-id ID           Secret name (default: --secret-name).
  --role-name NAME         IAM role name (default: agentvault-secrets-reader).
  --kms-key-id KEY         Existing customer-managed KMS key for a new secret.
  --write-kms-key-policy FILE
                            Write a copy-and-edit KMS key policy template and exit.
  --values-file PATH       Generated non-secret Helm values file.
  --skip-pod-identity      Do not create or associate the IAM role.

The KMS key policy remains the owning organisation's responsibility. When
--kms-key-id is used, the generated role policy includes kms:Decrypt for it.

EOF
  print_common_options
}

while (($#)); do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --profile) need_value --profile "${2:-}"; AWS_PROFILE_NAME="$2"; shift 2 ;;
    --region) need_value --region "${2:-}"; AWS_REGION_NAME="$2"; shift 2 ;;
    --cluster) need_value --cluster "${2:-}"; AWS_CLUSTER_NAME="$2"; shift 2 ;;
    --secret-id) need_value --secret-id "${2:-}"; AWS_SECRET_ID="$2"; shift 2 ;;
    --role-name) need_value --role-name "${2:-}"; AWS_ROLE_NAME="$2"; shift 2 ;;
    --kms-key-id) need_value --kms-key-id "${2:-}"; AWS_KMS_KEY_ID="$2"; shift 2 ;;
    --write-kms-key-policy)
      need_value --write-kms-key-policy "${2:-}"
      AWS_KMS_POLICY_TEMPLATE_OUTPUT="$2"
      shift 2
      ;;
    --values-file) need_value --values-file "${2:-}"; AWS_VALUES_FILE="$2"; shift 2 ;;
    --skip-pod-identity) ENABLE_POD_IDENTITY=false; shift ;;
    *)
      if parse_common_option "$@"; then shift "$COMMON_ARGC"; else die "unknown option: $1 (use --help)"; fi
      ;;
  esac
done

if [[ -n "$AWS_KMS_POLICY_TEMPLATE_OUTPUT" ]]; then
  cat >"$AWS_KMS_POLICY_TEMPLATE_OUTPUT" <<'EOF'
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "EnableRootAdministration",
      "Effect": "Allow",
      "Principal": { "AWS": "arn:aws:iam::REPLACE_AWS_ACCOUNT_ID:root" },
      "Action": "kms:*",
      "Resource": "*"
    },
    {
      "Sid": "AllowAgentVaultSecretReaderDecrypt",
      "Effect": "Allow",
      "Principal": { "AWS": "arn:aws:iam::REPLACE_AWS_ACCOUNT_ID:role/REPLACE_READER_ROLE_NAME" },
      "Action": ["kms:Decrypt", "kms:DescribeKey"],
      "Resource": "*"
    }
  ]
}
EOF
  log "wrote AWS KMS key policy template: ${AWS_KMS_POLICY_TEMPLATE_OUTPUT}"
  exit 0
fi

[[ "$ENABLE_POD_IDENTITY" == false || -n "$AWS_CLUSTER_NAME" ]] \
  || die "--cluster is required unless --skip-pod-identity is used"
AWS_SECRET_ID="${AWS_SECRET_ID:-$AGENTVAULT_SECRET_NAME}"
validate_secret_file

if [[ "$DRY_RUN" == true ]]; then
  log "would create/update AWS secret ${AWS_SECRET_ID} in ${AWS_REGION_NAME} using profile ${AWS_PROFILE_NAME}"
  [[ "$ENABLE_POD_IDENTITY" == true ]] && log "would associate EKS ${AWS_CLUSTER_NAME}/${AGENTVAULT_NAMESPACE}/${AGENTVAULT_KSA}"
else
  require_command aws
  aws sts get-caller-identity --profile="$AWS_PROFILE_NAME" --region="$AWS_REGION_NAME" >/dev/null
  if ! aws secretsmanager describe-secret --secret-id="$AWS_SECRET_ID" \
    --profile="$AWS_PROFILE_NAME" --region="$AWS_REGION_NAME" >/dev/null 2>&1; then
    create_args=(secretsmanager create-secret --name="$AWS_SECRET_ID" \
      --secret-string="file://${AGENTVAULT_SECRET_FILE}" --profile="$AWS_PROFILE_NAME" \
      --region="$AWS_REGION_NAME")
    [[ -n "$AWS_KMS_KEY_ID" ]] && create_args+=(--kms-key-id="$AWS_KMS_KEY_ID")
    aws "${create_args[@]}" >/dev/null
  else
    log "AWS secret already exists; retaining its KMS key"
    aws secretsmanager put-secret-value --secret-id="$AWS_SECRET_ID" \
      --secret-string="file://${AGENTVAULT_SECRET_FILE}" --profile="$AWS_PROFILE_NAME" \
      --region="$AWS_REGION_NAME" >/dev/null
  fi
  AWS_SECRET_ARN="$(aws secretsmanager describe-secret --secret-id="$AWS_SECRET_ID" \
    --profile="$AWS_PROFILE_NAME" --region="$AWS_REGION_NAME" --query ARN --output text)"

  if [[ "$ENABLE_POD_IDENTITY" == true ]]; then
    work_dir="$(mktemp -d)"
    trap 'rm -rf "$work_dir"' EXIT
    cat >"${work_dir}/trust.json" <<'EOF'
{
  "Version": "2012-10-17",
  "Statement": [{
    "Effect": "Allow",
    "Principal": { "Service": "pods.eks.amazonaws.com" },
    "Action": ["sts:AssumeRole", "sts:TagSession"]
  }]
}
EOF
    cat >"${work_dir}/policy.json" <<EOF
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": ["secretsmanager:GetSecretValue", "secretsmanager:DescribeSecret"],
      "Resource": "${AWS_SECRET_ARN}"
    }$(if [[ -n "$AWS_KMS_KEY_ID" ]]; then printf ',\n    {\n      "Effect": "Allow",\n      "Action": ["kms:Decrypt"],\n      "Resource": "%s"\n    }' "$AWS_KMS_KEY_ID"; fi)
  ]
}
EOF
    if ! aws iam get-role --role-name="$AWS_ROLE_NAME" --profile="$AWS_PROFILE_NAME" >/dev/null 2>&1; then
      aws iam create-role --role-name="$AWS_ROLE_NAME" --profile="$AWS_PROFILE_NAME" \
        --assume-role-policy-document="file://${work_dir}/trust.json" >/dev/null
    fi
    aws iam put-role-policy --role-name="$AWS_ROLE_NAME" --profile="$AWS_PROFILE_NAME" \
      --policy-name=AgentVaultReadOneSecret --policy-document="file://${work_dir}/policy.json"
    AWS_ROLE_ARN="$(aws iam get-role --role-name="$AWS_ROLE_NAME" --profile="$AWS_PROFILE_NAME" \
      --query 'Role.Arn' --output text)"
    association_ids="$(aws eks list-pod-identity-associations --cluster-name="$AWS_CLUSTER_NAME" \
      --namespace="$AGENTVAULT_NAMESPACE" --service-account="$AGENTVAULT_KSA" \
      --profile="$AWS_PROFILE_NAME" --region="$AWS_REGION_NAME" \
      --query 'associations[].associationId' --output text)"
    if [[ -z "$association_ids" || "$association_ids" == None ]]; then
      aws eks create-pod-identity-association --cluster-name="$AWS_CLUSTER_NAME" \
        --role-arn="$AWS_ROLE_ARN" --namespace="$AGENTVAULT_NAMESPACE" \
        --service-account="$AGENTVAULT_KSA" --profile="$AWS_PROFILE_NAME" \
        --region="$AWS_REGION_NAME" >/dev/null
    else
      for association_id in $association_ids; do
        associated_role="$(aws eks describe-pod-identity-association --cluster-name="$AWS_CLUSTER_NAME" \
          --association-id="$association_id" --profile="$AWS_PROFILE_NAME" \
          --region="$AWS_REGION_NAME" --query 'association.roleArn' --output text)"
        [[ "$associated_role" == "$AWS_ROLE_ARN" ]] \
          || die "existing Pod Identity association ${association_id} uses ${associated_role}, not ${AWS_ROLE_ARN}"
      done
      log "matching EKS Pod Identity association already exists"
    fi
  fi
fi

sync_kubernetes_secret
write_values_file "$AWS_VALUES_FILE" "secrets:
  provider: aws
  kubernetes:
    existingSecret: ${AGENTVAULT_SECRET_NAME}
  aws:
    region: ${AWS_REGION_NAME}
    secretName: ${AWS_SECRET_ID}"
