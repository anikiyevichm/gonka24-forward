# Scripts

This directory contains reproducible scripts for generating protobuf definitions, schemas, and release Wasm binaries.

A script is added only alongside tests verifying its output. The release build must use the optimizer image and digest pinned in `VERSIONS.md`.

## Gonka Protobuf

```bash
# On Linux/macOS:
./scripts/generate-proto.sh

# On Windows PowerShell:
# powershell -ExecutionPolicy Bypass -File scripts/generate-proto.ps1
```

The script first verifies the SHA-256 of each vendored `.proto`, then executes an isolated host-only generator on Rust `1.81.0`. System `protoc` and `buf` installations are not required; upstream `.proto` files are not fetched over the network. On a clean machine, Cargo downloads exact dependency versions from `tools/proto-gen/Cargo.lock` once, after which the command runs from the local Cargo cache. Normal contract compilation does not invoke this generator.

## JSON Schema

```bash
# On Linux/macOS:
./scripts/generate-schema.sh

# On Windows PowerShell:
# powershell -ExecutionPolicy Bypass -File scripts/generate-schema.ps1
```

This command generates the full IDL for each contract along with separate raw schemas for
instantiate, execute, query, and all query responses. Generated files are tracked in Git; CI reruns the command and requires a clean diff.

## A9: Optimizer Build, Manifest, and Deployment

The single command below archives the current commit into two independent build directories,
executes the pinned optimizer for `linux/amd64` twice, compares SHA-256 hashes of both Wasm
binaries, executes `cosmwasm-check 2.2.2`, and produces `build-manifest.json`:

```bash
python scripts/a9_release.py build --commit HEAD --output artifacts/a9-local
```

The command will fail if the current `HEAD` does not match the specified commit, uncommitted
changes exist in contract/build/tooling inputs, any committed lockfile is missing, Docker
is unavailable, the two builds differ, or Wasm validation fails. It never deletes or hides
user files. Standard `cargo build` is deliberately disallowed as an optimizer substitute.

Re-verifying an existing release bundle does not trigger a build:

```bash
python scripts/a9_release.py verify-artifacts --manifest artifacts/a9-local/build-manifest.json
```

Deployment is divided into read-only preparation and explicit transaction broadcast. The complete workflow, config/receipt formats, and integration of an external Gonka checkout are documented in [`docs/deployment-tooling.md`](../docs/deployment-tooling.md). Private keys and seed phrases are excluded from all formats; `tx.from` is strictly a key name or public address accessible to the specified CLI.

Tooling tests do not access the network and rely on verified JSON response fixtures:

```bash
python -m unittest discover -s scripts/tests -p 'test_*.py' -v
```

These fixtures validate parsing, fail-closed assertions, and retry policies of our
tooling. They do not prove compatibility with a live running fork of Gonka.

## Live acceptance testing

The acceptance harness and host wrappers live in
[gonka24/forward-e2e](https://github.com/gonka24/forward-e2e).
See the [E2E validation handoff](../docs/e2e-validation.md) for the repository
boundary and the exact source identities required for a release.
