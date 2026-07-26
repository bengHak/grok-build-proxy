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

grep -Fqx 'const VERSION: &str = env!("CARGO_PKG_VERSION");' src/main.rs \
  || fail 'CLI must use CARGO_PKG_VERSION'

make_version=$(make --no-print-directory print-version)
[ "$make_version" = "$package_version" ] \
  || fail "Make version $make_version differs from Cargo version $package_version"

python3 <<'PY'
from pathlib import Path
import json
import re
import subprocess
import sys


def fail(message):
    print(f"build contract failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def uncommented(value):
    return re.sub(r"\s+#.*$", "", value).strip()


def indentation(line):
    return len(line) - len(line.lstrip(" "))


# Parse the Make graph and recipes rather than accepting matching text in comments.
make_lines = Path("Makefile").read_text().splitlines()
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
    "build",
    "build-arm64",
    "build-amd64",
    "prepare-dist",
    "dist",
    "verify-dist",
    "clean",
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

checksum_recipe = "cd $(DIST_DIR) && shasum -a 256 $(BINARY)_Darwin_arm64.tar.gz $(BINARY)_Darwin_amd64.tar.gz > checksums.txt"
if targets["dist"]["recipes"].count(checksum_recipe) != 1:
    fail("dist must write checksums for exactly the arm64 and amd64 archives once")
checksum_line_count_recipe = "@test \"$$(wc -l < $(DIST_DIR)/checksums.txt | tr -d ' ')\" = \"2\""
if targets["verify-dist"]["recipes"].count(checksum_line_count_recipe) != 1:
    fail("verify-dist must require exactly two checksum lines")
checksum_coverage_recipe = "@test \"$$(awk 'NF == 2 && $$1 ~ /^[0-9a-f]{64}$$/ { print $$2 }' $(DIST_DIR)/checksums.txt)\" = \"$$(printf '%s\\n' $(BINARY)_Darwin_arm64.tar.gz $(BINARY)_Darwin_amd64.tar.gz)\""
if targets["verify-dist"]["recipes"].count(checksum_coverage_recipe) != 1:
    fail("verify-dist must require exactly one checksum entry for each expected archive")

try:
    dry_run = subprocess.run(
        ["make", "--no-print-directory", "-n", "-j2", "dist"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.splitlines()
except subprocess.CalledProcessError as error:
    sys.stderr.write(error.stderr)
    fail("make -n -j2 dist failed")

try:
    cleanup_index = dry_run.index("rm -rf bin dist")
    prepare_index = dry_run.index("mkdir -p bin dist")
    recursive_build_index = next(
        index
        for index, command in enumerate(dry_run)
        if re.search(r"(?:^|/)make build-arm64 build-amd64$", command)
    )
    packaging_index = dry_run.index("mkdir -p dist/arm64 dist/amd64")
except (ValueError, StopIteration):
    fail("dist dry-run is missing ordered cleanup, architecture build, or packaging commands")
build_indexes = [
    index
    for index, command in enumerate(dry_run)
    if re.search(r"^cargo build\b", command)
]
if len(build_indexes) != 2:
    fail("dist dry-run must contain exactly two Cargo architecture builds")
if not cleanup_index < prepare_index < recursive_build_index < min(build_indexes) <= max(build_indexes) < packaging_index:
    fail("dist dry-run must complete cleanup before architecture builds and package afterward")
for index in build_indexes:
    if "--locked" not in dry_run[index].split():
        fail(f"dist Cargo build is not locked: {dry_run[index]}")
if any(re.search(r"\bcargo clean\b|\brm\b.*\btarget(?:/|\b)", command) for command in dry_run):
    fail("dist dry-run deletes Cargo target output")


# Every dependency-resolving Cargo command in tracked build surfaces must be locked.
tracked = subprocess.run(
    ["git", "ls-files"], check=True, capture_output=True, text=True
).stdout.splitlines()
build_surfaces = []
for filename in tracked:
    path = Path(filename)
    if filename == "scripts/check-build-contract.sh":
        continue
    if filename == "Makefile" or filename == "install.sh" or filename.startswith(".github/workflows/") or (filename.startswith("scripts/") and path.suffix == ".sh"):
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


# Parse workflow YAML structure without depending on third-party Python packages.
def yaml_scalar(value):
    value = value.strip()
    if not value or value[0] not in "\"'":
        return value
    if value[0] == '"':
        try:
            return json.loads(value)
        except json.JSONDecodeError:
            return value
    if len(value) >= 2 and value[-1] == "'":
        return value[1:-1].replace("''", "'")
    return value


def workflow_lines(path):
    return Path(path).read_text().splitlines()


def workflow_uses(path, lines):
    found = []
    scalar_indent = None
    for line_number, raw in enumerate(lines, 1):
        if not raw.strip():
            continue
        indent = indentation(raw)
        if scalar_indent is not None:
            if indent > scalar_indent:
                continue
            scalar_indent = None
        code = raw.lstrip()
        if code.startswith("#"):
            continue
        if re.match(r"^(?:-\s*)?[^:]+:\s*[|>][-+]?\s*(?:#.*)?$", code):
            scalar_indent = indent
        match = re.match(r"^(?:-\s*)?uses:\s*(.+?)\s*$", code)
        if match:
            found.append((line_number, yaml_scalar(uncommented(match.group(1)))))
    return found


def job_blocks(path, lines):
    try:
        jobs_index = next(index for index, line in enumerate(lines) if re.match(r"^jobs:\s*(?:#.*)?$", line))
    except StopIteration:
        fail(f"{path} has no jobs mapping")
    blocks = {}
    order = []
    current_name = None
    current_lines = []
    for raw in lines[jobs_index + 1 :]:
        if raw.strip() and indentation(raw) == 0:
            break
        match = re.match(r"^  ([A-Za-z0-9_-]+):\s*(?:#.*)?$", raw)
        if match:
            if current_name is not None:
                blocks[current_name] = current_lines
            current_name = match.group(1)
            order.append(current_name)
            current_lines = []
        elif current_name is not None:
            current_lines.append(raw)
    if current_name is not None:
        blocks[current_name] = current_lines
    return order, blocks


def direct_value(lines, indent, key):
    pattern = re.compile(rf"^ {{{indent}}}{re.escape(key)}:\s*(.*)$")
    values = []
    for raw in lines:
        match = pattern.match(raw)
        if match:
            values.append(yaml_scalar(uncommented(match.group(1))))
    if len(values) > 1:
        fail(f"duplicate {key} keys in workflow structure")
    return values[0] if values else None


def nested_mapping(lines, parent_indent, key):
    start = None
    for index, raw in enumerate(lines):
        if re.match(rf"^ {{{parent_indent}}}{re.escape(key)}:\s*(?:#.*)?$", raw):
            start = index + 1
            break
    if start is None:
        return {}
    result = {}
    child_indent = parent_indent + 2
    index = start
    while index < len(lines):
        raw = lines[index]
        if raw.strip() and indentation(raw) <= parent_indent:
            break
        match = re.match(rf"^ {{{child_indent}}}([^:#]+):\s*(.*)$", raw)
        if match:
            child_key = match.group(1).strip()
            value = yaml_scalar(uncommented(match.group(2)))
            if value in ("|", ">", "|-", ">-"):
                content = []
                index += 1
                while index < len(lines):
                    content_line = lines[index]
                    if content_line.strip() and indentation(content_line) <= child_indent:
                        index -= 1
                        break
                    content.append(content_line[child_indent + 2 :] if len(content_line) >= child_indent + 2 else "")
                    index += 1
                value = "\n".join(content).rstrip()
            result[child_key] = value
        index += 1
    return result


def parse_steps(job_lines):
    try:
        start = next(index for index, raw in enumerate(job_lines) if re.match(r"^    steps:\s*(?:#.*)?$", raw)) + 1
    except StopIteration:
        return []
    raw_steps = []
    current = None
    for raw in job_lines[start:]:
        if raw.strip() and indentation(raw) <= 4:
            break
        match = re.match(r"^      -\s+(.*)$", raw)
        if match:
            if current is not None:
                raw_steps.append(current)
            current = ["        " + match.group(1)]
        elif current is not None:
            current.append(raw)
    if current is not None:
        raw_steps.append(current)

    steps = []
    for raw_step in raw_steps:
        step = {}
        for key in ("name", "uses", "if", "shell"):
            value = direct_value(raw_step, 8, key)
            if value is not None:
                step[key] = value
        step["env"] = nested_mapping(raw_step, 8, "env")
        step["with"] = nested_mapping(raw_step, 8, "with")
        run_value = direct_value(raw_step, 8, "run")
        if run_value in ("|", ">", "|-", ">-"):
            run_start = next(index for index, raw in enumerate(raw_step) if re.match(r"^        run:\s*[|>]", raw)) + 1
            run_lines = []
            for raw in raw_step[run_start:]:
                if raw.strip() and indentation(raw) <= 8:
                    break
                run_lines.append(raw[10:] if len(raw) >= 10 else "")
            run_value = "\n".join(run_lines).rstrip()
        step["run"] = run_value
        steps.append(step)
    return steps


def command_text(step):
    return "\n".join(
        line for line in (step.get("run") or "").splitlines() if not line.lstrip().startswith("#")
    )


workflow_paths = sorted(Path(".github/workflows").glob("*.y*ml"))
if not workflow_paths:
    fail("no GitHub Actions workflows found")
workflows = {}
all_uses = []
for path in workflow_paths:
    lines = workflow_lines(path)
    workflows[path.name] = (lines, *job_blocks(path, lines))
    all_uses.extend((path, line_number, value) for line_number, value in workflow_uses(path, lines))

for path, line_number, value in all_uses:
    if value.startswith("./"):
        continue
    if not re.fullmatch(r"[^@\s]+@[0-9a-f]{40}", value):
        fail(f"{path}:{line_number} remote uses must be pinned to a full commit SHA: {value}")

required_workflows = {"ci.yml", "quality.yml", "release.yml"}
missing_workflows = sorted(required_workflows - workflows.keys())
if missing_workflows:
    fail(f"missing required workflows: {', '.join(missing_workflows)}")

# The reusable quality workflow is the sole owner of validation and dist creation.
quality_lines, quality_order, quality_jobs = workflows["quality.yml"]
quality_code = "\n".join(line for line in quality_lines if not line.lstrip().startswith("#"))
if not re.search(r"^  workflow_call:\s*$", quality_code, re.MULTILINE):
    fail("quality workflow must support workflow_call")
if quality_order != ["quality"]:
    fail("quality workflow must contain exactly the quality job")
quality_steps = parse_steps(quality_jobs["quality"])
expected_quality_actions = (
    "actions/checkout@",
    "dtolnay/rust-toolchain@",
    "Swatinem/rust-cache@",
)
if len(quality_steps) != 10 or any(
    not quality_steps[index].get("uses", "").startswith(prefix)
    for index, prefix in enumerate(expected_quality_actions)
):
    fail("quality workflow must begin with checkout, Rust toolchain, and Rust cache actions")
quality_step_names = [step.get("name") for step in quality_steps if step.get("name")]
expected_quality_names = [
    "Check build contract",
    "Check formatting",
    "Clippy",
    "Test",
    "Validate installer",
    "Build and validate Apple Silicon and Intel archives",
    "Upload validated release archives",
]
if quality_step_names != expected_quality_names:
    fail("quality workflow validation and archive steps changed unexpectedly")
quality_commands = {step.get("name"): command_text(step) for step in quality_steps}
expected_quality_commands = {
    "Check build contract": "./scripts/check-build-contract.sh",
    "Check formatting": "cargo fmt --check",
    "Clippy": "cargo clippy --locked --all-targets --all-features -- -D warnings",
    "Test": "cargo test --locked --all-targets",
    "Validate installer": "sh -n install.sh",
    "Build and validate Apple Silicon and Intel archives": "make dist",
}
for name, expected in expected_quality_commands.items():
    if quality_commands.get(name) != expected:
        fail(f"quality workflow {name} command must be exactly: {expected}")
upload_step = next(step for step in quality_steps if step.get("name") == "Upload validated release archives")
if upload_step.get("if") != "inputs.upload_dist":
    fail("quality artifact upload must be conditional on inputs.upload_dist")
if upload_step.get("with", {}).get("name") != "macos-dist":
    fail("quality workflow must upload the macos-dist artifact")
expected_artifact_paths = [
    "dist/grok-build-proxy_Darwin_arm64.tar.gz",
    "dist/grok-build-proxy_Darwin_amd64.tar.gz",
    "dist/checksums.txt",
]
actual_artifact_paths = upload_step.get("with", {}).get("path", "").splitlines()
if actual_artifact_paths != expected_artifact_paths or len(actual_artifact_paths) != len(set(actual_artifact_paths)):
    fail("macos-dist must contain each expected archive and checksums.txt exactly once")
if upload_step.get("with", {}).get("if-no-files-found") != "error":
    fail("quality artifact upload must fail when expected files are absent")

ci_lines, ci_order, ci_jobs = workflows["ci.yml"]
if ci_order != ["quality"]:
    fail("CI must contain exactly one reusable quality job")
if direct_value(ci_jobs["quality"], 4, "uses") != "./.github/workflows/quality.yml":
    fail("CI quality job must call the local reusable quality workflow")
if parse_steps(ci_jobs["quality"]):
    fail("CI must not duplicate reusable quality steps")
if direct_value(ci_jobs["quality"], 4, "runs-on") is not None:
    fail("CI quality must remain a reusable-workflow caller without its own runner")

# Release must be tag-only and have the exact preflight -> quality -> publish graph.
release_lines, release_order, release_jobs = workflows["release.yml"]
release_structural_code = "\n".join(line for line in release_lines if not line.lstrip().startswith("#"))
if not re.search(r'^    tags:\s*\n      - ["\']v\*["\']\s*$', release_structural_code, re.MULTILINE):
    fail("release must trigger only for v* tags")
if re.search(r"^\s+(?:workflow_dispatch|branches):", release_structural_code, re.MULTILINE):
    fail("release must not allow manual or branch triggers")
if release_order != ["preflight", "quality", "publish"]:
    fail("release must contain exactly preflight, quality, and publish jobs in order")
if direct_value(release_jobs["quality"], 4, "needs") != "preflight":
    fail("release quality job must depend on preflight")
if direct_value(release_jobs["quality"], 4, "uses") != "./.github/workflows/quality.yml":
    fail("release quality job must call the local reusable quality workflow")
if nested_mapping(release_jobs["quality"], 4, "with").get("upload_dist") != "true":
    fail("release quality job must request validated distribution artifacts")
if parse_steps(release_jobs["quality"]) or direct_value(release_jobs["quality"], 4, "runs-on") is not None:
    fail("release quality must remain a reusable-workflow caller without local steps")
if direct_value(release_jobs["publish"], 4, "needs") != "[preflight, quality]":
    fail("release publish job must depend on preflight and quality")
if nested_mapping(release_jobs["publish"], 4, "permissions").get("contents") != "write":
    fail("release publish job requires contents: write")

preflight_steps = parse_steps(release_jobs["preflight"])
preflight_names = [step.get("name") for step in preflight_steps if step.get("name")]
if preflight_names != ["Verify tag and Cargo package version", "Reject an existing release"]:
    fail("release preflight must verify the version and reject existing releases")
release_gate = next(step for step in preflight_steps if step.get("name") == "Reject an existing release")
required_gate_env = {
    "GH_TOKEN": "${{ github.token }}",
    "GH_REPO": "${{ github.repository }}",
    "TAG": "${{ steps.version.outputs.tag }}",
}
if release_gate.get("env") != required_gate_env:
    fail("existing-release gate requires explicit token, repository, and tag context")
gate_commands = command_text(release_gate)
for required_fragment in (
    "set -euo pipefail",
    'release_id="$(gh api',
    '"/repos/${GH_REPO}/releases/tags/${TAG}"',
    "--jq '.id' 2>release-query-error.log",
    'if [[ "${query_status}" == "1" ]] && grep -Fq \'HTTP 404\' release-query-error.log; then',
    'cat release-query-error.log >&2',
    'exit "${query_status}"',
    'if [[ -n "${release_id}" ]]; then',
):
    if required_fragment not in gate_commands:
        fail(f"existing-release gate is missing authoritative API logic: {required_fragment}")
if re.search(r"gh\s+release\s+view|>/dev/null|2>&1", gate_commands):
    fail("existing-release gate must not hide or reinterpret GitHub API failures")
if re.search(r"\|\|\s*true|\bif\s+gh\s+api\b", gate_commands):
    fail("existing-release gate must accept only an explicit REST 404 as absence")

publish_steps = parse_steps(release_jobs["publish"])
if [step.get("name") for step in publish_steps if step.get("name")] != [
    "Validate downloaded release archives",
    "Publish immutable GitHub release",
]:
    fail("release publish job must only download, validate, and publish the artifact")
if len(publish_steps) != 3 or not publish_steps[0].get("uses"):
    fail("release publish job must begin by downloading the quality artifact")
download_step = publish_steps[0]
if download_step.get("with") != {"name": "macos-dist", "path": "dist"}:
    fail("release publish job must download macos-dist into dist")
if any("actions/checkout@" in (step.get("uses") or "") for step in publish_steps):
    fail("artifact-only publish job must not checkout or rebuild source")
validation_step = next(step for step in publish_steps if step.get("name") == "Validate downloaded release archives")
validation_commands = command_text(validation_step)
for asset in expected_artifact_paths:
    if asset not in validation_commands:
        fail(f"release artifact validation is missing {asset}")
for required_fragment in (
    'find dist -type f',
    "awk 'NF == 2 && $1 ~ /^[0-9a-f]{64}$/ { print $2 }' dist/checksums.txt",
    'wc -l < dist/checksums.txt',
    '(cd dist && shasum -a 256 -c checksums.txt)',
):
    if required_fragment not in validation_commands:
        fail(f"release artifact validation is missing: {required_fragment}")

publish_step = next(step for step in publish_steps if step.get("name") == "Publish immutable GitHub release")
required_publish_env = {
    "GH_TOKEN": "${{ github.token }}",
    "GH_REPO": "${{ github.repository }}",
    "TAG": "${{ needs.preflight.outputs.tag }}",
}
if publish_step.get("env") != required_publish_env:
    fail("release publication requires explicit token, repository, and tag context")
publish_commands = command_text(publish_step)
if 'gh release create "${TAG}"' not in publish_commands:
    fail("release publication must create the preflight-approved tag release")
for asset in expected_artifact_paths:
    if publish_commands.count(asset) != 1:
        fail(f"release publication must attach {asset} exactly once")

all_workflow_commands = "\n".join(
    command_text(step)
    for _, _, blocks in workflows.values()
    for block in blocks.values()
    for step in parse_steps(block)
)
if all_workflow_commands.splitlines().count("make dist") != 1:
    fail("the reusable quality workflow must be the sole make dist owner")
release_commands = "\n".join(command_text(step) for block in release_jobs.values() for step in parse_steps(block))
if re.search(r"(?:^|\n)\s*(?:make\s+dist|cargo\s+(?:build|test|clippy)\b|gh\s+release\s+upload\b)", release_commands):
    fail("release preflight and publish jobs must not rebuild or upload replacement assets")
if re.search(r"(?:^|\s)--clobber(?:\s|$)", release_commands):
    fail("release publication must never clobber assets")
PY

printf 'build contract ok: version %s\n' "$package_version"
