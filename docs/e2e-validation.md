# E2E validation across repositories

The independent acceptance runner is maintained in
[gonka24/forward-e2e](https://github.com/gonka24/forward-e2e).
It was extracted from contracts commit
`d637eea5432506d60c90c1d8436c67b93802d829`, after PR #1.

## Responsibilities

`forward-contracts` owns production Rust contracts and packages, schemas,
unit/property/`cw-multi-test` tests, release/deployment tooling and the small
`a8-caller`, `a8-cw20` and `a8-query-boundary` Cargo fixtures. The E2E runner
builds these fixtures from the same selected contracts SHA; they are not
production contracts.

`forward-e2e` owns the acceptance catalog, orchestrator, verifier, host wrappers,
Docker image and Compose configuration, external Kotlin tests, network
templates, Go/Wasm probes and their offline Python tests. Its pinned copy of
`scripts/a9_release.py` comes from the recorded extraction commit, is tested
there and is hashed into the runner lock. Updates to the product helper require
an explicit reviewed import into the runner and a new image/plan.

Historical reviews and evidence remain here for audit continuity. The runner
keeps byte-identical copies needed by its recorded-fixture tests. Neither copy
certifies a new contracts commit or a new runner image.

## Run against a contracts commit

From a separate `forward-e2e` checkout, build the image with
`ops/e2e/build-runner.sh` (Windows: `ops/e2e/Build-Runner.ps1`), then run:

```bash
./ops/e2e/run-e2e.sh run \
  --gonka-repo https://github.com/gonka-ai/gonka \
  --gonka-sha <GONKA_FULL_40_HEX_SHA> \
  --contracts-repo https://github.com/gonka24/forward-contracts \
  --contracts-sha <CONTRACTS_FULL_40_HEX_SHA> \
  --profile all \
  --output ./out/e2e
```

On Windows use `ops/e2e/Run-E2E.ps1` with the same arguments. For a sibling
contracts checkout, replace `--contracts-repo` with
`--contracts-path ../forward-contracts`. Both inputs must be full 40-hex SHAs;
branches, tags and `HEAD` are rejected. The selected source snapshots remain
immutable and outputs live outside them.

See the [runner guide](https://github.com/gonka24/forward-e2e/blob/main/ops/e2e/README.md)
for portable plans, replay, offline reporting and evidence recovery.

## Release evidence

A release review must identify the exact contracts commit, Gonka commit,
runner commit/version and immutable runner image ID. Keep the run lock,
build/execution manifests, deployed Wasm checksums and full result package.
An automated `PASSED` verdict still has `acceptance_status: NOT_REVIEWED`.

Contract CI runs local Rust and release-tooling checks. Runner CI runs offline
Python tests, local Git acquisition tests, Windows wrapper checks and Compose
validation. Neither CI workflow starts a live chain or replaces the full
24-check acceptance run for the final release commit.

Changing the runner or rebuilding its image requires a new plan. Existing
locks remain bound to their original runner hashes and image IDs; moving code
to this repository must not rebind or relax those checks.
