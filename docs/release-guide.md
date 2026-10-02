# Build, release and deployment

The canonical product helper is [scripts/a9_release.py](../scripts/a9_release.py).
It uses Python's standard library and passes subprocess commands as argv arrays.
Its versioned JSON formats are defined in [release/schemas](../release/schemas);
[examples](../release/examples) contain placeholders and are not deployable
configuration until completed by the operator.

## Separate records

| Record | Meaning |
| --- | --- |
| Build manifest | Exact committed source, tool pins, lockfile/schema/Wasm hashes and build checks |
| Deployment config | Intended network, manifest hash, token, fees, label and public CLI/key reference; no invented code IDs/addresses |
| Deployment receipt | Actual broadcast/confirmed transactions, resulting code IDs/address and independent verification |
| Production gate reports | Runtime/network/artifact-bound assertions, attestations and hashed primary evidence |

Never place private keys or seed phrases in these records. A source/interface
pin does not establish the runtime binary executing on validators.

## Reproducible artifacts

Use a committed checkout at the intended release `HEAD`:

```powershell
python scripts/a9_release.py build --commit HEAD --output artifacts/release
python scripts/a9_release.py verify-artifacts --manifest artifacts/release/build-manifest.json
```

The helper requires the selected commit to match HEAD and contract/build/tooling
inputs to be clean, including both lockfiles. Its `SOURCE_PATHS` list includes
contracts, packages, generator, scripts, release formats, CI, VERSIONS and SECURITY.
Other files are not silently deleted or stashed. Two independent `git archive`
exports are built using optimizer `0.16.1`, digest
`sha256:b9c92b2900b7ebaab3499203615c1b8589592bc557355ed3432e48851ffde69e`,
platform `linux/amd64`, with Rust 1.81.0. Both Factory/Deal Wasm hashes must match
and pass `cosmwasm-check 2.2.2`. The manifest records each required check as pass.
Missing/not-run checks, divergent outputs or unavailable Docker fail the build.
Ordinary `cargo build` does not replace optimizer evidence.

Generated schemas and protobuf snapshots must reproduce as committed. Tool
versions are specified in [VERSIONS.md](../VERSIONS.md), the helper and CI.
Release output under `artifacts/` is ignored; it is not contract source.

## Production evidence gates

Local/testnet configs require `runtime_evidence: null`. Production requires
`gonka-marketplace-release-evidence` schema version `1.0.0` with exactly these
hashed reports:

| Report ID | Required proof |
| --- | --- |
| `B1_allowlisted_grpc` | Exact four routes and constructors callable from deployed Wasm; broad routes stay unavailable |
| `B3_gas_pruning_bounds` | Native response construction/gas limits, real routing pruning boundaries and bounded processing |
| `B4_runtime_and_claimed_safety` | Actual runtime identity/advisory remediation; claim marker/payment atomicity and VM gas/panic safety |
| `B5_golden_e2e` | Fresh full acceptance for exact deployable Factory/Deal and selected Gonka/runner identities |
| `B6_production_parameters_and_operations` | Agreed network/token/fee/vesting parameters, signer and keeper procedures, monitoring and recovery |

The optional typed-absence enhancement is not an additional gate. Each required
report must be `PASS`, identify its kind/check ID and full target, include operator,
UTC timestamp and method attestation, non-empty assertions, and at least one
primary artifact with a verified path/hash. FAIL, PARTIAL, NOT_RUN or bare PASS
declarations do not satisfy validation.

The shared target binds production chain ID, exact HTTPS RPC endpoint, height-1
`block_id.hash` network fingerprint, pinned interface source, manifest hash,
runtime source/version/non-zero binary hash, deployable Wasm hashes, CW20/decimals,
fee recipient/bps, label and explicit transaction parameters. Preflight compares
the fingerprint against the connected node and checks report/artifact bytes.
Changing semantic parameters invalidates evidence. Runtime source is distinct
from protobuf source; their relationship must be explained rather than forced
equal. A matching chain ID alone is insufficient.

`runtime_evidence_complete` is a validation result, not an operator override.
Hash consistency does not prove remote runtime truth: production review must
assess operator/validator attestation and native receipts. No documentation or
historical test verdict supplies these gates for a new release.

## Configure and preflight

Keep the Gonka checkout separate from this repository and supply a built
`inferenced` CLI path; the helper neither creates keys nor builds the native
chain. The optional `--gonka-checkout` check compares that checkout's HEAD to
`expected_gonka_source_sha`, which is fixed to the protobuf/interface pin
`379bebced638aeb5e6077bfd51c986f898443832`, not the production runtime SHA.
Do not pass a newer runtime checkout to this optional interface-source check;
it will reject it. Identify that runtime separately through release evidence.
Start with
[deployment-config.local.example.json](../release/examples/deployment-config.local.example.json),
save a completed copy under `artifacts/deployment-config.json`, and hash manifest
bytes for `build_manifest_sha256`. Fee must be 150 bps and token decimals 6.

```powershell
$manifestPath = 'artifacts/release/build-manifest.json'
$configPath = 'artifacts/deployment-config.json'
$gonkaCheckout = (Resolve-Path '../gonka').Path
$gonkaCli = Join-Path $gonkaCheckout 'inference-chain/inferenced.exe'
$manifestHash = (Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
```

Use the actual CLI location on your host (on Linux typically `inferenced`). Both
commands below are read-only and check manifest/artifacts, config, optional Gonka
checkout, node chain ID, token and production evidence where required:

```powershell
python scripts/a9_release.py check-environment --manifest $manifestPath --config $configPath --gonka-checkout $gonkaCheckout --cli $gonkaCli
python scripts/a9_release.py prepare-deployment --manifest $manifestPath --config $configPath --gonka-checkout $gonkaCheckout --cli $gonkaCli
```

Preparation prints two store argv arrays and one Factory instantiate argv array.
Instantiate must contain exactly one `--no-admin` and no `--admin`, with no funds.
Omitting `--admin` alone is not the explicit no-admin declaration. No transaction
is signed or broadcast by these commands.

## Broadcast, reconcile and verify

Only `deploy` broadcasts and requires the acknowledgement flag:

```powershell
python scripts/a9_release.py deploy --manifest $manifestPath --config $configPath --gonka-checkout $gonkaCheckout --cli $gonkaCli --receipt artifacts/deployment.receipt.json --approve-broadcast
```

A durable receipt is created before the first broadcast and updated as each
transaction confirms. An existing receipt blocks another deploy before network
access. Sync broadcast returns a tx hash; successful deployment requires a
separate included `query tx` response with code zero. Structured events supply
code IDs/address, then independent chain queries verify actual state.

Failed transactions are recorded failed; unresolved confirmation/timeouts become
ambiguous. There is no automatic resend. Locate the original hash/result first;
do not delete a receipt to force another deployment while inclusion is unknown.

Recheck deployment independently using read-only commands:

```powershell
python scripts/a9_release.py verify-deployment --manifest $manifestPath --config $configPath --receipt artifacts/deployment.receipt.json --cli $gonkaCli
python scripts/a9_release.py verify-deal --manifest $manifestPath --config $configPath --receipt artifacts/deployment.receipt.json --request artifacts/deal-verification.json --cli $gonkaCli
```

Use [deal-verification.example.json](../release/examples/deal-verification.example.json)
for the completed request. Verification checks chain ID, local hash bindings,
on-chain code checksum, ContractInfo code/admin, Factory config and token decimals.
Deal verification first reuses deployment verification, then checks Deal code/admin,
Host/E/price/budget, Factory/token/fees/interface SHA and exact Factory pair index.
It never creates offers automatically.

Offline fake-CLI fixtures validate orchestration and rejection of mismatches.
They do not validate a real keyring, runtime, RPC or event encoding. Build and
deployment verification are necessary but not sufficient for production readiness.

The runner maintains its own pinned, hashed copy of the helper. Product helper
updates require a reviewed import there and a new runner image/plan; the runner
must not dynamically load tooling from a contracts checkout.
