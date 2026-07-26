#!/bin/sh
set -eu

python3 <<'PY'
from pathlib import Path
import os
import shutil
import subprocess
import sys
import tempfile

ROOT = Path.cwd()
FIXTURE_FILES = (
    "Cargo.lock",
    "Cargo.toml",
    "Makefile",
    "install.sh",
    "src/lib.rs",
    "src/main.rs",
    "src/proxy.rs",
    "tests/kimi_http.rs",
    ".github/workflows/ci.yml",
    ".github/workflows/quality.yml",
    ".github/workflows/release.yml",
    "scripts/check-build-contract.sh",
    "scripts/release-preflight.sh",
    "scripts/test-build-contract.sh",
)


def fail(message):
    print(f"build contract mutation tests failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def run_mutation(invariant, name, relative_path, old, new, expected_message):
    with tempfile.TemporaryDirectory(prefix="build-contract-mutation-") as temp_dir:
        repo = Path(temp_dir) / "repo"
        repo.mkdir()
        for relative in FIXTURE_FILES:
            source = ROOT / relative
            destination = repo / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
        path = repo / relative_path
        if old is None:
            if path.exists():
                fail(f"{invariant} / {name} expected absent fixture {relative_path}")
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(new)
        else:
            text = path.read_text()
            if text.count(old) != 1:
                fail(f"{invariant} / {name} has a non-unique fixture in {relative_path}")
            path.write_text(text.replace(old, new, 1))
        subprocess.run(["git", "init", "--quiet"], cwd=repo, check=True)
        subprocess.run(["git", "add", "."], cwd=repo, check=True)
        result = subprocess.run(
            ["./scripts/check-build-contract.sh"],
            cwd=repo,
            env=os.environ.copy(),
            capture_output=True,
            text=True,
        )
        output = result.stdout + result.stderr
        if result.returncode == 0:
            fail(f"{invariant} / {name} was accepted")
        if expected_message not in output:
            fail(f"{invariant} / {name} failed for the wrong reason:\n{output}")


MUTATIONS = [
    # N0-1: ordinary build/dist preserves Cargo output, even with hostile Make overrides.
    (
        "N0-1 incremental-safe dist",
        "implicit cargo clean",
        "Makefile",
        "prepare-dist:\n\trm -rf bin $(DIST_DIR)",
        "prepare-dist:\n\t$(CARGO) clean\n\trm -rf bin $(DIST_DIR)",
        "prepare-dist must replace only bin and dist packaging output",
    ),
    (
        "N0-1 incremental-safe dist",
        "implicit target deletion",
        "Makefile",
        "\trm -rf bin $(DIST_DIR)\n\tmkdir -p bin $(DIST_DIR)",
        "\trm -rf bin $(DIST_DIR) target\n\tmkdir -p bin $(DIST_DIR)",
        "prepare-dist must replace only bin and dist packaging output",
    ),
    (
        "N0-1 incremental-safe dist",
        "DIST_DIR command-line override enabled",
        "Makefile",
        "override DIST_DIR := dist",
        "DIST_DIR := dist",
        "safety variable DIST_DIR",
    ),
    (
        "N0-1 incremental-safe dist",
        "CARGO command-line injection enabled",
        "Makefile",
        "override CARGO := cargo",
        "CARGO := cargo",
        "safety variable CARGO",
    ),
    (
        "N0-1 incremental-safe dist",
        "recursive MAKE command-line injection enabled",
        "Makefile",
        "override MAKE := make",
        "MAKE := make",
        "safety variable MAKE",
    ),
    (
        "N0-1 incremental-safe dist",
        "PACKAGE_VERSION override enabled",
        "Makefile",
        "override PACKAGE_VERSION :=",
        "PACKAGE_VERSION :=",
        "package version must be non-overridable",
    ),
    # N0-2: exact package membership and checksum coverage.
    (
        "N0-2 validated archives",
        "arm64 archive membership weakened",
        "Makefile",
        "$(BINARY) LICENSE README.md\n\ttar -C $(DIST_DIR)/amd64",
        "$(BINARY) LICENSE\n\ttar -C $(DIST_DIR)/amd64",
        "dist must create the arm64 archive with exact membership",
    ),
    (
        "N0-2 validated archives",
        "checksum omits Intel archive",
        "Makefile",
        "shasum -a 256 $(BINARY)_Darwin_arm64.tar.gz $(BINARY)_Darwin_amd64.tar.gz > checksums.txt",
        "shasum -a 256 $(BINARY)_Darwin_arm64.tar.gz > checksums.txt",
        "checksums for exactly the arm64 and amd64 archives",
    ),
    (
        "N0-2 validated archives",
        "checksum verification removed",
        "Makefile",
        "\tcd $(DIST_DIR) && shasum -a 256 -c checksums.txt",
        "\t@true # checksum verification removed",
        "verify-dist must verify archive checksums",
    ),
    # N0-3: Cargo is the only project and runtime version source.
    (
        "N0-3 Cargo-only version",
        "VERSION file reintroduced",
        "VERSION",
        None,
        "0.0.18\n",
        "VERSION must not exist",
    ),
    (
        "N0-3 Cargo-only version",
        "build.rs version override reintroduced",
        "build.rs",
        None,
        'fn main() { println!("cargo:rustc-env=GROK_BUILD_PROXY_BUILD_VERSION=dev"); }\n',
        "build.rs version override must not exist",
    ),
    (
        "N0-3 Cargo-only version",
        "library version disconnected from Cargo",
        "src/lib.rs",
        'pub const VERSION: &str = env!("CARGO_PKG_VERSION");',
        'pub const VERSION: &str = "dev";',
        "library runtime version must use CARGO_PKG_VERSION",
    ),
    (
        "N0-3 Cargo-only version",
        "runtime ProxyConfig construction disconnected",
        "src/main.rs",
        "        client_token: a.client_token,\n        compatibility_version: a.codex_compat_version,",
        "        client_token: a.client_token,\n        version: \"dev\".into(),\n        compatibility_version: a.codex_compat_version,",
        "runtime version must not be injectable",
    ),
    (
        "N0-3 Cargo-only version",
        "ProxyConfig version injection restored",
        "src/proxy.rs",
        "    pub client_token: String,\n    pub compatibility_version: String,",
        "    pub client_token: String,\n    pub version: String,\n    pub compatibility_version: String,",
        "ProxyConfig runtime version must not be injectable",
    ),
    (
        "N0-3 Cargo-only version",
        "Codex User-Agent disconnected",
        "src/proxy.rs",
        "        .header(\n            header::USER_AGENT,\n            format!(\"grok-build-proxy/{}\", crate::VERSION),\n        )",
        "        .header(\n            header::USER_AGENT,\n            \"grok-build-proxy/dev\",\n        )",
        "Codex User-Agent must use the Cargo-derived runtime version",
    ),
    (
        "N0-3 Cargo-only version",
        "runtime response version disconnected",
        "src/proxy.rs",
        "        response.headers_mut().insert(\n            \"x-grok-build-proxy-version\",\n            HeaderValue::from_static(crate::VERSION),\n        );\n        return response;",
        "        response.headers_mut().insert(\n            \"x-grok-build-proxy-version\",\n            HeaderValue::from_static(\"dev\"),\n        );\n        return response;",
        "proxy response version headers must use the Cargo-derived runtime version",
    ),
    (
        "N0-3 Cargo-only version",
        "Make metadata package identity injectable",
        "Makefile",
        'p["name"] == "grok-build-proxy"',
        'p["name"] == "$(BINARY)"',
        "derive directly from locked Cargo metadata",
    ),
    # N0-4: workflows are valid YAML with understood root/job/step semantics.
    (
        "N0-4 real workflow parsing",
        "malformed trailing YAML",
        ".github/workflows/release.yml",
        "            --verify-tag\n",
        "            --verify-tag\nbroken: [\n",
        "workflow YAML validation failed",
    ),
    (
        "N0-4 real workflow parsing",
        "duplicate root jobs key",
        ".github/workflows/ci.yml",
        "jobs:\n  quality:",
        "jobs: {}\njobs:\n  quality:",
        "duplicate key",
    ),
    (
        "N0-4 real workflow parsing",
        "quoted unknown root key",
        ".github/workflows/ci.yml",
        "permissions:\n  contents: read",
        '"broken": []\n\npermissions:\n  contents: read',
        "unknown root keys",
    ),
    (
        "N0-4 real workflow parsing",
        "block scalar hides extra quality command",
        ".github/workflows/quality.yml",
        "      - name: Check formatting\n        run: cargo fmt --check",
        "      - name: Check formatting\n        run: |\n          cargo fmt --check\n          true",
        "Check formatting command must be exactly",
    ),
    # N0-5: quality inputs, runners, toolchain, and all gates are exact.
    (
        "N0-5 shared quality gates",
        "quality trigger broadened",
        ".github/workflows/quality.yml",
        "on:\n  workflow_call:",
        "on:\n  workflow_call:\n  workflow_dispatch:",
        "quality workflow must trigger only through workflow_call",
    ),
    (
        "N0-5 shared quality gates",
        "upload_dist type weakened",
        ".github/workflows/quality.yml",
        "        type: boolean",
        "        type: string",
        "upload_dist input must be boolean",
    ),
    (
        "N0-5 shared quality gates",
        "quality runner floated",
        ".github/workflows/quality.yml",
        "    runs-on: macos-14",
        "    runs-on: macos-latest",
        "quality job must run on macos-14",
    ),
    (
        "N0-5 shared quality gates",
        "Rust toolchain floated",
        ".github/workflows/quality.yml",
        "          toolchain: 1.88.0",
        "          toolchain: stable",
        "quality Rust toolchain must be exactly 1.88.0",
    ),
    (
        "N0-5 shared quality gates",
        "format gate removed",
        ".github/workflows/quality.yml",
        "        run: cargo fmt --check",
        "        run: true",
        "Check formatting command must be exactly",
    ),
    (
        "N0-5 shared quality gates",
        "Clippy gate removed",
        ".github/workflows/quality.yml",
        "        run: cargo clippy --locked --all-targets --all-features -- -D warnings",
        "        run: true",
        "Clippy command must be exactly",
    ),
    (
        "N0-5 shared quality gates",
        "test gate removed",
        ".github/workflows/quality.yml",
        "        run: cargo test --locked --all-targets",
        "        run: true",
        "Test command must be exactly",
    ),
    (
        "N0-5 shared quality gates",
        "installer gate removed",
        ".github/workflows/quality.yml",
        "        run: sh -n install.sh",
        "        run: true",
        "Validate installer command must be exactly",
    ),
    (
        "N0-5 shared quality gates",
        "remote action ref floated",
        ".github/workflows/quality.yml",
        "actions/checkout@11d5960a326750d5838078e36cf38b85af677262",
        "actions/checkout@v4",
        "remote uses must be pinned",
    ),
    # N0-6: release tag/version preflight executes exact shell semantics and fails closed.
    (
        "N0-6 executable release preflight",
        "inline-comment tag-gate bypass",
        "scripts/release-preflight.sh",
        'if [[ "${GITHUB_REF_TYPE:-}" != "tag" || "${GITHUB_REF_NAME:-}" != "${expected}" ]]; then',
        'if [[ "${GITHUB_REF_TYPE:-}" != "tag" || "${GITHUB_REF_NAME:-}" != v* ]]; then # [[ "${GITHUB_REF_NAME:-}" != "${expected}" ]]',
        "release version gate accepted mismatching tag",
    ),
    (
        "N0-6 executable release preflight",
        "release pagination removed",
        "scripts/release-preflight.sh",
        "      --paginate \\\n",
        "",
        "existing-release preflight handled stubbed absent response incorrectly",
    ),
    (
        "N0-6 executable release preflight",
        "unsupported gh slurp and jq combination",
        "scripts/release-preflight.sh",
        "      --paginate \\\n",
        "      --paginate \\\n      --slurp \\\n",
        "existing-release preflight handled stubbed absent response incorrectly",
    ),
    (
        "N0-6 executable release preflight",
        "release exact tag filter weakened",
        "scripts/release-preflight.sh",
        ".tag_name == env.TAG",
        ".tag_name | contains(env.TAG)",
        "existing-release preflight handled stubbed absent response incorrectly",
    ),
    (
        "N0-6 executable release preflight",
        "release API error ignored",
        "scripts/release-preflight.sh",
        "      --jq '.[] | select(.tag_name == env.TAG) | .tag_name')\"",
        "      --jq '.[] | select(.tag_name == env.TAG) | .tag_name')\" || true",
        "existing-release preflight handled stubbed api-failure response incorrectly",
    ),
    # N0-7: validated artifact handoff and immutable publication never rebuild or overwrite.
    (
        "N0-7 artifact-only immutable publish",
        "artifact upload path changed",
        ".github/workflows/quality.yml",
        "            dist/checksums.txt",
        "            dist/other.txt",
        "macos-dist upload must contain exactly",
    ),
    (
        "N0-7 artifact-only immutable publish",
        "artifact handoff name disconnected",
        ".github/workflows/release.yml",
        "          name: macos-dist",
        "          name: other-dist",
        "download macos-dist into dist",
    ),
    (
        "N0-7 artifact-only immutable publish",
        "publish rebuild inserted",
        ".github/workflows/release.yml",
        "          gh release create \"${TAG}\" \\\n",
        "          make dist\n          gh release create \"${TAG}\" \\\n",
        "the reusable quality workflow must be the sole make dist owner",
    ),
    (
        "N0-7 artifact-only immutable publish",
        "replacement upload inserted",
        ".github/workflows/release.yml",
        "          gh release create \"${TAG}\" \\\n",
        "          gh release upload \"${TAG}\" dist/checksums.txt\n          gh release create \"${TAG}\" \\\n",
        "release publication must attach dist/checksums.txt exactly once",
    ),
    (
        "N0-7 artifact-only immutable publish",
        "clobber enabled",
        ".github/workflows/release.yml",
        "            --verify-tag",
        "            --verify-tag \\\n            --clobber",
        "must never clobber assets",
    ),
    (
        "N0-7 artifact-only immutable publish",
        "checksum filename validation removed",
        ".github/workflows/release.yml",
        "          actual_checksums=\"$(awk 'NF == 2 && $1 ~ /^[0-9a-f]{64}$/ { print $2 }' dist/checksums.txt | LC_ALL=C sort)\"",
        "          actual_checksums=\"${expected_checksums}\"",
        "artifact validation is missing: awk",
    ),
]

for mutation in MUTATIONS:
    run_mutation(*mutation)

print(f"build contract mutation tests ok: {len(MUTATIONS)} mutations across 7 N0 invariants")
PY
