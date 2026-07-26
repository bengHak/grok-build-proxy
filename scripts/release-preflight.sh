#!/bin/bash
set -euo pipefail

package_version() {
  cargo metadata --locked --no-deps --format-version 1 \
    | python3 -c 'import json,sys; data=json.load(sys.stdin); matches=[p["version"] for p in data["packages"] if p["name"] == "grok-build-proxy"]; assert len(matches) == 1; print(matches[0])'
}

case "${1:-}" in
  verify-version)
    version="$(package_version)"
    expected="v${version}"
    if [[ "${GITHUB_REF_TYPE:-}" != "tag" || "${GITHUB_REF_NAME:-}" != "${expected}" ]]; then
      echo "Release tag ${GITHUB_REF_NAME:-<missing>} does not match Cargo package version ${expected}" >&2
      exit 1
    fi
    printf 'tag=%s\n' "${expected}" >> "${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"
    ;;
  reject-existing-release)
    : "${GH_TOKEN:?GH_TOKEN is required}"
    : "${GH_REPO:?GH_REPO is required}"
    : "${TAG:?TAG is required}"
    existing_release="$(gh api \
      --method GET \
      --paginate \
      -H "Accept: application/vnd.github+json" \
      -H "X-GitHub-Api-Version: 2022-11-28" \
      "/repos/${GH_REPO}/releases?per_page=100" \
      --jq '.[] | select(.tag_name == env.TAG) | .tag_name')"
    if [[ -n "${existing_release}" ]]; then
      echo "Release ${TAG} already exists; refusing to replace draft or published assets." >&2
      exit 1
    fi
    ;;
  *)
    echo "usage: $0 {verify-version|reject-existing-release}" >&2
    exit 2
    ;;
esac
