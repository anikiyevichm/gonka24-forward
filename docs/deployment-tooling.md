# A9: Reproducible Build and Deployment Tooling

Status: Preparatory phase. This document does not authorize production
deployment and does not declare A9/MVP production-ready. Pinned Gonka source:
`379bebced638aeb5e6077bfd51c986f898443832`.

## 1. Three Distinct Entities

| Entity | What It Records | What It Does Not Contain |
|---|---|---|
| Build manifest | commit, pinned build tools, lockfile/schema/Wasm SHA-256, and build gate results | chain addresses, code IDs, and assumptions regarding completed deployment |
| Deployment config | targeted network, RPC, expected manifest, CW20, fee recipient, fee bps, and CLI parameters | secrets and unobtained code IDs / addresses |
| Deployment receipt | confirmed tx hashes/heights, actual code IDs, Factory address, and read-only verification | seed phrases / private keys and fictitious results |

JSON Schemas version `1.0.0` reside in [`release/schemas`](../release/schemas).
Examples intentionally contain explicit placeholders and will not pass validation until
an operator supplies actual local or agreed parameters.

Important: Pinned source SHA specifies which interface/protobuf source has been reviewed
by the Marketplace. It does not equal `runtime.source_sha`: the latter refers to the source
from which the actually running runtime binary is claimed to have been compiled. Neither
in isolation proves that validators are running that binary.

For `network_class = local` and `testnet`, `runtime_evidence` must be `null`.
For `production`, versioned `gonka-marketplace-release-evidence` `1.0.0` is required;
the legacy set of nullable fields no longer constitutes production evidence.
Its schema is defined in `release/schemas/deployment-config.schema.json`, and the
format of each report in `release/schemas/release-evidence-check.schema.json`.

### Production Release-Evidence 1.0.0

Evidence contains `runtime` (`source_sha`, non-empty `binary_version`, and
non-zero `binary_sha256`), along with a shared `target`: production chain ID,
exact HTTPS RPC endpoint, and fingerprint — `block_id.hash` of block height 1,
read by tooling via `inferenced query block 1`. Preflight fail-closed checks
reconcile this hash against the connected node's response; a matching chain ID
alone is no longer sufficient to spoof the network using local A8/Testermint.

Target additionally mirrors pinned interface source, build-manifest hash, runtime
source/version/binary hash, deployable Deal/Factory Wasm hashes, CW20/decimals,
fee recipient/bps, Factory label, and explicit tx parameters. Any modification to
these values after evidence generation invalidates production preflight. This does
not equate runtime source with pinned interface source: the relationship between
them must be explicitly articulated in verification B4.

Individual hashed JSON reports with status strictly `PASS` are mandatory:

- `B1_allowlisted_grpc`;
- `B3_gas_pruning_bounds`;
- `B4_runtime_and_claimed_safety`;
- `B5_golden_e2e`;
- `B6_production_parameters_and_operations`.

These are exactly the required B-dependencies from the backlog; B2 is marked as
an optional improvement and was not introduced as a new gate. Each report mirrors
its ID and complete target, and must include attestation (`operator`, UTC
timestamp, method), a non-empty list of assertions, and at least one primary artifact
with path and SHA-256. Tooling validates existence and hash of both report and each
artifact, schema/kind, `PASS`, and target alignment with config, current manifest,
and connected node. Empty declarations of `status=PASS` are rejected.
Consequently, `FAIL`, `NOT_RUN`, `PARTIAL`, empty/zero values, reports from
local A8 runs (even with `gonka-mainnet` chain ID), foreign runtimes, or
divergent Wasms cannot achieve production-readiness.

`runtime_evidence_complete` is returned strictly as the output of this comprehensive
validation; there is no loose "fields are not null" check. Hashes and report
consistency protect structure and file binding, but do not create remote trust:
they do not by themselves prove that the remote node is executing the claimed binary.
For that, B4/B5/B6 must incorporate verifiable operator attestations and real-network
receipts. Until such evidence is provided, production preflight deliberately remains blocked.

## 2. Reused Assets

- Rust `1.81.0`, optimizer `0.16.1`, immutable image digest, and
  `linux/amd64` are adopted unchanged from `VERSIONS.md`.
- Both committed lockfiles serve as inputs; Cargo dependencies were not updated.
- Generated protobuf definitions and contract schemas remain existing CI gates.
- `cosmwasm-check 2.2.2`, `cargo-audit 0.22.2`, and `cargo-deny 0.20.2` are not
  duplicated by new versions.
- The existing Rust CI job now executes A9 tests and replaces the standard release
  Wasm step with dual optimizer builds + manifest verification. The supply-chain job
  remains separate and unchanged.

Prior to A9, dual independent optimizer builds, machine-readable
manifest/config/receipts, transaction reconciliation, and independent verifiers were absent.

## 3. Build → Verify Artifacts

From clean committed `HEAD`:

```powershell
python scripts/a9_release.py build --commit HEAD --output artifacts/a9-local
python scripts/a9_release.py verify-artifacts --manifest artifacts/a9-local/build-manifest.json
```

`build` executes outside the source tree: two `git archive` bundles are extracted to
`artifacts/a9-local/build-1` and `build-2`. Each optimizer receives an isolated
`/code` directory, while resultant Wasm/schema snapshots and manifest are saved adjacent.
The `artifacts` directory is gitignored.

Release build verifies only paths influencing contracts/build/tooling. Extraneous
untracked review files are neither deleted nor stashed, but modifications to
`Cargo.lock`, contract sources, generator, scripts, schemas, or CI immediately halt the build.

If Docker engine is unavailable, the command exits with non-zero code. In that
case, reproducibility remains **unconfirmed**; standard `cargo build` is not
accepted as a substitute.

## 4. Dedicated Checkout for Future Gonka Fork

Marketplace and Gonka must remain adjacent or separate Git repositories.
Gonka source must never be copied here. Paths are always passed via arguments:

```powershell
$gonkaCheckout = Resolve-Path '..\gonka'
$gonkaCli = Join-Path $gonkaCheckout 'inference-chain\inferenced.exe'
```

On Linux, the binary name is typically `inferenced`; the exact path is specified by
the operator after compiling the fork. Tooling does not assume personal home directories
and creates no keys.

Developer B must provide:

1. checkout/fork URL and exact reviewed SHA;
2. compiled CLI/binary path, version output, SHA-256, and linkage between binary and source;
3. local chain ID, RPC endpoint, and lifecycle start/stop procedures;
4. settlement CW20 address with `TokenInfo.decimals = 6`, fee recipient, and signer key reference without secrets;
5. resolution or explicit status for B1–B6: allowlist, absence/retention APIs,
   gas/pruning bounds, claimed atomicity/runtime advisories, real-chain harness,
   and keeper/production operations.

Following availability of the fork, live validation of chain ID, CW20 decimals,
store/instantiate, on-chain checksum/admin/config, Deal immutable terms, and
Factory indexing will become possible. Golden claim/streamvesting E2E remains a distinct B5 gate.

## 5. Prepare Deployment Config

Copy [`deployment-config.local.example.json`](../release/examples/deployment-config.local.example.json)
to an unversioned directory or beneath ignored `artifacts/`, populate actual local
values, and compute the SHA256 hash of manifest **bytes**:

```powershell
$manifest = Resolve-Path 'artifacts/a9-local/build-manifest.json'
$manifestHash = (Get-FileHash -LiteralPath $manifest -Algorithm SHA256).Hash.ToLowerInvariant()
```

Set `$manifestHash` in `build_manifest_sha256`. `fee_bps` must equal
`150`, `settlement_cw20_decimals` must equal `6`. Code IDs and contract addresses
are omitted from config: they do not yet exist.

## 6. Check Environment and Execute Dry-Run

Both commands are read-only. They validate schema/invariants, manifest and artifact
hashes, optional checkout SHA, node chain ID, and CW20 decimals:

```powershell
python scripts/a9_release.py check-environment `
  --manifest $manifest --config artifacts/deployment-config.local.json `
  --gonka-checkout $gonkaCheckout --cli $gonkaCli

python scripts/a9_release.py prepare-deployment `
  --manifest $manifest --config artifacts/deployment-config.local.json `
  --gonka-checkout $gonkaCheckout --cli $gonkaCli
```

`prepare-deployment` prints exact argv for two `tx wasm store` invocations and one
`tx wasm instantiate`. Instantiate contains exactly one explicit `--no-admin` and no
`--admin`; funds are also omitted. Mere omission of `--admin` is insufficient:
versioned wasmd CLI `v0.54.2` rejects the command unless the operator specifies
either `--admin` or `--no-admin`, and rejects providing both simultaneously.
The command signs and broadcasts nothing.

## 7. Explicit Broadcast and Receipt Generation

This command alone broadcasts transactions; acknowledgement flag is mandatory:

```powershell
python scripts/a9_release.py deploy `
  --manifest $manifest --config artifacts/deployment-config.local.json `
  --gonka-checkout $gonkaCheckout --cli $gonkaCli `
  --receipt artifacts/deployment.local.receipt.json --approve-broadcast
```

Commands conform to documented Gonka `inferenced tx wasm store/instantiate`
interfaces in pinned `contracts/community-sale/README.md` and standard wasmd
CLI `v0.54.2`. Broadcast utilizes `sync`, followed by tooling invoking
`query tx <hash>` until confirmed, asserting `code == 0`, and extracting
`code_id`/`_contract_address` strictly from structured events.

Prior to broadcast, tooling re-verifies that Factory instantiate contains exactly
one `--no-admin` and no `--admin`. Following confirmation, the verifier
independently queries `ContractInfo` requiring empty admin: the CLI flag declares
intent, while post-deploy query validates actual on-chain state.

A receipt is created prior to first broadcast and atomically updated following
each confirmed tx. Any repeated `deploy` against an existing receipt aborts prior
to network access. Failed tx records `failed`; timeouts lacking definitive
confirmed outcomes record `ambiguous`. Tooling never automatically resends:
the operator must first locate the original hash/result.

## 8. Independent Deployment Re-Verification

This command reads network state only and is suited for execution by independent operators:

```powershell
python scripts/a9_release.py verify-deployment `
  --manifest $manifest --config artifacts/deployment-config.local.json `
  --receipt artifacts/deployment.local.receipt.json --cli $gonkaCli
```

It returns non-zero exit code upon chain ID mismatch; altered manifest/artifacts;
mismatches in on-chain checksum, code ID, or Factory address; presence of an admin;
discrepancies in Deal code ID, CW20, fee recipient, or fee bps; or `TokenInfo.decimals != 6`.

An instantiated Deal is verified separately — the verifier never automatically creates offers:

```powershell
python scripts/a9_release.py verify-deal `
  --manifest $manifest --config artifacts/deployment-config.local.json `
  --receipt artifacts/deployment.local.receipt.json `
  --request artifacts/deal-verification.json --cli $gonkaCli
```

Request is based on [`deal-verification.example.json`](../release/examples/deal-verification.example.json).
Tooling first executes the same fail-closed flow as `verify-deployment`: validity and
local hashes of manifest/artifacts, config, receipt and hash bindings, node chain ID,
Factory checksum/code ID/admin/config, and settlement CW20 decimals. Only thereafter
does it verify Deal checksum/code ID/admin, immutable Host/epoch/price/budget,
Factory/CW20/fee config, `pinned_gonka_sha`, and exact Factory `(Host, E)` index.
Both stages read files and network only; the command constructs and sends no transactions.

## 9. Boundary of Fixtures and Release Gates

Fixtures in `scripts/tests/fixtures` conform to JSON shapes for `TxResponse`,
wasmd code/contract info, smart queries, and status reconciled against:

- pinned Gonka community-sale deployment commands;
- wasmd `v0.54.2` CLI/query protobuf;
- Cosmos SDK `TxResponse` (`code`, `txhash`, `logs/events`);
- CometBFT status `node_info.network`, current Cosmos CLI form `NodeInfo.network`,
  and CometBFT RPC `/block?height=1` envelope `result.block_id.hash`.

Fake CLI additionally models wasmd `v0.54.2` behavior: instantiate lacking both
admin flags and instantiate specifying both flags terminate with errors.

These fixtures validate our parser and orchestration, but not compiled forks,
keyrings, RPC, event encoding, or live network dynamics. Open items include: review/fix
of fork B; potential updates to native adapter/protobuf; claim-expiry Refund/Expired;
golden E2E; actual runtime/advisories; production parameters and operational runbooks.
Successful build or deployment verification does not of itself establish production-readiness.
