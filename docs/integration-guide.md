# Gonka Forward Marketplace: Integration Guide

Status: Contract-only interface post-A8. This guide is not a production
deployment guide and does not substitute for golden E2E on a real Gonka chain.
Pinned Gonka source: `379bebced638aeb5e6077bfd51c986f898443832`.

## Native Acceptance Boundary

Runs `a8-negative-016` and `a8-negative-017` validate live runtime gas gates
(`current_epoch >= E+3`, `claimed=true`, out-of-gas and sufficient-gas rejection),
vesting-safe donation evidence, and no-Buyer Expired outcome. They do not replace
E+2/`claimed=false` timing proof or native keeper fault harnesses; prior to their resolution
and verification of CWA-2025-007, production deployment is prohibited.

## 1. Roles and Units

| Role | What It Signs |
|---|---|
| Host | `CreateOffer`; independently native `MsgSetClaimRecipients` and `MsgClaimRewards` |
| Buyer | CW20 `Send` with hook `Fund` |
| Permissionless caller / keeper | `Lock`, `SettleClaim`, `WithdrawUsdt`, `ReleaseUnlockedGnk`, permitted `Refund`; under appropriate conditions `Cancel`/alias |

- USDT amounts and price: `microUSDT`, settlement CW20 must have 6 decimals.
- GNK amounts: `ngonka`, `1 GNK = 1_000_000_000 ngonka`.
- All CosmWasm `Uint128` and `Uint256` are serialized in JSON as decimal **strings**.
- Epoch, code id, pagination limit, and `fee_bps` are JSON numbers.
- Addresses below are explicit placeholders, not production addresses.

## 2. Deployment Boundary

Factory instantiate JSON from generated schema:

```json
{
  "deal_code_id": 17,
  "settlement_cw20": "SETTLEMENT_CW20_ADDRESS",
  "fee_recipient": "FEE_RECIPIENT_ADDRESS",
  "fee_bps": 150
}
```

Mandatory preconditions: CW20 `TokenInfo.decimals == 6`, `fee_bps == 150`, Factory
instantiation is performed with `admin = null` and without native funds. `deal_code_id` and
addresses are sourced from specific A9 deployment receipts, and parameters — from
deployment configs; build manifest contains only build evidence. This
guide does not invent them. The Factory independently creates Deals with `admin = null`.
A step-by-step safe runbook is documented in
[`deployment-tooling.md`](deployment-tooling.md).

## 3. Happy Path: Host → Buyer → Keeper

### Step 1. Host Creates Offer

```json
{
  "create_offer": {
    "target_epoch": 123,
    "price_micro_usdt_per_gnk": "1000000",
    "buyer_budget_micro_usdt": "100000000"
  }
}
```

Preconditions: `current < target_epoch <= current + 40`, price/budget > 0,
capacity > 0, `(Host, target_epoch)` does not yet exist. Caller becomes
the immutable Host. Event `offer_created` includes id, Deal address, Host, epoch,
price, and budget.

Query Deal address:

```json
{"deal":{"id":1}}
```

or:

```json
{"deal_by_host_epoch":{"host":"HOST_ADDRESS","epoch":123}}
```

### Step 2. Host Sets Native Routing

Prior to the start of E, Host broadcasts native Gonka `MsgSetClaimRecipients`
using its native key, setting exact `(Host, E) -> DEAL_ADDRESS`. This is **not**
a Marketplace contract execute call, and the contract cannot sign on behalf of the Host.
The integrator must query the native schedule and confirm exact address before funding.

### Step 3. Buyer Deposits Exact Funds

Buyer calls `Send` on the configured settlement CW20, not `Receive` directly:

```json
{
  "send": {
    "contract": "DEAL_ADDRESS",
    "amount": "100000000",
    "msg": "eyJmdW5kIjp7fX0="
  }
}
```

`msg` is the base64 encoding of the exact hook JSON:

```json
{"fund":{}}
```

Internal generated Deal payload invoked by CW20 after Send:

```json
{
  "receive": {
    "sender": "BUYER_ADDRESS",
    "amount": "100000000",
    "msg": "eyJmdW5kIjp7fX0="
  }
}
```

Preconditions: Deal is `Open`, immediate caller is configured CW20, amount
exactly matches budget, `current < E`, native routing is exact and unique. Success:
`Funded`, event `deal_funded`; CW20 remains on Deal. Partial, excess, repeated,
or late funding are atomically rejected.

### Step 4. Keeper Locks Routing

Within the window `E <= current < E+5`:

```json
{"lock":{}}
```

Caller may be arbitrary. The contract independently queries exact Host/E routing. Success:
`Open/Funded -> Locked`, `recipient_locked=true`, event `deal_locked`. After
success, late modification/deletion of query rows and pruning do not re-evaluate Lock.
If keepers miss `E..E+4`, the contract fails closed; this constitutes a liveness incident,
not justification to fabricate historical routing.

### Step 5. Host Performs Native Claim

Strictly at `current = E+1`, Host broadcasts `MsgClaimRewards(E)` using its native key.
This is not a Marketplace execute message. WorkCoins and RewardCoins
must be routed by native Gonka to Deal address; streamvesting and unlocks are
not initiated by the contract.

### Step 6. Permissionless Settlement

Following an authoritative exact summary with `claimed=true`:

```json
{"settle_claim":{}}
```

The contract calculates amounts internally. For a funded deal:

```text
buyer_entitlement = min(work + reward, funded_capacity)
gross = floor(buyer_entitlement * price / 1_000_000_000)
fee = floor(gross * 150 / 10_000)
host_net = gross - fee
buyer_refund = deposit - gross
```

Invariant: `host_net + fee + buyer_refund = exact deposit`.
`SettleClaim` records obligations without sending CW20. Positive claims enter
`Releasing`; zero claims enter `Completed` and accrue a full Buyer refund.
No-sale creates no USDT obligations. The fee remains 150 bps.

Keepers query `{"usdt_payments":{}}` for `host`, `fee`, and `buyer`, each exposing
`recipient`, `accrued_micro_usdt`, `paid_micro_usdt`, and `pending_micro_usdt`.
Send each non-zero role in a separate confirmed transaction:

```json
{"withdraw_usdt":{"role":"host"}}
{"withdraw_usdt":{"role":"fee"}}
{"withdraw_usdt":{"role":"buyer"}}
```

Callers cannot select the amount or recipient. Continue other roles after failure;
re-query before retrying outstanding debt. Combining settlement and withdrawals
or multiple roles in one transaction couples their outcomes again.
`NothingToWithdraw` means no debt exists, including an already paid role.

GNK release follows unlock independently of USDT delivery. `Completed` concerns
the original GNK lifecycle; USDT withdrawals remain available afterward.
Coincident recipients retain separate role accounting. Unsolicited CW20 transfers
do not increase obligations and cannot be withdrawn by a keeper. Refund branches
remain atomic and cannot execute after settlement.

`claim_settled` records accrual, with `deal_completed` for zero claims.
Only successful withdrawals emit `usdt_paid` (Host/Fee) or `usdt_refunded` (Buyer).

### Step 7. Permissionless GNK Release

Once spendable `ngonka` balance appears on the Deal:

```json
{"release_unlocked_gnk":{}}
```

The contract distributes **all** available `ngonka` balance per frozen exact shares.
For `gnk_release_policy = proportional` and `total_claim > 0`:

```text
P = buyer_paid + host_paid
A = current spendable ngonka balance
U = P + A
buyer_target = floor(U * buyer_entitlement / total_claim)
host_target = U - buyer_target
delta = target - already_paid
```

Under `gnk_release_policy = host_only`, initial claim was zero, so dividing
by `total_claim` is disallowed. This policy is equivalent to Buyer/Host weights `0/1`:
`buyer_target = 0`, `host_target = U`, and all available `A` is transferred to the Host.
This occurs, for example, following zero settlement or routing refund.
The integrator must first read `gnk_release_policy` via `State`,
`Entitlements`, or `ReleaseStatus` before selecting the expected payout formula.

The sum of Buyer/Host deltas equals A. Distribution is cumulative: output does not depend on
how deposits are partitioned across release invocations. At the first `U >= original total_claim`,
status transitions to `Completed` and `deal_completed` is emitted once. This denotes
fulfillment of initial entitlement, but **not the termination of native vesting**. Late
unlocks and donations continue to be split according to the same shares; lifetime counters may
exceed original claim amounts.

The primary event `gnk_released` includes entry point, available balance, share
numerators/denominator, deltas, and lifetime totals. Zero balance returns
`NothingToRelease` without mutation and without zero transfers.

Compatible alias permitted strictly in `Completed`:

```json
{"forward_excess_gnk":{}}
```

It executes the same cumulative logic and additionally emits
`excess_gnk_forwarded`. It does not send the entire balance to the Host when a positive Buyer
share exists, and never resets counters.

## 4. Cancel and Refund

### No-Sale Cancel

```json
{"cancel":{}}
```

Permitted strictly from `Open`, without Buyer, and upon proven missing/mismatch routing. Prior to E,
caller must be Host; in `E..E+4`, caller may be arbitrary; at `E+5+`, rejected.
Success: `Cancelled`, event `deal_cancelled`, assets do not move, Factory index
remains permanently occupied. From Cancelled, funding/release/settlement are prohibited.

### Routing-Failure Refund

```json
{"refund":{}}
```

The routing path operates from `Funded` in `E..E+4`, if validated routing
response proves missing or another valid recipient. The exact
configured deposit is returned to the recorded Buyer; caller provides no amount/recipient.
Settlement-token donations, other CW20s, `ngonka`, and other denoms are excluded from
refund. Success: `Refunded`, `refund_reason` = `routing_missing` or
`routing_mismatch`, events `deal_refunded` and `usdt_refunded`, Host-only GNK
policy for late deposits.

The claim-expiry path operates from `Locked` after `current >= E+2` based on a successfully
read exact Host/E summary with `claimed=false`. With a Buyer, this is exact deposit
refund and `Refunded`; without Buyer — `Expired` without transfer. Both receive
`refund_reason = claim_expiry` and HostOnly.

After `current >= checked(E+3)`, the same `Refund {}` permits emergency
`refund_reason = network_unconfirmed` if the exact summary request returned a standard
native error/NotFound, typed unavailable route, or response cannot be safely
decoded/validated by size/nested summary/Host/E identity. This represents deliberate
risk allocation: even if a claim actually occurred, Buyer receives full deposit,
Host/platform receives 0 USDT, and Host receives 100% of current and future GNK.
`claimed=true` always blocks Refund. Errors in GetCurrentEpoch, request encoding,
deadline overflow, state/accounting, or unexpected system errors fail closed.
Each call re-queries summary anew; prior errors are not persisted.

Example of terminal fields on funded Deal after emergency refund:

```json
{
  "status": "refunded",
  "refund_reason": "network_unconfirmed",
  "buyer": "BUYER_ADDRESS",
  "recipient_locked": true,
  "gnk_release_policy": "host_only",
  "gross_usdt": "0",
  "fee_usdt": "0",
  "host_net_usdt": "0",
  "buyer_refund_usdt": "100000000"
}
```

This is an abbreviated snippet: actual `State` additionally contains all zero claim/
entitlement fields and lifetime release counters from the generated schema.

## 5. Query JSON and Fields

Factory:

```json
{"config":{}}
```

```json
{"deal":{"id":1}}
```

```json
{"deal_by_host_epoch":{"host":"HOST_ADDRESS","epoch":123}}
```

```json
{"list_deals":{"start_after":null,"limit":30}}
```

`ListDeals` uses exclusive `start_after`; server-side limit is bounded.

Deal:

```json
{"config":{}}
```

```json
{"state":{}}
```

```json
{"funding":{}}
```

```json
{"entitlements":{}}
```

```json
{"release_status":{}}
```

```json
{"native_status":{}}
```

- `Config`: immutable terms, capacity, addresses, and pinned SHA.
- `State`: status, Buyer, lock proof, claim components, entitlements, release
  policy, lifetime counters, and USDT accounting.
- `Funding`: configured budget/capacity and presence of recorded Buyer.
- `Entitlements`: original frozen claim ownership and policy.
- `ReleaseStatus`: lifetime paid counters and diagnostic original remaining,
  clamped to zero; remaining is not a payout cap.
- `NativeStatus`: diagnostic remaining vesting + liquid balance. Vesting
  query errors may break this convenience query, but never bank-only release.

Example response numerical fields:

```json
{
  "status": "completed",
  "refund_reason": null,
  "buyer": "BUYER_ADDRESS",
  "recipient_locked": true,
  "work_ngonka": "80000000000",
  "reward_ngonka": "20000000000",
  "total_claim_ngonka": "100000000000",
  "buyer_entitlement_ngonka": "80000000000",
  "host_entitlement_ngonka": "20000000000",
  "gnk_release_policy": {
    "proportional": {
      "buyer_share_numerator": "80000000000",
      "share_denominator": "100000000000"
    }
  },
  "released_total_ngonka": "160000000000",
  "buyer_released_ngonka": "128000000000",
  "host_released_ngonka": "32000000000",
  "gross_usdt": "80000000",
  "fee_usdt": "1200000",
  "host_net_usdt": "78800000",
  "buyer_refund_usdt": "0"
}
```

## 6. Events and Indexing

Contract emits event types: `offer_created`, `deal_funded`, `deal_locked`,
`claim_settled`, `usdt_paid`, `usdt_refunded`, `gnk_released`,
`deal_completed`, `deal_refunded`, `deal_expired`, `deal_cancelled`, and
`excess_gnk_forwarded`. Chain/indexer may expose custom types with the prefix
`wasm-`. For refund/expired, index typed `reason`: `claim_expiry` and
`network_unconfirmed` hold different evidentiary semantics.

Source of truth after a tx is successful tx receipt **and subsequent state/ledger queries**,
not a single event. Failed transactions must not be indexed as completed
transitions. For every monetary event, reconcile recipient, amount, Deal, Host, and epoch.

## 7. Retry Policy

| Error | Safe Action |
|---|---|
| Native query/decode/identity failure | Emit nothing; check node/route and retry identical deterministic execute |
| CW20/Bank outgoing transfer failure | State and preceding transfers must roll back; resolve underlying issue and retry |
| `NothingToRelease` / `NothingToForward` | Await fresh spendable `ngonka`; retrying now is a no-op |
| Wrong state after successful tx | Do not retry terminal transitions; query state first |
| Lock window missed | Halt automated settlement/refund assumptions and raise liveness incident |
| Claim expiry from Locked | From E+2, exact unclaimed summary yields ClaimExpiry; from E+3 permitted current error yields NetworkUnconfirmed; claimed requires SettleClaim |

Keepers must prioritize Lock from the onset of E, followed by Settle/Release and
proven Refund. Keepers are permissionless and possess no authority to alter amounts or
recipients. Keepers never sign native Host messages. Keeper implementation is external to A8.

## 8. Contract-Only Reproduction

From repository root:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/generate-proto.ps1
powershell -ExecutionPolicy Bypass -File scripts/generate-schema.ps1
cargo fmt --all --check
cargo fmt --manifest-path tools/proto-gen/Cargo.toml -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo clippy --manifest-path tools/proto-gen/Cargo.toml --all-targets --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo test --manifest-path tools/proto-gen/Cargo.toml --locked
cargo build -p marketplace-factory -p marketplace-deal --release --target wasm32-unknown-unknown --locked
cosmwasm-check target/wasm32-unknown-unknown/release/marketplace_factory.wasm target/wasm32-unknown-unknown/release/marketplace_deal.wasm
cargo audit
cargo audit --file tools/proto-gen/Cargo.lock
cargo deny check
cargo deny --manifest-path tools/proto-gen/Cargo.toml --config deny.toml --locked check
```

End-to-end ledger suite execution:

```powershell
cargo test -p marketplace-factory --test integration --locked
```

These commands do not broadcast transactions to Gonka and do not close native gates.

The preparatory A9 scenario additionally performs two independent pinned
optimizer builds, writes/verifies the build manifest, and isolates read-only dry-runs from
explicit deployment:

```powershell
python scripts/a9_release.py build --commit HEAD --output artifacts/a9-local
python scripts/a9_release.py verify-artifacts --manifest artifacts/a9-local/build-manifest.json
python -m unittest discover -s scripts/tests -p 'test_*.py' -v
```

Without running Docker, the first command must fail; standard release builds
above do not verify reproducible optimizer outputs.

## 9. Golden E2E Handoff for Developer B

Status 2026-09-08: A funded positive-claim slice and P0 have been executed on
local Gonka on the exact runtime SHA. Factory/Deal deployed without admin, native
ClaimRewards and SettleClaim succeeded, two vesting tranches brought the Deal to Completed,
and late donations were distributed according to frozen shares. Four Marketplace
gRPC routes are accessible from Wasm; broad routes are prohibited. Exact tx/state/balance
evidence is recorded in [A8/B acceptance report](reviews/a8-b-acceptance-report.md).

Reproducible runs require a clean Marketplace checkout and a clean Gonka checkout
at the selected commit. **The Gonka checkout is never modified**: the Marketplace
Kotlin scenarios, Compose fragments, the B3 genesis provisioner, the API container
controller and the Go/Wasm probes live on our side under `ops/a8/harness/`.

When executing `a8_acceptance.py run-live`, the harness:
1. Verifies that both checkouts are exactly their selected commits (HEAD, tree, tracked bytes, submodules, no added or ignored files).
2. Checks that every upstream Testermint API the scenarios need exists, before creating any network.
3. Builds the selected Gonka's unmodified Testermint and the external harness out-of-tree under `--work-root`, and prepares a separate network work directory (`GONKA_REPO_ROOT`) with byte-identical copies of the upstream network resources.
4. Runs the selected scenario, collects JUnit and logs from the external project, and re-verifies both checkouts; any change fails the run (`source-immutability.json`).

In practice run it through the E2E runner (`ops/e2e/run-e2e.sh`); see
[`ops/e2e/RUNBOOK-immutable-sources.md`](../ops/e2e/RUNBOOK-immutable-sources.md).
The direct invocation below is kept for reference:

```powershell
git clone https://github.com/gonka-ai/gonka.git ..\gonka-clean
python scripts/a8_acceptance.py run-live `
  --gonka-dir ..\gonka-clean `
  --expected-gonka-sha <GONKA_FULL_40_HEX_SHA> `
  --manifest artifacts/a9-local/build-manifest.json `
  --run-id a8-funded-local --timeout-minutes 60
```

If `--manifest` is omitted, harness performs two pinned A9 optimizer builds first.
Presence of legacy `.wasm` is not accepted as evidence: stale commits, artifact
hashes, or runtime versions terminate the run prior to deployment.

In `claim_settle`, expected amounts are not sourced from Deal state. An independent oracle
first validates epoch, Host, `claimed`, Work/Reward from native summary and exact
offer terms, calculates GNK entitlement, gross, fee, Host net, and Buyer refund internally,
and only then compares them against state and actual CW20 balance deltas. Consequently,
conservation-preserving substitutions such as `fee=0, Host=entire budget` are rejected.

This resolves items 1, 3, and positive portions of 5/7 strictly for the verified
configuration. The remaining items below remain release gates.

Minimum real-chain scenarios and expected outcomes:

1. Factory/Deal deploy without admin; all four gRPC routes accessible from Wasm.
2. Contract address accepted as exact recipient; changing E after onset is
   rejected; pruning boundary measured on real keeper.
3. Exact CW20 funding → Lock → real positive claim: summary fields match
   Work/Reward payout; USDT conservation converges.
4. Under/exact/overproduction and no-sale: expected entitlements, fee/refund, and
   GNK recipients match contract-only matrix.
5. Streamvesting: locked total decreases, spendable Deal bank balance increments
   by identical tranche; no implicit contract execute occurs.
6. Multiple Deals for Host/epoch: schedules, bank balances, and payouts do not
   aggregate across addresses.
7. Multiple releases up to T, single Completed event, late liquid and vested gifts;
   `ReleaseUnlockedGnk` and alias maintain lifetime shares at U > T.
8. Payment failure on each native leg and claimed write fault: full atomicity,
   successful retry without double payout.
9. Routing missing/mismatch in `E..E+4`: exact deposit refund; E+5+ fail closed.
10. Disabled/malformed route, decode mismatch, and storage failure: neither state
    nor funds mutate.
11. Re-verify Refund/Expired on real Gonka: E+1 reject, E+2 unclaimed success,
    E+2 summary error reject, E+3/late permitted error matrix success,
    claimed=true reject before and after E+3; restored summary overrides only
    unfinalized attempts.
12. Under constrained gas, execute Refund directly and via contract/submessage:
    healthy summary queries must not degrade into successful emergency outcomes.
13. Verify running `wasmd`/`wasmvm` versions and remediation of all applicable
    advisories, including CWA-2025-007.

Optional follow-up: the B2 typed-absence enhancement. It is not itself a production
gate. Production gates still open are B3 gas/pruning bounds; B4 claimed atomicity
and native gas traces; the remaining B5 matrix; B6 keeper operations; and validated
production parameters. The pinned target uses `wasmd v0.54.2`, which is affected
by [CWA-2025-007](https://github.com/CosmWasm/advisories/blob/main/CWAs/CWA-2025-007.md);
the advisory lists `v0.54.3` as patched and says the fix requires a coordinated,
consensus-breaking chain upgrade. Verify the actual Gonka runtime and its upgrade
before production; changing only the contract repository's dependency declaration
does not remediate the running chain. While any mandatory gate remains open,
full A8/MVP is not production-ready.
