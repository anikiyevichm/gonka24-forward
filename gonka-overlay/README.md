# Gonka Test & Runtime Overlay

This directory contains test harness components, Wasm query allowlists, Docker Compose fixtures, genesis overrides, and fault-injection runtime modules for the **Gonka Network** that support live acceptance and integration testing for the **Gonka 24 Smart Contracts**.

## Rationale

These components are **not intended to be merged into the core Gonka blockchain codebase** (`gonka-ai/gonka` or production branches). Instead:
1. They are maintained and versioned directly within the smart contract repository (`gonka24-smart-contract`).
2. The acceptance test runner (`scripts/a8_acceptance.py`) takes an unmodified, clean checkout of a specific pinned Gonka version (e.g. `379bebced638aeb5e6077bfd51c986f898443832` / `main`).
3. The runner dynamically creates an isolated temporary copy of the Gonka tree, overlays these files onto it, and runs compilation and acceptance tests against the temporary copy.
4. The host Gonka repository remains pristine and unmodified.

## Provenance

- **Base Gonka SHA**: `379bebced638aeb5e6077bfd51c986f898443832` (`mlnode vLLM 0.25.1...`)
- **Reviewed Harness Head**: `a21fee6ba8e51fc18acfe911874a68433d9839c7` (combining PR #1, PR #2, and PR #3)
- **Integrity**: Verified by `CHECKSUMS.sha256`

## File Directory Structure

- `inference-chain/app/`
  - `legacy.go`: Registers marketplace-accessible gRPC queries (`/inference.inference.Query/GetCurrentEpoch`, `ListClaimRecipients`, `EpochPerformanceSummaryByParticipant`, `/inference.streamvesting.Query/TotalVestingAmount`) and hooks query fault injection options.
  - `legacy_test.go`, `wasm_grpc_query_allowlist_test.go`: Go tests validating the gRPC query allowlist.
  - `a8_faults_disabled.go`, `a8_faults_disabled_test.go`: Default no-op query fault interceptor.
  - `a8_faults_enabled.go`, `a8_faults_enabled_test.go`: Query fault decorator active when building with `-tags=a8faults`.
  - `a8_query_fault_selector_test.go`: Tests for fault selector matching.
  - `a8faults/plan.go`, `a8faults/plan_test.go`: Parser and evaluator for query fault plans.
- `inference-chain/cmd/a8-query-fault-plan/`
  - `main.go`: Tool to parse and validate query fault plans.
- `inference-chain/contracts/p0-probe/`
  - CosmWasm probe contract used to test gRPC query routing from inside the Wasm VM.
- `inference-chain/scripts/init-docker-genesis.sh`
  - Genesis initialisation script with support for opt-in foreign native denom account (`A8_B3_FOREIGN_DENOM`).
- `local-test-net/`
  - `docker-compose.a8-query-faults.yml`: Docker Compose overlay mounting fault plans and custom binary.
  - `docker-compose.genesis-a8-b3-foreign-denom.yml`: Docker Compose overlay for B3 foreign denom genesis.
- `testermint/`
  - `src/main/kotlin/DockerGroup.kt`: Extended Docker compose configuration and robust directory permission setup.
  - `src/main/kotlin/LocalInferencePair.kt`: Container management fix for restarting stopped APIs.
  - `src/test/kotlin/MarketplaceContractAcceptanceTests.kt`: Complete acceptance test suite running against local-test-net.
  - `src/test/kotlin/MarketplaceHarnessProcess.kt`: Harness process driver invoking `scripts/a8_acceptance.py`.
  - `src/test/kotlin/A8PackageBBankFaultPlan*.kt`, `DockerBindUserArgsTests.kt`, `MarketplaceHarnessProcessTests.kt`: Auxiliary test classes.
  - `src/test/resources/a8-b3-genesis-validation-overrides.json`: Genesis validation overrides for foreign denom tests.
- `README_SMART_CONTRACT_TEST.md`
  - Local verification guide and test execution instructions.

## Verification

To verify that the overlay directory is intact:
```bash
shasum -a 256 -c CHECKSUMS.sha256
```

### Workspace preparation guarantees

Preparation checks out the exact pinned base and aborts on clone, checkout or
commit errors; it never copies a dirty source tree or falls back to another HEAD.
The checksum manifest must enumerate every payload file exactly once. Only the
root README and the manifest itself are metadata; nested README files are payload.
Unlisted files, malformed entries, unsafe paths and symlinks are rejected.

`--keep-temp-gonka` retains the generated workspace for inspection. Without it,
the automatically allocated workspace is removed after the run. An explicit
`--temp-gonka-dir` is caller-owned and is retained. These options do not perform
Docker cleanup; continue using the ownership-scoped launcher cleanup procedure.
