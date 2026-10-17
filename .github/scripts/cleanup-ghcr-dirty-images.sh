#!/usr/bin/env bash

# Delete only the GHCR image versions tagged by commits from one merged PR.
# Cleanup is best-effort and refuses to delete a manifest carrying any tag
# outside that PR's immutable dirty-* tag set.
set -uo pipefail

warn() {
  echo "::warning title=Dirty image cleanup::$*"
}

if [[ $# -lt 4 ]]; then
  warn "usage: $0 <owner/repository> <pull-request> <package-owner> <package>..."
  exit 0
fi

repository=$1
pull_request=$2
package_owner=$3
shift 3
packages=("$@")

current_commit_shas=$(gh api --paginate \
  "/repos/${repository}/pulls/${pull_request}/commits?per_page=100" \
  --jq '.[].sha' 2>/dev/null) || {
  warn "could not list commits for ${repository}#${pull_request}; dirty images remain"
  exit 0
}
timeline_commit_shas=$(gh api --paginate \
  -H "Accept: application/vnd.github+json" \
  "/repos/${repository}/issues/${pull_request}/timeline?per_page=100" \
  --jq '.[] | (.sha?, .commit_id?, .before_commit?, .after_commit?) | select(type == "string")' \
  2>/dev/null) || {
  warn "could not read the PR timeline; force-pushed dirty images may remain"
  timeline_commit_shas=""
}

declare -a dirty_tags=()
declare -A targeted_tags=()
while IFS= read -r commit_sha; do
  [[ -n "${commit_sha}" ]] || continue
  dirty_tag="dirty-${commit_sha:0:8}"
  [[ -z "${targeted_tags[${dirty_tag}]+present}" ]] || continue
  dirty_tags+=("${dirty_tag}")
  targeted_tags["${dirty_tag}"]=1
done <<< "${current_commit_shas}"$'\n'"${timeline_commit_shas}"

if [[ ${#dirty_tags[@]} -eq 0 ]]; then
  warn "no commits were returned for ${repository}#${pull_request}; dirty images remain"
  exit 0
fi

owner_type=$(gh api "/users/${package_owner}" --jq .type 2>/dev/null) || owner_type=""
case "${owner_type}" in
  Organization) package_scope="orgs" ;;
  User) package_scope="users" ;;
  *)
    warn "could not resolve GHCR owner type for ${package_owner}; dirty images remain"
    exit 0
    ;;
esac

dirty_tags_json=$(printf '%s\n' "${dirty_tags[@]}" | jq -R . | jq -s .)
temporary_directory=$(mktemp -d)
trap 'rm -rf -- "${temporary_directory}"' EXIT

deleted=0
failed=0
for package in "${packages[@]}"; do
  versions_file="${temporary_directory}/${package}.json"
  if ! gh api --paginate --slurp \
    "/${package_scope}/${package_owner}/packages/container/${package}/versions?per_page=100" \
    > "${versions_file}" 2>/dev/null; then
    warn "could not list versions for ghcr.io/${package_owner}/${package}"
    failed=$((failed + 1))
    continue
  fi

  while IFS=$'\t' read -r version_id version_tags matched_tags; do
    [[ -n "${version_id}" ]] || continue
    safe_to_delete=true
    IFS=',' read -ra attached_tags <<< "${version_tags}"
    for attached_tag in "${attached_tags[@]}"; do
      if [[ -z "${targeted_tags[${attached_tag}]+present}" ]]; then
        safe_to_delete=false
        break
      fi
    done
    if [[ "${safe_to_delete}" != true ]]; then
      warn "skipping ${package} version ${version_id}: PR tag ${matched_tags} shares its manifest with ${version_tags}"
      failed=$((failed + 1))
      continue
    fi
    if gh api --method DELETE \
      "/${package_scope}/${package_owner}/packages/container/${package}/versions/${version_id}" \
      >/dev/null 2>&1; then
      echo "Deleted ghcr.io/${package_owner}/${package}:${matched_tags}"
      deleted=$((deleted + 1))
    else
      warn "failed to delete ghcr.io/${package_owner}/${package}:${matched_tags} (version ${version_id})"
      failed=$((failed + 1))
    fi
  done < <(
    jq -r --argjson dirty_tags "${dirty_tags_json}" '
      .[][]
      | . as $version
      | [($version.metadata.container.tags // [])[]
           | select(. as $tag | $dirty_tags | index($tag) != null)] as $matches
      | select(($matches | length) > 0)
      | [($version.id | tostring),
         (($version.metadata.container.tags // []) | join(",")),
         ($matches | join(","))]
      | @tsv
    ' "${versions_file}"
  )
done

if [[ ${failed} -gt 0 ]]; then
  warn "dirty image cleanup completed with ${failed} warning(s); ${deleted} package version(s) deleted"
else
  echo "Dirty image cleanup complete: ${deleted} package version(s) deleted."
fi
exit 0
