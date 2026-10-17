#!/usr/bin/env bash
# Initialise the Kubernetes Secret consumed by AgentVault.

set -Eeuo pipefail
source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/setup-secret.sh"

usage() {
  cat <<'EOF'
Usage: setup-secret-k8s.sh [OPTIONS]

Create or update the Kubernetes Secret that supplies every AgentVault runtime
credential through the chart's kubernetes secret provider.

EOF
  print_common_options
}

while (($#)); do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    *)
      if parse_common_option "$@"; then
        shift "$COMMON_ARGC"
      else
        die "unknown option: $1 (use --help)"
      fi
      ;;
  esac
done

validate_secret_file
[[ "$SKIP_KUBERNETES_MIRROR" == false ]] \
  || die "--skip-kubernetes-mirror is not valid for the Kubernetes Secret bootstrap"
sync_kubernetes_secret
