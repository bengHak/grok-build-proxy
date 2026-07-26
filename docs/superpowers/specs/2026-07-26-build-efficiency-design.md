# Build Efficiency and Release Integrity Design

**Date:** 2026-07-26  
**Status:** Approved for planning

## 1. Goal

Reduce local and CI build waste while making release validation identical, versioning unambiguous, and published assets immutable.

The work covers the full N0 recommendation from `docs/maintainer-report-2026-07-26.md`:

- preserve Cargo incremental artifacts during distribution builds;
- remove duplicate CI builds;
- share one quality workflow between CI and release;
- use `Cargo.toml` as the only version source;
- publish only from a matching Git tag;
- reject an already-existing release instead of replacing assets.

It does not add artifact signing, attestation, dependency auditing, or changes to runtime proxy behavior.

## 2. Current Problems

### 2.1 Distribution builds always start clean

`Makefile` declares `dist: clean build-arm64 build-amd64`. The `clean` target runs `cargo clean`, deleting the complete `target/` directory before every dual-architecture build. The current local `target/` is approximately 6.2 GB, so this discards all reusable dependency and incremental artifacts.

CI also runs `make build` immediately before `make dist`. The native release binary built by `make build` is then discarded when `make dist` invokes `cargo clean`, making that build entirely redundant.

### 2.2 CI and release enforce different quality gates

CI runs formatting, Clippy, tests, installer syntax validation, a native build, and distribution builds. Release only runs tests and distribution builds. A tagged commit can therefore reach publication without the same formatting, Clippy, and installer checks used for pull requests.

### 2.3 Version state is duplicated

The project version is represented by:

- `[package].version` in `Cargo.toml`;
- the `VERSION` file;
- `GROK_BUILD_PROXY_VERSION` passed by Make and release automation;
- `build.rs`, which injects `GROK_BUILD_PROXY_BUILD_VERSION`;
- `CARGO_PKG_VERSION`, already used by the Kimi API-key client.

This permits CLI, package metadata, and User-Agent versions to diverge.

### 2.4 Release reruns can replace published bytes

The release workflow accepts tag, branch, and manual invocation paths. If a release already exists, it uploads assets with `--clobber`. A rerun can therefore replace archives and checksums under an existing version.

## 3. Chosen Architecture

### 3.1 Incremental-safe Make targets

`make dist` will clean only generated packaging directories (`bin/` and `dist/`) and will never invoke `cargo clean` implicitly. An explicit `make clean` remains available for developers who intentionally want to delete Cargo artifacts.

The dual-architecture release builds remain separate Cargo invocations because they target different Apple triples, but both reuse Cargo's target cache. The CI-only native `make build` step is removed because `make dist` already compiles both supported release architectures.

`make dist` will derive its version from Cargo package metadata rather than a file or environment override. Build commands will not set `GROK_BUILD_PROXY_VERSION`.

### 3.2 Reusable quality workflow

Add a reusable workflow invoked through `workflow_call`. It owns the shared quality contract:

1. checkout;
2. Rust 1.88 with rustfmt, Clippy, and both macOS targets;
3. Rust cache restoration;
4. `cargo fmt --check`;
5. `cargo clippy --locked --all-targets --all-features -- -D warnings`;
6. `cargo test --locked --all-targets`;
7. `sh -n install.sh`;
8. `make dist` using locked Cargo dependencies;
9. archive/checksum validation;
10. optional upload of the already-validated distribution files as a GitHub Actions artifact.

CI becomes a thin caller of this workflow. Release calls the same workflow with artifact upload enabled, then publishes exactly the downloaded files. The release job does not rebuild them.

All third-party actions, including official GitHub actions, are pinned to full commit SHAs. A human-readable version comment may accompany each SHA.

### 3.3 Cargo as the single version source

`Cargo.toml` `[package].version` becomes authoritative.

- Remove `VERSION`.
- Remove `build.rs` and `GROK_BUILD_PROXY_VERSION` support.
- Use `env!("CARGO_PKG_VERSION")` for CLI output and all runtime User-Agent/version fields.
- Make reads the version from `cargo metadata --no-deps --format-version 1` only where a shell value is required; it cannot override the version.
- Release compares `GITHUB_REF_NAME` with `v${package.version}` before running the quality workflow.

A version release therefore requires changing `Cargo.toml`/`Cargo.lock`, committing that change, and creating the exact matching tag.

### 3.4 Tag-only immutable release

The release workflow triggers only on `v*` tag pushes. `workflow_dispatch` and `release-v*` branch triggers are removed.

A preflight job runs before the expensive quality workflow and:

1. verifies the ref is a tag;
2. reads the Cargo package version;
3. requires the tag to equal `v${package.version}` exactly;
4. checks that the GitHub release does not already exist.

If an existing release is found, the workflow fails. It does not repair an empty release, compare remote assets, upload missing files, or replace bytes.

After quality succeeds, the publish job downloads the workflow artifact, validates its expected filenames and checksums again, and executes one `gh release create --verify-tag`. No `gh release upload` or `--clobber` path remains.

## 4. Data and Control Flow

### Pull request or main push

```text
ci.yml
  -> reusable quality workflow
     -> shared checks
     -> dual-architecture dist build
     -> archive/checksum verification
```

### Release tag push

```text
release.yml
  -> preflight(tag == Cargo version, release absent)
  -> reusable quality workflow(upload artifact = true)
  -> publish job downloads validated artifact
  -> validate files/checksums
  -> gh release create --verify-tag
```

The published archive bytes are therefore the same bytes that passed the shared quality workflow.

## 5. Failure Handling

- Invalid or mismatched tag: fail in preflight before compilation.
- Existing release: fail in preflight before compilation.
- Formatting, Clippy, tests, installer, build, or package validation failure: do not start publish.
- Missing or unexpected downloaded asset: fail before `gh release create`.
- Checksum mismatch: fail before publication.
- Publication race after preflight: `gh release create` fails because the release now exists; no overwrite path is available.
- Explicit `make clean` remains destructive by design; no ordinary build or distribution target depends on it.

## 6. Verification Strategy

The user approved a destructive before/after benchmark. The existing `make dist` will therefore be run once before implementation even though it invokes `cargo clean` and deletes the current approximately 6.2 GB `target/` cache.

Record:

1. **Before:** current cold `make dist` wall time and resulting archive/checksum status.
2. **After cold:** changed `make dist` after an explicit clean, wall time and output validity.
3. **After warm:** immediate second changed `make dist`, wall time and output validity.
4. **Preservation:** create a sentinel under `target/`; changed `make dist` must retain it.

Functional validation:

- `cargo fmt --check`;
- `cargo clippy --locked --all-targets --all-features -- -D warnings`;
- `cargo test --locked --all-targets`;
- `sh -n install.sh`;
- archive listing contains only the binary, `LICENSE`, and `README.md`;
- `shasum -a 256 -c dist/checksums.txt` succeeds;
- binary `--version` equals `cargo metadata` package version;
- no `VERSION`, `GROK_BUILD_PROXY_VERSION`, or `GROK_BUILD_PROXY_BUILD_VERSION` references remain;
- CI and release both call the reusable quality workflow;
- release has tag-only triggers, exact tag/version comparison, early existing-release rejection, and no overwrite command;
- workflow YAML parses and repository workflow invariants pass static tests.

Timing results are reported as observations from this machine, not universal speedup claims.

## 7. Workflow-Orchestrated Implementation

A Grok workflow will coordinate bounded agents with isolated responsibilities:

1. baseline benchmark and contract inventory;
2. parallel implementation of Make/version changes and GitHub Actions changes in isolated worktrees;
3. integration into the main workspace;
4. independent code/build and CI/release contract reviewers;
5. corrective pass if reviewers find actionable issues;
6. orchestrator-level cold/warm benchmark and complete verification.

External publication is explicitly excluded. The workflow may edit local files and run builds/tests, but it must not create tags, releases, branches on the remote, or push commits.

## 8. Non-goals

- Runtime proxy performance or behavior changes.
- Dependency upgrades.
- Linux or Windows artifacts.
- Artifact signing, provenance attestation, or SBOM generation.
- `cargo audit`/`cargo deny` policy.
- Release recovery for an accidentally created empty release.
- Backward compatibility for `make dist VERSION=...` or manual/branch-based releases.
