#!/bin/sh
set -eu

fail() {
  printf 'build contract failed: %s\n' "$*" >&2
  exit 1
}

package_version=$(cargo metadata --locked --no-deps --format-version 1 \
  | python3 -c 'import json,sys; data=json.load(sys.stdin); matches=[p["version"] for p in data["packages"] if p["name"] == "grok-build-proxy"]; assert len(matches) == 1; print(matches[0])')

[ ! -e VERSION ] || fail 'VERSION must not exist; Cargo.toml is authoritative'
[ ! -e build.rs ] || fail 'build.rs version override must not exist'

if git grep -n -E 'GROK_BUILD_PROXY_BUILD_VERSION|GROK_BUILD_PROXY_VERSION' -- ':!install.sh' ':!README.md' ':!docs/**' ':!scripts/check-build-contract.sh'; then
  fail 'build-time version override remains'
fi

grep -q 'const VERSION: &str = env!("CARGO_PKG_VERSION");' src/main.rs \
  || fail 'CLI must use CARGO_PKG_VERSION'

make_version=$(make --no-print-directory print-version)
[ "$make_version" = "$package_version" ] \
  || fail "Make version $make_version differs from Cargo version $package_version"

[ -f .github/workflows/quality.yml ] || fail 'reusable quality workflow is missing'
grep -q 'workflow_call:' .github/workflows/quality.yml || fail 'quality workflow must support workflow_call'
grep -q 'uses: ./.github/workflows/quality.yml' .github/workflows/ci.yml || fail 'CI must call quality workflow'

remote_actions=$(grep -R -E '^[[:space:]]*- uses: ' .github/workflows \
  | sed -E 's/^[^:]+:[[:space:]]*- uses: //' \
  | grep -v '^\./' || true)
invalid_actions=$(printf '%s\n' "$remote_actions" \
  | grep -vE '^[^@[:space:]]+@[0-9a-f]{40}([[:space:]]+#.*)?$' || true)
[ -z "$invalid_actions" ] || fail "GitHub Actions must be pinned to full commit SHAs:\n$invalid_actions"

grep -q 'tags:' .github/workflows/release.yml || fail 'release must be tag-triggered'
if grep -qE 'workflow_dispatch:|branches:' .github/workflows/release.yml; then
  fail 'release must not allow manual or branch triggers'
fi
grep -q 'uses: ./.github/workflows/quality.yml' .github/workflows/release.yml || fail 'release must call quality workflow'
grep -q 'upload_dist: true' .github/workflows/release.yml || fail 'release must request validated artifacts'
grep -q 'Release .* already exists' .github/workflows/release.yml || fail 'release must reject an existing release'
if grep -qE -- '--clobber|gh release upload|make dist|cargo (build|test|clippy)' .github/workflows/release.yml; then
  fail 'release publish path must not overwrite or rebuild'
fi

printf 'build contract ok: version %s\n' "$package_version"
