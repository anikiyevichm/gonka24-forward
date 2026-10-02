# Validation and release acceptance

## Proof levels and local checks

Unit tests establish guards and query/response handling. Property tests establish
math conservation, rounding and monotonicity. `cw-multi-test` exercises actual
contract messages with CW20/bank ledgers, but its Gonka adapters are synthetic.
Go/VM probes establish only the boundary they actually execute. Native E2E must
use real Gonka routing, claims, streamvesting and bank keepers. These levels are
complementary and must not be reported as equivalent proof.

Run the checks appropriate to changes; CI defines full release validation:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
python3 -B -m unittest discover -s scripts/tests -p 'test_*.py' -v
```

On Windows, use `python` for the Python command. Archive-extraction tests create
symlinks; the Windows account needs symlink privilege or Developer Mode for
those cases, otherwise they fail with WinError 1314. Do not report the whole
suite as passed when this host requirement prevents execution.

CI also formats/tests/lints `tools/proto-gen` with its separate lockfile,
regenerates protobuf and both contract schemas requiring a clean diff, performs
dual optimizer builds and verifies the manifest. Security checks run `cargo audit`
and `cargo deny` for both dependency graphs. Security CLIs use a separate Rust
1.88.0 (`cargo-audit 0.22.2`, `cargo-deny 0.20.2`); contract Rust remains 1.81.0.
Do not freeze an advisory database to hide new findings. `deny.toml` governs
unmaintained/yanked/duplicate dependency handling; historical warnings must be
reassessed on upgrades rather than reported as current scan results.

Generation on Linux uses `scripts/generate-proto.sh` and `generate-schema.sh`;
Windows uses the corresponding `.ps1` scripts. The pinned vendored protobuf
snapshot is checksummed before generation; no manual wire-layout reconstruction.
Relevant executable coverage lives in:

- [Deal tests](../contracts/marketplace-deal/src/contract/tests.rs): funding,
  state/epoch/proof guards, refund classification and independent debts.
- [Query adapter tests](../contracts/marketplace-deal/src/gonka.rs): wire/identity/
  size/denom validation and typed raw-query categories.
- [Math/property tests](../packages/marketplace-common/src/math/tests.rs): economic
  examples, conservation, wide arithmetic and partition-independent rounding.
- [Factory tests](../contracts/marketplace-factory/src/contract/tests.rs) and
  [integration suite](../contracts/marketplace-factory/tests/integration.rs):
  registry/reply rollback, complete lifecycles, recipient failures, no-admin,
  isolated deals, donations and late distributions.
- [Release tooling tests](../scripts/tests/test_a9_release.py): offline build,
  evidence/preflight, ambiguous tx/receipt and independent verifier fixtures.

## Preserve policy coverage when changing acceptance

Former review inventories mapped 73 query-fault checks: 71 synthetic contract/
ledger-policy cases and two native HostOnly recovery cases. The current coverage
obligations survive removal of those review files; old offline statuses do not.

Routing cases include duplicate rows, current-epoch failure, native handler error
and malformed protobuf for both Lock and routing Refund, with unchanged state and
healthy retry. They map to `c_routing_*` tests. Summary cases include handler error,
unsupported route, invalid participant, wrong Host/epoch, absent nested summary,
malformed and oversized bytes. Each must cover E+2 rejection, E+3 fallback,
SettleClaim rejection, and terminal behavior after recovery (`c_policy_*`).

Ledger tests
`c_recovery_before_deadline_preserves_ledgers_and_settles_once` and
`c_network_unconfirmed_refund_rolls_back_retries_and_keeps_terminal_host_only_economics`
check recovery before finalization, atomic Buyer refund failure/retry, no double
settlement and permanent terminal HostOnly GNK. Actual native emergency HostOnly
release with bank restriction rollback/retry is a separate obligation. Synthetic
fault injection does not prove arbitrary faults are reachable on production Gonka
or establish Cosmos SDK/wasmd/FFI execution behavior.

## Independent acceptance runner

[forward-e2e](https://github.com/gonka24/forward-e2e) owns acceptance catalog,
orchestrator/verifier, host wrappers, Docker/Compose, external Kotlin tests,
network templates, Go/Wasm probes and offline Python tests. Read its AGENTS.md
before changing it. The contracts repository retains the test-only `a8-caller`,
`a8-cw20` and `a8-query-boundary` crates; the runner builds them from the same
selected contracts SHA. Do not remove them as historical documentation.

The runner's copied `a9_release.py` is separately pinned and hashed into its lock.
Changing product tooling requires an explicit reviewed import there. Locks cannot
be rebound to a new image or relaxed after code movement.

Use the runner's own build scripts to create an image, then from its checkout:

```bash
./ops/e2e/run-e2e.sh run \
  --gonka-repo https://github.com/gonka-ai/gonka \
  --gonka-sha <GONKA_FULL_40_HEX_SHA> \
  --contracts-repo https://github.com/gonka24/forward-contracts \
  --contracts-sha <CONTRACTS_FULL_40_HEX_SHA> \
  --profile all \
  --output ./out/e2e
```

Windows uses `ops/e2e/Run-E2E.ps1` with the same arguments. A separate sibling
checkout can be supplied with `--contracts-path ../forward-contracts`. Select
full 40-hex SHAs, not HEAD, branches or tags. Follow the selected runner revision's
guide for build, plan, replay and recovery. Inputs must stay immutable; out-of-tree
outputs and separate network work directories must not alter either checkout.
Cleanup must be restricted to proven run-owned resources.

## Required native scenarios

The exact profile/catalog belongs to the selected runner. Preserve these
acceptance requirements when splitting scenarios; labels alone are not evidence:

| Scenario family | Required observations |
| --- | --- |
| Deployment/allowlist | Factory and Deals have no admin; code checksums/config match; all four exact routes work from Wasm, broad routes reject |
| Funded claim | Exact pre-E funding and recipient, Lock proof, real positive claim, independently checked native summary and settlement debts; per-role payment failure/retry without blocking other transactions |
| Claim restart | Prepare/manual claim with API stopped, restart/resume and prove one included native claim rather than blindly claiming twice |
| Economics | Under/exact/overproduction, reward/work mixes, dust, no-sale, zero/unclaimed outcomes; compute expectations from native amounts and offer terms independently of Deal state |
| Original vesting | At least two observed native tranches, matching vesting decreases/spendable increases, timed releases and exact cumulative payouts; zero-balance repeat rejects unchanged |
| No-sale/contamination | Buyer absent, positive native claim, Host ownership, donations before/after settlement, foreign CW20/native balances untouched |
| Vesting additions | Add to a non-empty native schedule with recorded funding/governance receipts; no early release and no postponement of original tranches; equal/different/zero periods and tranche remainder |
| Late Completed donations | Two included donor-to-Deal transfers and release receipts; recompute cumulative rounding, keep frozen shares at U > T and no duplicate completion |
| Routing refunds/isolation | Missing and mismatched native rows; exact deposit refund, CW20 rollback/retry, unchanged terms, permanent Factory indices and independent ledgers across multiple Host/E pairs |
| Lock boundaries | Funded E/E+4 success and E+5 rejection; no-Buyer E+4/E+5/pruning behavior; record actual inclusion epochs, not just pre-broadcast observations |
| Claim expiry | Positive/zero unclaimed summary, E+1 refusal and E+2 funded refund or no-Buyer Expired; zero-genesis fixtures explicitly distinguish test configuration from production |
| Network unconfirmed | Exact native missing summary, E+2 rejection and E+3 success; claimed=true always rejects; recovery cannot reopen finalized outcome |
| Emergency HostOnly recovery | After committed emergency refund, donate GNK, induce native bank restriction, prove state/balance rollback, then retry exact Host-only release |
| Gas/VM safety | Positive claimed summary and valid Lock, direct and caller/submessage gas sweep; observe OOG and unchanged accounting, and sufficient-gas semantic refusal rather than emergency success |
| Native claim atomicity | Work/reward payment failure and claimed-marker failure/abort semantics; retry without partial payout or duplicate claim |
| Runtime/operations | Actual binary identity, advisory remediation, native gas/pruning bounds, selected production parameters, keeper/recovery procedures |

For gas sweeps, phase names or producer-reported success are insufficient: inspect
native summary, exact recipient, Lock receipt, all gas attempts, OOG observations
and before/after accounting (excluding charged network fees). For no-sale vesting,
inspect actual claim, contamination transfers, the included pre-unlock zero-balance
probe, non-empty schedule addition and ordered release receipts. For late donations,
derive totals from included transfers, not asserted deltas. Testing each policy
error synthetically does not replace native payment/gas/recovery cases.

## Evidence and release verdict

Every release must bind exact contracts SHA, Gonka SHA, runner revision/version
and immutable runner image identity. Preserve run lock, plans, build/execution
manifests, deployable/test Wasm checksums, runtime metadata, deployed addresses,
confirmed tx responses, query/balance snapshots, JUnit, logs, fault configuration,
independent oracle calculations and cleanup/immutability evidence. Failed and
partial runs must retain their real status and diagnostic limits.

An automated `PASSED` run can still have `acceptance_status: NOT_REVIEWED`.
Independent review must reconcile proof levels, network parameters, omitted cases
and release-artifact binding before acceptance. Neither contract CI nor runner
offline CI starts a live chain or replaces fresh full-profile acceptance.
Rebuilding/changing the runner needs a new image/plan and evidence.

Store new run packages outside source snapshots, in durable release/runner
artifact storage with immutable identities. Do not commit raw logs to product
`docs`. Historical reviews/evidence remain accessible through prior Git revisions;
the runner's recorded-fixture copies are maintained there independently. Deleting
the product documentation copies does not authorize deleting those fixtures or
relabeling historical evidence as a current-release result.
