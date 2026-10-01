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

## A8: Live Testermint Acceptance against an unmodified Gonka

The Gonka checkout is **never modified**. There is no `gonka-overlay/` any more:
the Marketplace Kotlin scenarios live in the external Gradle project
[`ops/a8/harness/testermint`](../ops/a8/harness/testermint/README.md), Compose
fragments and the B3 genesis provisioner in `ops/a8/harness/network`, the API
container controller in `ops/a8/harness/container_control.py`, and the Go/Wasm
probes in `ops/a8/harness/go-boundary` and `ops/a8/harness/wasm-probe`.

Live acceptance execution is supported **exclusively inside the Linux Docker runner container** via [`ops/e2e/run-e2e.sh`](../ops/e2e/run-e2e.sh) (Linux/macOS) or [`ops/e2e/Run-E2E.ps1`](../ops/e2e/Run-E2E.ps1) (Windows) against two explicitly selected full 40-hex commit SHAs (`--gonka-sha` and `--contracts-sha`). Direct host execution and `wsl.exe` bridging are not supported.

```bash
./ops/e2e/run-e2e.sh run \
  --gonka-repo https://github.com/gonka-ai/gonka \
  --gonka-sha <GONKA_FULL_40_HEX_SHA> \
  --contracts-path . \
  --contracts-sha <CONTRACTS_FULL_40_HEX_SHA> \
  --scenario funded-claim \
  --output ./out/e2e
```

Inside the runner container, `ops/a8` materialises both commits from Git
objects and invokes `scripts/a8_acceptance.py run-live` with mandatory
provenance expectations:
- `--expected-gonka-sha <sha>`: the selected Gonka commit. The snapshot must be exactly this commit (HEAD, tree, tracked bytes, submodules, no added or ignored files) before and after the Testermint run, and the running binary must report it.
- `--expected-marketplace-sha <sha>`: the selected contracts commit, checked the same way.
- `--work-root <dir>`: outside both snapshots; holds Gradle state, the out-of-tree upstream Testermint build, the harness build and the network work directory (`GONKA_REPO_ROOT`).
- `--testermint-harness-dir <dir>`: the runner's external Kotlin harness.
- `--expected-proto-sha <sha>`: Independent contract ABI/protobuf compatibility pin (`EXPECTED_PROTO_SHA`), never derived from the selected Gonka runtime SHA.

`run-live` fails before creating any network when a required upstream Testermint
API is missing (`TESTERMINT_API_MISSING`), fails when the selected JUnit test did
not run (`SELECTED_TEST_NOT_EXECUTED`), and fails when either snapshot changed
(`source-immutability.json` verdict other than `UNCHANGED`).

`build-external-harness --gonka-dir DIR --work-root DIR` performs only the
snapshot check, the API check, the out-of-tree upstream Testermint build and the
harness compilation — no chain or inner Docker daemon. It may download Gradle
dependencies, and the documented invocation runs inside the runner container.
It is step 2 of
[`ops/e2e/RUNBOOK-immutable-sources.md`](../ops/e2e/RUNBOOK-immutable-sources.md).

During settlement, an independent oracle evaluates native performance summary and submitted offer terms rather than economic fields from Deal state. It independently calculates Work/Reward, Buyer/Host GNK shares, gross, fee, Host net, Buyer refund, and expected CW20 deltas. During release, another oracle calculates cumulative Buyer/Host shares. Mere conservation of sums is insufficient and is not accepted as PASS.


## Independent Settlement Withdrawals (R5)

`claim-settle` saves `settlement_committed` immediately after confirmation, before
queries or withdrawals. It then sends each non-zero role in a separate transaction.
Each result is saved as `usdt_withdrawal_attempt` before attempting the next role.
A rejected role does not stop the others; the command exits non-zero after saving
all outcomes. A storage failure stops further broadcasts.

Retry outstanding payments without repeating native claim or settlement:

```sh
python scripts/a8_acceptance.py withdraw-usdt --context artifacts/a8-evidence/context.json
```

For a named scenario, add `--name <scenario>`. Use the context from the actual run.
The retry reads fresh `usdt_payments`, skips paid roles, and preserves earlier
attempt records. Once all payments finish, `settlement_delivery_verified` checks
cumulative balance deltas against the original pre-settlement checkpoint and the
independent oracle. Failed attempts never create this verification record. The
check requires isolated test balances; unrelated token transfers can invalidate
it. Older contexts without a checkpoint record withdrawals only and cannot
retroactively establish settlement evidence.

`--cw20-fault-positions` now injects faults into independent `WithdrawUsdt` calls
following settlement, checking that failed withdrawals preserve settled state,
all balances, and pending obligations. Historical atomic-settlement evidence does
not validate the new behavior. Named settlement and Package C recovery also use
separate withdrawals. Fresh real-chain acceptance is required for this version.

This is a local testnet helper using DockerGonka and the genesis key, not a
production keeper service.
