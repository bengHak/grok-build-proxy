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

if git grep -n -E 'GROK_BUILD_PROXY_BUILD_VERSION|GROK_BUILD_PROXY_VERSION' -- ':!install.sh' ':!README.md' ':!docs/**' ':!scripts/check-build-contract.sh' ':!scripts/test-build-contract.sh'; then
  fail 'build-time version override remains'
fi

grep -Fqx 'pub const VERSION: &str = env!("CARGO_PKG_VERSION");' src/lib.rs \
  || fail 'library runtime version must use CARGO_PKG_VERSION'
grep -Fq 'use grok_build_proxy::' src/main.rs \
  || fail 'CLI must import the shared library version'
grep -Fq 'VERSION,' src/main.rs \
  || fail 'CLI must import the shared library version'
if grep -Eq '^(pub )?const VERSION:' src/main.rs; then
  fail 'main must not define a second runtime version constant'
fi
if grep -Eq '^        version:' src/main.rs; then
  fail 'runtime version must not be injectable through ProxyConfig construction'
fi
if grep -Fq 'pub version:' src/proxy.rs; then
  fail 'ProxyConfig runtime version must not be injectable'
fi
[ "$(grep -Fc 'HeaderValue::from_static(crate::VERSION)' src/proxy.rs)" = "2" ] \
  || fail 'proxy response version headers must use the Cargo-derived runtime version'

make_version=$(make --no-print-directory print-version)
[ "$make_version" = "$package_version" ] \
  || fail "Make version $make_version differs from Cargo version $package_version"

python3 - "$package_version" <<'PY'
from pathlib import Path
import json
import os
import re
import subprocess
import sys
import tempfile

package_version = sys.argv[1]


def fail(message):
    print(f"build contract failed: {message}", file=sys.stderr)
    raise SystemExit(1)


proxy_text = Path("src/proxy.rs").read_text()
user_agent_fragment = '''.header(
            header::USER_AGENT,
            format!("grok-build-proxy/{}", crate::VERSION),
        )'''
if proxy_text.count(user_agent_fragment) != 1:
    fail("Codex User-Agent must use the Cargo-derived runtime version")
user_agent_assertion = '''headers[header::USER_AGENT],
                    format!("grok-build-proxy/{}", crate::VERSION)'''
if proxy_text.count(user_agent_assertion) != 1:
    fail("Codex User-Agent behavior must be tested against the Cargo-derived runtime version")
response_header_assertion = '''response.headers()["x-grok-build-proxy-version"],
            crate::VERSION'''
if proxy_text.count(response_header_assertion) != 1:
    fail("proxy response version behavior must be tested against the Cargo-derived runtime version")


# Parse the Make graph and recipes, then inspect expanded hostile dry-runs.
make_text = Path("Makefile").read_text()
make_lines = make_text.splitlines()
required_assignments = {
    "BINARY": "override BINARY := grok-build-proxy",
    "CARGO": "override CARGO := cargo",
    "DIST_DIR": "override DIST_DIR := dist",
    "MAKE": "override MAKE := make",
}
for variable, assignment in required_assignments.items():
    if make_lines.count(assignment) != 1:
        fail(f"Makefile safety variable {variable} must be a single non-overridable assignment")
package_assignment = "override PACKAGE_VERSION := $(shell cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import json,sys; data=json.load(sys.stdin); print(next(p[\"version\"] for p in data[\"packages\"] if p[\"name\"] == \"grok-build-proxy\"))')"
if make_lines.count(package_assignment) != 1:
    fail("Make package version must be non-overridable and derive directly from locked Cargo metadata")

targets = {}
current = []
for line in make_lines:
    if line.startswith("\t"):
        for target in current:
            targets[target]["recipes"].append(line[1:])
        continue
    current = []
    match = re.match(r"^([^#\s][^:]*):(?!=)(?:\s*(.*))?$", line)
    if not match:
        continue
    names = match.group(1).split()
    dependencies = (match.group(2) or "").split()
    for name in names:
        targets[name] = {"dependencies": dependencies, "recipes": []}
    current = names

required_targets = {
    "build", "build-arm64", "build-amd64", "prepare-dist", "dist", "verify-dist", "clean"
}
missing_targets = sorted(required_targets - targets.keys())
if missing_targets:
    fail(f"Makefile is missing required targets: {', '.join(missing_targets)}")
if targets["dist"]["dependencies"] != ["prepare-dist"]:
    fail("dist must depend only on prepare-dist so cleanup completes before architecture builds")
if targets["prepare-dist"]["dependencies"]:
    fail("prepare-dist must not have prerequisites that could race with cleanup")
if not targets["dist"]["recipes"] or targets["dist"]["recipes"][0] != "$(MAKE) build-arm64 build-amd64":
    fail("dist must synchronously build both architectures as its first recipe command")
if targets["prepare-dist"]["recipes"] != ["rm -rf bin $(DIST_DIR)", "mkdir -p bin $(DIST_DIR)"]:
    fail("prepare-dist must replace only bin and dist packaging output")

ordinary_recipes = "\n".join(
    recipe
    for target in ("build", "build-arm64", "build-amd64", "prepare-dist", "dist")
    for recipe in targets[target]["recipes"]
)
if re.search(r"(?:\$\(CARGO\)|cargo)\s+clean\b", ordinary_recipes):
    fail("ordinary build or dist targets must not run cargo clean")
if re.search(r"\brm\b[^\n]*(?:\btarget\b|\$\([^)]*TARGET[^)]*\))", ordinary_recipes):
    fail("ordinary build or dist targets must not delete Cargo target output")
if not any(re.search(r"(?:\$\(CARGO\)|cargo)\s+clean\b", recipe) for recipe in targets["clean"]["recipes"]):
    fail("the explicit clean target must retain cargo clean")

expected_dist_recipes = {
    "tar -C $(DIST_DIR)/arm64 -czf $(DIST_DIR)/$(BINARY)_Darwin_arm64.tar.gz $(BINARY) LICENSE README.md": "dist must create the arm64 archive with exact membership",
    "tar -C $(DIST_DIR)/amd64 -czf $(DIST_DIR)/$(BINARY)_Darwin_amd64.tar.gz $(BINARY) LICENSE README.md": "dist must create the amd64 archive with exact membership",
    "cd $(DIST_DIR) && shasum -a 256 $(BINARY)_Darwin_arm64.tar.gz $(BINARY)_Darwin_amd64.tar.gz > checksums.txt": "dist must write checksums for exactly the arm64 and amd64 archives once",
}
expected_verify_recipes = {
    "@test \"$$(wc -l < $(DIST_DIR)/checksums.txt | tr -d ' ')\" = \"2\"": "verify-dist must require exactly two checksum lines",
    "cd $(DIST_DIR) && shasum -a 256 -c checksums.txt": "verify-dist must verify archive checksums",
    "@test \"$$(awk 'NF == 2 && $$1 ~ /^[0-9a-f]{64}$$/ { print $$2 }' $(DIST_DIR)/checksums.txt)\" = \"$$(printf '%s\\n' $(BINARY)_Darwin_arm64.tar.gz $(BINARY)_Darwin_amd64.tar.gz)\"": "verify-dist must require exactly one checksum entry for each expected archive",
    "@test \"$$(tar -tzf $(DIST_DIR)/$(BINARY)_Darwin_arm64.tar.gz | LC_ALL=C sort)\" = \"$$(printf '%s\\n' $(BINARY) LICENSE README.md | LC_ALL=C sort)\"": "verify-dist must require exact arm64 archive membership",
    "@test \"$$(tar -tzf $(DIST_DIR)/$(BINARY)_Darwin_amd64.tar.gz | LC_ALL=C sort)\" = \"$$(printf '%s\\n' $(BINARY) LICENSE README.md | LC_ALL=C sort)\"": "verify-dist must require exact amd64 archive membership",
}
for recipe, message in expected_dist_recipes.items():
    if targets["dist"]["recipes"].count(recipe) != 1:
        fail(message)
for recipe, message in expected_verify_recipes.items():
    if targets["verify-dist"]["recipes"].count(recipe) != 1:
        fail(message)

hostile_overrides = [
    "DIST_DIR=target",
    "CARGO=cargo clean; cargo",
    "PACKAGE_VERSION=hostile-version",
    "BINARY=target",
    "MAKE=rm -rf target",
]
try:
    dry_run = subprocess.run(
        ["make", "--no-print-directory", "-n", "-j2", "dist", *hostile_overrides],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.splitlines()
except subprocess.CalledProcessError as error:
    sys.stderr.write(error.stdout + error.stderr)
    fail("make hostile-override dist dry-run failed")
try:
    cleanup_index = dry_run.index("rm -rf bin dist")
    prepare_index = dry_run.index("mkdir -p bin dist")
    recursive_build_index = next(
        index for index, command in enumerate(dry_run)
        if re.search(r"(?:^|/)make build-arm64 build-amd64$", command)
    )
    packaging_index = dry_run.index("mkdir -p dist/arm64 dist/amd64")
except (ValueError, StopIteration):
    fail("dist hostile-override dry-run is missing safe ordered commands")
build_indexes = [index for index, command in enumerate(dry_run) if re.search(r"^cargo build\b", command)]
if len(build_indexes) != 2:
    fail("dist dry-run must contain exactly two Cargo architecture builds")
if not cleanup_index < prepare_index < recursive_build_index < min(build_indexes) <= max(build_indexes) < packaging_index:
    fail("dist dry-run must complete cleanup before architecture builds and package afterward")
if any("--locked" not in dry_run[index].split() for index in build_indexes):
    fail("dist Cargo architecture builds must use --locked")
expanded = "\n".join(dry_run)
if re.search(r"\bcargo clean\b|\brm\b[^\n]*\btarget(?:/|\b)|hostile-version", expanded):
    fail("hostile Make overrides must not execute cleanup, delete target, or replace package version")
hostile_version = subprocess.run(
    ["make", "--no-print-directory", "print-version", *hostile_overrides],
    check=True,
    capture_output=True,
    text=True,
).stdout.strip()
if hostile_version != package_version:
    fail("hostile Make overrides changed the Cargo-derived package version")

# Every dependency-resolving Cargo command in tracked build surfaces must be locked.
tracked = subprocess.run(["git", "ls-files"], check=True, capture_output=True, text=True).stdout.splitlines()
build_surfaces = []
for filename in tracked:
    path = Path(filename)
    if filename in {"scripts/check-build-contract.sh", "scripts/test-build-contract.sh"}:
        continue
    if filename in {"Makefile", "install.sh"} or filename.startswith(".github/workflows/") or (filename.startswith("scripts/") and path.suffix == ".sh"):
        build_surfaces.append(path)
cargo_pattern = re.compile(r"(?:\bcargo|\$\(CARGO\))\s+(metadata|build|check|clippy|test|run|bench)\b([^\n]*)")
for path in build_surfaces:
    logical_text = path.read_text().replace("\\\n", " ")
    for line_number, line in enumerate(logical_text.splitlines(), 1):
        if line.lstrip().startswith("#"):
            continue
        code = line.split("#", 1)[0]
        for match in cargo_pattern.finditer(code):
            if "--locked" not in match.group(2).split():
                fail(f"{path}:{line_number} cargo {match.group(1)} must use --locked")

# Parse every workflow with Psych, rejecting malformed YAML and duplicate keys before semantics.
workflow_paths = sorted(Path(".github/workflows").glob("*.y*ml"))
if not workflow_paths:
    fail("no GitHub Actions workflows found")
ruby_parser = r'''
require "yaml"
require "json"

def check_node(node, path)
  case node
  when Psych::Nodes::Mapping
    seen = {}
    node.children.each_slice(2) do |key_node, value_node|
      raise "#{path}: mapping key is not a scalar" unless key_node.is_a?(Psych::Nodes::Scalar)
      key = key_node.value
      raise "#{path}: duplicate key #{key.inspect}" if seen.key?(key)
      seen[key] = true
      check_node(value_node, path)
    end
  when Psych::Nodes::Sequence, Psych::Nodes::Document, Psych::Nodes::Stream
    node.children.each { |child| check_node(child, path) }
  end
end

ARGV.each do |path|
  syntax = Psych.parse_file(path)
  check_node(syntax, path)
  yaml = File.read(path)
  parameters = Psych.method(:safe_load).parameters
  if parameters.any? { |kind, name| kind == :key && name == :aliases }
    data = YAML.safe_load(
      yaml,
      permitted_classes: [],
      permitted_symbols: [],
      aliases: false,
      filename: path
    )
  else
    data = YAML.safe_load(yaml, [], [], false, path)
  end
  raise "#{path}: workflow root must be a mapping" unless data.is_a?(Hash)
  data["on"] = data.delete(true) if data.key?(true)
  puts JSON.generate([path, data])
end
'''
parsed = subprocess.run(
    ["ruby", "-ryaml", "-rjson", "-e", ruby_parser, *map(str, workflow_paths)],
    capture_output=True,
    text=True,
)
if parsed.returncode != 0:
    fail(f"workflow YAML validation failed:\n{parsed.stderr.strip()}")
workflows = {}
for line in parsed.stdout.splitlines():
    path, document = json.loads(line)
    workflows[Path(path).name] = document

allowed_root_keys = {"name", "run-name", "on", "permissions", "env", "defaults", "concurrency", "jobs"}
allowed_job_keys = {
    "name", "permissions", "needs", "if", "uses", "with", "secrets", "strategy",
    "continue-on-error", "container", "services", "runs-on", "environment", "concurrency",
    "outputs", "env", "defaults", "steps", "timeout-minutes",
}
allowed_step_keys = {"id", "if", "name", "uses", "run", "working-directory", "shell", "with", "env", "continue-on-error", "timeout-minutes"}
for name, workflow in workflows.items():
    unknown_root = set(workflow) - allowed_root_keys
    if unknown_root:
        fail(f"{name} has unknown root keys: {', '.join(sorted(unknown_root))}")
    jobs = workflow.get("jobs")
    if not isinstance(jobs, dict):
        fail(f"{name} jobs must be a mapping")
    for job_name, job in jobs.items():
        if not isinstance(job, dict):
            fail(f"{name} job {job_name} must be a mapping")
        unknown_job = set(job) - allowed_job_keys
        if unknown_job:
            fail(f"{name} job {job_name} has unknown keys: {', '.join(sorted(unknown_job))}")
        steps = job.get("steps", [])
        if not isinstance(steps, list):
            fail(f"{name} job {job_name} steps must be a sequence")
        for index, step in enumerate(steps, 1):
            if not isinstance(step, dict):
                fail(f"{name} job {job_name} step {index} must be a mapping")
            unknown_step = set(step) - allowed_step_keys
            if unknown_step:
                fail(f"{name} job {job_name} step {index} has unknown keys: {', '.join(sorted(unknown_step))}")
            if ("run" in step) == ("uses" in step):
                fail(f"{name} job {job_name} step {index} must declare exactly one of run or uses")

required_workflows = {"ci.yml", "quality.yml", "release.yml"}
missing_workflows = sorted(required_workflows - workflows.keys())
if missing_workflows:
    fail(f"missing required workflows: {', '.join(missing_workflows)}")
if set(workflows) != required_workflows:
    fail("only ci.yml, quality.yml, and release.yml may define build/release workflows")

all_steps = []
for workflow_name, workflow in workflows.items():
    for job_name, job in workflow["jobs"].items():
        for step in job.get("steps", []):
            all_steps.append((workflow_name, job_name, step))
            uses = step.get("uses")
            if uses and not uses.startswith("./") and not re.fullmatch(r"[^@\s]+@[0-9a-f]{40}", uses):
                fail(f"{workflow_name} job {job_name} remote uses must be pinned to a full commit SHA: {uses}")

quality = workflows["quality.yml"]
if set(quality.get("on", {})) != {"workflow_call"}:
    fail("quality workflow must trigger only through workflow_call")
workflow_call = quality["on"]["workflow_call"]
if not isinstance(workflow_call, dict) or set(workflow_call) != {"inputs"}:
    fail("quality workflow_call must declare only the upload_dist input")
inputs = workflow_call["inputs"]
if not isinstance(inputs, dict) or set(inputs) != {"upload_dist"}:
    fail("quality workflow_call must expose only the upload_dist input")
upload_dist = inputs["upload_dist"]
if not isinstance(upload_dist, dict) or set(upload_dist) != {"description", "required", "default", "type"}:
    fail("quality upload_dist must declare description, required, default, and type exactly once")
if upload_dist["required"] is not False:
    fail("quality upload_dist input must not be required")
if upload_dist["default"] is not False:
    fail("quality upload_dist input must default to false")
if upload_dist["type"] != "boolean":
    fail("quality upload_dist input must be boolean")
if list(quality["jobs"]) != ["quality"]:
    fail("quality workflow must contain exactly the quality job")
quality_job = quality["jobs"]["quality"]
if quality_job.get("runs-on") != "macos-14":
    fail("quality job must run on macos-14")
quality_steps = quality_job.get("steps", [])
expected_quality_actions = ("actions/checkout@", "dtolnay/rust-toolchain@", "Swatinem/rust-cache@")
if len(quality_steps) != 11 or any(not quality_steps[index].get("uses", "").startswith(prefix) for index, prefix in enumerate(expected_quality_actions)):
    fail("quality workflow must begin with checkout, Rust toolchain, and Rust cache actions")
rust_step = quality_steps[1]
if rust_step.get("with") != {
    "toolchain": "1.88.0",
    "components": "rustfmt, clippy",
    "targets": "aarch64-apple-darwin, x86_64-apple-darwin",
}:
    fail("quality Rust toolchain must be exactly 1.88.0 with rustfmt, clippy, and both macOS targets")
quality_by_name = {step.get("name"): step for step in quality_steps if step.get("name")}
expected_quality_commands = {
    "Check build contract": "./scripts/check-build-contract.sh",
    "Test build contract mutations": "./scripts/test-build-contract.sh",
    "Check formatting": "cargo fmt --check",
    "Clippy": "cargo clippy --locked --all-targets --all-features -- -D warnings",
    "Test": "cargo test --locked --all-targets",
    "Validate installer": "sh -n install.sh",
    "Build and validate Apple Silicon and Intel archives": "make dist",
}
if list(quality_by_name) != [
    "Check build contract", "Test build contract mutations", "Check formatting", "Clippy", "Test",
    "Validate installer", "Build and validate Apple Silicon and Intel archives", "Upload validated release archives",
]:
    fail("quality workflow validation and archive steps changed unexpectedly")
for name, command in expected_quality_commands.items():
    if quality_by_name.get(name, {}).get("run") != command:
        fail(f"quality workflow {name} command must be exactly: {command}")
upload_step = quality_by_name["Upload validated release archives"]
if upload_step.get("if") != "inputs.upload_dist":
    fail("quality artifact upload must be conditional on inputs.upload_dist")
if not upload_step.get("uses", "").startswith("actions/upload-artifact@"):
    fail("quality workflow must use the pinned upload-artifact action")
expected_artifact_paths = [
    "dist/grok-build-proxy_Darwin_arm64.tar.gz",
    "dist/grok-build-proxy_Darwin_amd64.tar.gz",
    "dist/checksums.txt",
]
if upload_step.get("with") != {
    "name": "macos-dist",
    "path": "\n".join(expected_artifact_paths) + "\n",
    "if-no-files-found": "error",
    "retention-days": 1,
}:
    fail("macos-dist upload must contain exactly the validated archives and checksums")

ci = workflows["ci.yml"]
if ci.get("on") != {"push": {"branches": ["main"]}, "pull_request": None}:
    fail("CI must trigger only on main pushes and pull requests")
if list(ci["jobs"]) != ["quality"]:
    fail("CI must contain exactly one reusable quality job")
if ci["jobs"]["quality"] != {"uses": "./.github/workflows/quality.yml"}:
    fail("CI quality job must only call the local reusable quality workflow")

release = workflows["release.yml"]
if release.get("on") != {"push": {"tags": ["v*"]}}:
    fail("release must trigger only on v* tag pushes")
if list(release["jobs"]) != ["preflight", "quality", "publish"]:
    fail("release must contain exactly preflight, quality, and publish jobs in order")
preflight_job = release["jobs"]["preflight"]
if preflight_job.get("runs-on") != "macos-14":
    fail("release preflight must run on macos-14")
if preflight_job.get("permissions") != {"contents": "write"}:
    fail("release preflight requires contents: write to list draft releases")
if preflight_job.get("outputs") != {"tag": "${{ steps.version.outputs.tag }}"}:
    fail("release preflight must expose the verified version tag output")
preflight_steps = preflight_job.get("steps", [])
if len(preflight_steps) != 4:
    fail("release preflight must contain only checkout, Rust setup, version verification, and release rejection")
if not preflight_steps[0].get("uses", "").startswith("actions/checkout@"):
    fail("release preflight must begin with checkout")
if not preflight_steps[1].get("uses", "").startswith("dtolnay/rust-toolchain@") or preflight_steps[1].get("with") != {"toolchain": "1.88.0"}:
    fail("release preflight Rust toolchain must be exactly 1.88.0")
version_step = preflight_steps[2]
if version_step != {
    "name": "Verify tag and Cargo package version",
    "id": "version",
    "shell": "bash",
    "run": "./scripts/release-preflight.sh verify-version",
}:
    fail("release version verification must execute the tested preflight version gate")
release_gate = preflight_steps[3]
if release_gate != {
    "name": "Reject an existing release",
    "env": {
        "GH_TOKEN": "${{ github.token }}",
        "GH_REPO": "${{ github.repository }}",
        "TAG": "${{ steps.version.outputs.tag }}",
    },
    "shell": "bash",
    "run": "./scripts/release-preflight.sh reject-existing-release",
}:
    fail("existing-release gate must execute the tested draft-aware API preflight")
quality_call = release["jobs"]["quality"]
if quality_call != {
    "needs": "preflight",
    "permissions": {"contents": "read"},
    "uses": "./.github/workflows/quality.yml",
    "with": {"upload_dist": True},
}:
    fail("release quality job must depend on preflight and request validated artifacts")
publish_job = release["jobs"]["publish"]
if publish_job.get("needs") != ["preflight", "quality"]:
    fail("release publish job must depend on preflight and quality")
if publish_job.get("runs-on") != "macos-14" or publish_job.get("permissions") != {"contents": "write"}:
    fail("release publish job must run on macos-14 with contents: write")
publish_steps = publish_job.get("steps", [])
if len(publish_steps) != 3 or not publish_steps[0].get("uses", "").startswith("actions/download-artifact@"):
    fail("release publish job must begin by downloading the quality artifact")
if publish_steps[0].get("with") != {"name": "macos-dist", "path": "dist"}:
    fail("release publish job must download macos-dist into dist")
if any("actions/checkout@" in step.get("uses", "") for step in publish_steps):
    fail("artifact-only publish job must not checkout or rebuild source")
validation_step = publish_steps[1]
if validation_step.get("name") != "Validate downloaded release archives":
    fail("release publish job must validate downloaded artifacts before publication")
validation_commands = validation_step.get("run", "")
for asset in expected_artifact_paths:
    if asset not in validation_commands:
        fail(f"release artifact validation is missing {asset}")
for required_fragment in (
    "find dist -type f",
    "awk 'NF == 2 && $1 ~ /^[0-9a-f]{64}$/ { print $2 }' dist/checksums.txt",
    "wc -l < dist/checksums.txt",
    "(cd dist && shasum -a 256 -c checksums.txt)",
):
    if required_fragment not in validation_commands:
        fail(f"release artifact validation is missing: {required_fragment}")
publish_step = publish_steps[2]
if publish_step.get("name") != "Publish immutable GitHub release" or publish_step.get("env") != {
    "GH_TOKEN": "${{ github.token }}",
    "GH_REPO": "${{ github.repository }}",
    "TAG": "${{ needs.preflight.outputs.tag }}",
}:
    fail("release publication requires explicit token, repository, and preflight tag context")
publish_commands = publish_step.get("run", "")
if 'gh release create "${TAG}"' not in publish_commands:
    fail("release publication must create the preflight-approved tag release")
if publish_commands.split().count("--verify-tag") != 1:
    fail("release publication must verify the tag exactly once")
for asset in expected_artifact_paths:
    if publish_commands.count(asset) != 1:
        fail(f"release publication must attach {asset} exactly once")

all_run_commands = [step.get("run", "") for _, _, step in all_steps if "run" in step]
if sum(command.splitlines().count("make dist") for command in all_run_commands) != 1:
    fail("the reusable quality workflow must be the sole make dist owner")
release_commands = "\n".join(step.get("run", "") for job in release["jobs"].values() for step in job.get("steps", []))
if re.search(r"(?:^|\n)\s*(?:make\s+dist|cargo\s+(?:build|test|clippy)\b|gh\s+release\s+upload\b)", release_commands):
    fail("release preflight and publish jobs must not rebuild or upload replacement assets")
if re.search(r"(?:^|\s)--clobber(?:\s|$)", release_commands):
    fail("release publication must never clobber assets")

# Execute the release preflight with controlled environments and a stubbed gh.
preflight_script = Path("scripts/release-preflight.sh")
if not preflight_script.is_file() or not os.access(preflight_script, os.X_OK):
    fail("release preflight script must exist and be executable")
with tempfile.TemporaryDirectory(prefix="release-preflight-contract-") as temp_dir:
    temp = Path(temp_dir)
    output = temp / "github-output"
    base_env = os.environ.copy()
    base_env.update({
        "GITHUB_REF_TYPE": "tag",
        "GITHUB_REF_NAME": f"v{package_version}",
        "GITHUB_OUTPUT": str(output),
    })
    result = subprocess.run([str(preflight_script), "verify-version"], env=base_env, capture_output=True, text=True)
    if result.returncode != 0 or output.read_text() != f"tag=v{package_version}\n":
        fail(f"release version gate rejected the exact Cargo tag:\n{result.stdout}{result.stderr}")
    for name, overrides in (
        ("mismatching tag", {"GITHUB_REF_NAME": "v999.999.999"}),
        ("branch ref", {"GITHUB_REF_TYPE": "branch"}),
        ("missing ref", {"GITHUB_REF_NAME": ""}),
    ):
        output.write_text("")
        environment = base_env | overrides
        result = subprocess.run([str(preflight_script), "verify-version"], env=environment, capture_output=True, text=True)
        if result.returncode == 0 or output.read_text():
            fail(f"release version gate accepted {name}")

    stub_dir = temp / "bin"
    stub_dir.mkdir()
    gh_stub = stub_dir / "gh"
    gh_stub.write_text(r'''#!/bin/bash
set -euo pipefail
[[ "${1:-}" == "api" ]] || exit 64
shift
paginate=0
method=""
endpoint=""
query=""
while (($#)); do
  case "$1" in
    --method) method="$2"; shift 2 ;;
    --paginate) paginate=1; shift ;;
    --slurp) echo "unsupported --slurp" >&2; exit 64 ;;
    -H) shift 2 ;;
    --jq) query="$2"; shift 2 ;;
    /repos/*) endpoint="$1"; shift ;;
    *) echo "unexpected gh argument: $1" >&2; exit 64 ;;
  esac
done
[[ "$method" == "GET" && "$paginate" == "1" ]] || exit 64
[[ "$endpoint" == "/repos/${GH_REPO}/releases?per_page=100" ]] || exit 64
[[ "$query" == '.[] | select(.tag_name == env.TAG) | .tag_name' ]] || exit 64
case "${GH_STUB_MODE:-}" in
  absent) ;;
  draft|published|page-two) printf '%s\n' "$TAG" ;;
  api-failure) exit 42 ;;
  *) exit 64 ;;
esac
''')
    gh_stub.chmod(0o755)
    gh_env = os.environ.copy()
    gh_env.update({
        "PATH": f"{stub_dir}{os.pathsep}{gh_env['PATH']}",
        "GH_TOKEN": "stub-token",
        "GH_REPO": "owner/repo",
        "TAG": f"v{package_version}",
    })
    for mode, should_succeed in (
        ("absent", True),
        ("draft", False),
        ("published", False),
        ("page-two", False),
        ("api-failure", False),
    ):
        environment = gh_env | {"GH_STUB_MODE": mode}
        result = subprocess.run([str(preflight_script), "reject-existing-release"], env=environment, capture_output=True, text=True)
        if (result.returncode == 0) != should_succeed:
            fail(f"existing-release preflight handled stubbed {mode} response incorrectly:\n{result.stdout}{result.stderr}")

print(f"build contract ok: version {package_version}")
PY
