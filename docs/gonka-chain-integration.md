# Gonka Integration for Forward Marketplace Dual-Escrow MVP

Workstream: Developer B
Status: Source-verified design, real-chain P0 pending
Pinned Gonka SHA: `379bebced638aeb5e6077bfd51c986f898443832`

## 1. Integration Analysis Summary

| Question | Result |
|---|---|
| Contract address as claim recipient | Source path does not prohibit; real-chain proof mandatory |
| WorkCoins and RewardCoins to single recipient | `VERIFIED FROM SOURCE` |
| Streamvesting keyed by contract-capable address type | `VERIFIED FROM SOURCE` as `sdk.AccAddress`; real contract E2E pending |
| Unlock via bank keeper | `VERIFIED FROM SOURCE`; spendable contract balance E2E pending |
| Single global marketplace recipient | `NO-GO`, schedules aggregate by address |
| Gonka business logic change | Not required |
| Core patch | Four read-only entries in `AcceptedGrpcQueries()` |

Architectural verdict: `GO WITH CAVEATS`. Economic model `RewardCoins + WorkCoins` — `GO`. Production gate — full path verified on real local Gonka.

## 2. Source of Truth

Developer B must start with:

```bash
git clone https://github.com/gonka-ai/gonka.git
cd gonka
git checkout 379bebced638aeb5e6077bfd51c986f898443832
git rev-parse HEAD
```

Expected full SHA:

```text
379bebced638aeb5e6077bfd51c986f898443832
```

Chain binary, protobuf generation, Wasm build manifest, Docker image, and Testermint reports must embed this exact SHA. Migrating to another commit requires a full source review of critical paths.

## 3. Native Claim Recipient Path

### 3.1 `SetClaimRecipients`

Source:

```text
inference-chain/x/inference/keeper/msg_server_set_claim_recipients.go
```

Functions:

- `SetClaimRecipients`;
- `applyClaimRecipientEntry`.

Verified semantics:

- Message requires Host `ParticipantPermission`;
- Caller/creator parsed as `sdk.AccAddress`;
- Target epoch must be strictly greater than current;
- Maximum lookahead is 40;
- Non-empty recipient is validated strictly via `sdk.AccAddressFromBech32`;
- Recipient is not checked as Participant, signer, or EOA;
- Batch applies atomically in `CacheContext`.

Source conclusion: CosmWasm contract address is not excluded by validation logic. Real-chain conclusion is pending until P0 spike.

### 3.2 Storage and Immutability

Source:

```text
inference-chain/x/inference/keeper/claim_recipient_schedule.go
```

Functions:

- `SetClaimRecipientForEpoch`;
- `GetClaimRecipientForEpoch`;
- `ResolveClaimRecipientAddress`;
- `RemoveClaimRecipientForEpoch`;
- `GetClaimRecipientsByParticipant`.

Mapping is stored by `(Host sdk.AccAddress, epoch)`. Once E becomes current, the setter rejects modifications for E. Therefore, exact `(Host, E) -> DealContract` is immutable routing proof while the record remains accessible to queries.

`inference-chain/x/inference/keeper/pruning.go` specifies `ClaimRecipientPruningThreshold = 5` and a maximum of 1,000 deletions per block. Keepers must nonetheless establish `LOCKED` at the start of E: E2E must validate the actual deletion point considering pruning state and backlog, rather than treating a constant as a UI guarantee. After potential pruning, an empty query result is ambiguous and confers no right to refund or settlement.

## 4. Native Claim Path

Source:

```text
inference-chain/x/inference/keeper/msg_server_claim_rewards.go
```

Key functions:

- `ClaimRewards`;
- `validateRequest`;
- `payoutClaim`;
- `resolvePayoutAddress`;
- `finishSettle`.

### 4.1 Claim Window

`validateRequest` accepts strictly:

```text
msg.epoch_index == current_epoch - 1
```

For target E:

```text
current = E+1  -> claim allowed
current >= E+2 -> claim rejected
```

### 4.2 Single Recipient for Both Components

`payoutClaim` invokes `resolvePayoutAddress(Host, E)` once, then:

```text
WorkCoins   -> PayParticipantFromEscrow(payoutAddress, ...)
RewardCoins -> PayParticipantFromModule(payoutAddress, ...)
```

Both components route to the Deal Contract. Native Gonka does not split them between Buyer and Host; the split is performed by the Deal Contract upon unlocking.

### 4.3 Atomicity

`payoutClaim` utilizes nested `CacheContext`:

1. Resolve recipient.
2. Pay/vest WorkCoins.
3. Pay/vest RewardCoins.
4. `finishSettle`.
5. Commit.

Failure of any payout does not record a partial claim. This must be re-verified via real-chain retry tests.

### 4.4 `claimed=true` Consistency

`finishSettle` deletes the settle amount and persists `EpochPerformanceSummary.claimed = true` within the same payout `CacheContext`. Returned errors are logged, but exact source review indicates: scalar protobuf marshal returns nil, `collections.Map.Set` calls runtime `coreKVStore.Set`, which returns nil after underlying Set; store failures manifest as panics. For a properly preserved summary, a silent returned-error branch has not been proven reachable. Native payment-failure and happy-path tests cover rollback/claimed; real-chain retries remain mandatory.

### 4.5 Zero Claim

If `WorkCoins + RewardCoins == 0`, a summary with `claimed=false` is retained, and no settle amount is created; a manually constructed zero-settle `ClaimRewards` is rejected prior to payout. After E+2, Marketplace executes refund for funded Buyers or Expired for no-sale deals strictly upon a successfully retrieved exact summary.

If one component is zero while the other is positive, the claim can succeed, and total claim equals the positive component.

## 5. Authoritative Claim Data

Source:

```text
inference-chain/x/inference/keeper/accountsettle.go
inference-chain/proto/inference/inference/epoch_performance_summary.proto
```

Mapping:

```text
EpochPerformanceSummary.earned_coins   = Settle.WorkCoins
EpochPerformanceSummary.rewarded_coins = Settle.RewardCoins
```

Marketplace calculation:

```text
total_claim_ngonka = checked_add(earned_coins, rewarded_coins)
```

Native fields are not renamed. The term `total claim GNK` exists only on the marketplace layer.

Base denom confirmed in:

```text
inference-chain/x/inference/types/coin.go
inference-chain/denom.json
```

```text
BaseCoin = ngonka
1 GNK = 1_000_000_000 ngonka
```

## 6. Streamvesting Path

Source:

```text
inference-chain/x/inference/keeper/payment_handler.go
inference-chain/x/streamvesting/keeper/keeper.go
inference-chain/proto/inference/streamvesting/vesting_schedule.proto
```

### 6.1 Claim to Vesting

`PayParticipantFromModule`:

- If vesting period > 0, invokes `AddVestedRewards`;
- If period = 0, performs direct module-to-account transfer.

`AddVestedRewards`:

- Transfers funds to streamvesting module account;
- Parses recipient as `sdk.AccAddress`;
- Stores `VestingSchedule` by recipient address;
- Aggregates new amounts into existing schedule;
- Splits amount across epochs, appending remainder to first tranche.

### 6.2 Unlock

`AdvanceEpoch` invokes `ProcessEpochUnlocks`. For each schedule:

1. Takes first `EpochAmounts` tranche.
2. Executes `SendCoinsFromModuleToAccount(streamvesting, recipient, tranche)`.
3. Removes first tranche.
4. Removes empty schedule.

Native bank send contains no Wasm callback. Expected runtime behavior: contract balance increases, but execute is not triggered.

### 6.3 Different Periods

Fields:

```text
TokenomicsParams.WorkVestingPeriod
TokenomicsParams.RewardVestingPeriod
```

Production genesis in pinned repository sets 180/180; runtime governance/upgrades may alter parameters. Marketplace does not pin them into pricing and does not split ownership by coin type.

### 6.4 Why Per-Deal Address Is Mandatory

`VestingSchedules` is declared as:

```go
collections.Map[sdk.AccAddress, types.VestingSchedule]
```

The schedule contains no Host, epoch, or deal ID. Two claims to a single recipient address aggregate. Therefore, golden E2E must deploy an isolated Deal Contract for every deal and verify absence of address reuse.

## 7. CosmWasm Boundary

Pinned app uses:

```text
github.com/CosmWasm/wasmd v0.54.2
github.com/CosmWasm/wasmvm/v2 v2.2.4
```

Source:

```text
inference-chain/go.mod
inference-chain/app/app.go
inference-chain/app/legacy.go
```

Source-positive indicators:

- Wasm contract addresses are used as `sdk.AccAddress` in app code;
- Existing Gonka contracts send native tokens via `BankMsg::Send`;
- Recipient validation and streamvesting require no recipient account pubkey/signature;
- Bank unlocks do not trigger contract execution.

However, only real-chain E2E can elevate these inferences to `VERIFIED BY LOCAL E2E`.

## 8. Minimal Production Patch

File:

```text
inference-chain/app/legacy.go
```

Add four read-only entries to `AcceptedGrpcQueries()`:

```go
func AcceptedGrpcQueries() wasmkeeper.AcceptedQueries {
    return wasmkeeper.AcceptedQueries{
        "/inference.inference.Query/ApprovedTokensForTrade": func() proto.Message {
            return &inferencetypes.QueryApprovedTokensForTradeResponse{}
        },
        "/inference.inference.Query/ValidateWrappedTokenForTrade": func() proto.Message {
            return &inferencetypes.QueryValidateWrappedTokenForTradeResponse{}
        },
        "/inference.inference.Query/ValidateIbcTokenForTrade": func() proto.Message {
            return &inferencetypes.QueryValidateIbcTokenForTradeResponse{}
        },
        "/inference.inference.Query/GetCurrentEpoch": func() proto.Message {
            return &inferencetypes.QueryGetCurrentEpochResponse{}
        },
        "/inference.inference.Query/ListClaimRecipients": func() proto.Message {
            return &inferencetypes.QueryListClaimRecipientsResponse{}
        },
        "/inference.inference.Query/EpochPerformanceSummaryByParticipant": func() proto.Message {
            return &inferencetypes.QueryEpochPerformanceSummaryByParticipantResponse{}
        },
        "/inference.streamvesting.Query/TotalVestingAmount": func() proto.Message {
            return &streamvestingtypes.QueryTotalVestingAmountResponse{}
        },
    }
}
```

Add import alias for:

```text
github.com/productscience/inference/x/streamvesting/types
```

Do not modify:

- `AcceptedStargateQueries()`;
- Protobuf definitions;
- Reward calculations;
- Claim recipient storage;
- `ClaimRewards`;
- Streamvesting writes/unlocks;
- Bank/authz/governance;
- Marketplace-specific native state.

Standard CosmWasm `BankQuery::Balance` requires no custom Gonka allowlist entry.

## 9. Why `TotalVestingAmount` Is Needed

Source:

```text
inference-chain/x/streamvesting/keeper/query.go::TotalVestingAmount
inference-chain/proto/inference/streamvesting/query.proto
```

The query sums all remaining `EpochAmounts` for the address. Under the new [ADR-0010](decisions/0010-permanent-gnk-shares.md), this value remains diagnostic and does not limit release. The previous implementation's formula `T - min(T, remaining)` has been superseded: all available ngonka are distributed according to frozen shares. New schedules aggregate with existing tranches without shifting the existing schedule. Bank balance, spendability, and actual unlocks still require E2E validation.

The E2E scenarios below follow ADR-0010: they expect preservation of shares, U > T, and continuation of releases after Completed, rather than delays from increasing remaining vesting or Host-only excess when positive Buyer shares exist.

## 10. Query Contract Between A and B

| Query | Request | Contract Checks |
|---|---|---|
| `GetCurrentEpoch` | empty | Response exists; epoch monotonic assumption is not shared across transactions |
| `ListClaimRecipients` | `participant = Host` | Exactly one target entry; recipient = Deal |
| `EpochPerformanceSummaryByParticipant` | `epoch_index = E`, `participant_id = Host` | Exact IDs, `claimed`, checked earned + rewarded |
| `TotalVestingAmount` | `participant_address = Deal` | Diagnostic for `NativeStatus`: strictly `ngonka`; malformed/duplicate denom rejects query, but not bank-only release |

Generated Rust protobuf types must be compiled from the pinned proto tree. Handwritten field layouts are prohibited.

## 11. Source-Level Tests for Patch

Add Go tests:

```text
TestAcceptedGrpcQueriesForwardMarketplaceV2
TestAcceptedGrpcQueriesResponseConstructors
TestAcceptedGrpcQueriesDoesNotExposeBroadInferenceQueries
TestAcceptedGrpcQueriesDoesNotExposeFullVestingSchedule
```

Verify presence of exactly the new target routes and absence of:

```text
/inference.inference.Query/EpochPerformanceSummary
/inference.inference.Query/EpochPerformanceSummaryAll
/inference.inference.Query/Params
/inference.streamvesting.Query/VestingSchedule
/inference.streamvesting.Query/Params
```

Existing wrapped-token routes must remain unchanged.

## 12. Local Environment

Requirements:

- Clean checkout of pinned SHA;
- Chain binary with target allowlist patch;
- x/wasm active;
- Host cold key, Buyer, fee recipient, and keeper accounts;
- Factory and Deal Wasm without admin;
- Standard mock CW20 USDT, `decimals = 6`;
- Controllable epochs E, E+1, E+2;
- Capability to generate real WorkCoins and RewardCoins;
- Logs, tx responses, query snapshots, and balances persisted upon failure.

Ethereum bridge is excluded from golden test.

The current source review work environment does not contain Go, Rust, or Docker; therefore, the following represents mandatory spike specifications, not assertions of completed E2E runs.

## 13. P0 Real-Chain Feasibility Spike

### 13.1 Setup

1. Build chain on pinned SHA.
2. Apply four-entry allowlist patch.
3. Start clean local Gonka.
4. Deploy minimal mock CW20.
5. Deploy Deal code and Factory without admin.

### 13.2 Contract Recipient

6. Host creates offer E; Factory returns Deal address.
7. Host cold key broadcasts `SetClaimRecipients(E, Deal)`.
8. Query schedule and assert exact address.
9. Negative control: invalid Bech32 rejected.
10. Negative control: attempt to alter E after E begins rejected.

### 13.3 Real Claim

11. Buyer funds exact CW20 budget before E.
12. Advance to E and call permissionless `Lock`.
13. Generate real inference workload so Host summary has controlled positive fields.
14. Advance to E+1.
15. Host broadcasts real `ClaimRewards(E)`.
16. Assert tx success and `claimed=true`.
17. Assert summary fields exactly match native settlement.
18. Assert WorkCoins + RewardCoins committed to Deal recipient.

### 13.4 Vesting and Bank Balance

19. Query `TotalVestingAmount(Deal)` immediately after claim.
20. Assert expected positive locked amount when periods > 0.
21. Record Deal bank balance.
22. Advance one epoch.
23. Assert vesting total decreases by first tranche.
24. Assert Deal spendable `ngonka` balance increases by the same amount.
25. Confirm no contract execute event occurred solely from unlock.

### 13.5 Permissionless Split

26. Third-party keeper calls `ReleaseUnlockedGnk`.
27. Assert Buyer and Host bank balances increase by cumulative deltas.
28. Repeat release in the same epoch; assert no double transfer.
29. Advance through all tranches and release each time.
30. Assert exact original entitlements at U = T and single `Completed` event.
31. Add liquid/vested gift, await spendable balance, and call release after Completed.
32. Assert lifetime targets `floor(U*B/T)` / remainder at U > T; no duplicate completion event. Calling `ForwardExcessGnk` yields identical split.

### 13.6 Acceptance Gate

Architecture receives `GO` only if steps 7, 15, 19, 24, 26, 30, and 32 pass on real keepers of the pinned chain. Any failure is documented with exact path, tx response, logs, and minimal required native change.

## 14. Economic E2E Matrix

If exact chain rewards are difficult to generate through natural workload, Developer B must implement deterministic integration fixtures on real keepers, without substituting mocks.

| Case | Reward | Work | Capacity | Buyer | Host |
|---|---:|---:|---:|---:|---:|
| Underproduction | 50 | 2 | 100 | 52 | 0 |
| Exact | 90 | 10 | 100 | 100 | 0 |
| 50/50 | 100 | 100 | 100 | 100 | 100 |
| Reward-heavy | 99 | 1 | 100 | 100 | 0 |
| Work-heavy | 10 | 90 | 100 | 100 | 0 |
| Massive overproduction | 50,000 | 50,000 | 100 | 100 | 99,900 |
| Zero | 0 | 0 | 100 | 0 | 0 |

Each case validates USDT gross/fee/refund and cumulative GNK release.

## 15. Security and Liveness Tests

### 15.1 Routing

- Recipient missing.
- Recipient = Buyer instead of Deal.
- Recipient = another contract.
- Host changes recipient before E.
- Change/delete after E rejected.
- Lock called after pruning: contract fails closed and does not interpret row absence as routing mismatch.

### 15.2 Claim

- Host does not mine.
- Host refuses to claim.
- Claim fails and retry succeeds.
- Claim rate limit.
- E+1 missed, E+2 rejected.
- Work payment failure rolls back Reward payment.
- Reward payment failure rolls back Work payment.
- `claimed` write-error semantics.

### 15.3 Vesting

- Work and Reward periods equal.
- Periods different.
- One period zero, the other positive.
- Remainder in first tranche.
- 180-epoch completion.
- Empty schedule after final unlock.
- Multiple Deal addresses remain isolated.

### 15.4 Contamination

- Liquid `ngonka` donation before claim.
- Liquid donation during vesting.
- Vested donation via `MsgTransferWithVesting`.
- Unrelated denom transfer.
- Another Host routes a claim to the same Deal address.

Expected invariant: All spendable `ngonka`, regardless of source, enter U and are split per frozen shares; lifetime payouts may exceed authoritative claims. New vesting schedules must not reschedule old tranches. Errors in diagnostic vesting queries do not block release of available bank balance.

### 15.5 Contract Boundary

- Disabled allowlist route fails closed.
- Wrong response constructor/decode fails.
- Bank unlock does not trigger execute.
- Permissionless caller cannot influence recipients/amounts.
- Repeated release/settle/refund does not move funds.

## 16. Testermint Deliverable

A single command must:

1. Spin up a clean network.
2. Deploy CW20, Factory, and Deal code.
3. Create Host/epoch offer.
4. Configure native routing.
5. Fund Buyer.
6. Progress through E/E+1 claim.
7. Settle USDT.
8. Progress through at least two unlock tranches.
9. Verify Buyer/Host split.
10. Execute repeat-call negatives.
11. Persist report with SHA, code checksums, addresses, tx hashes, and balance deltas.
12. Teardown and clean up network idempotently.

The golden test cannot substitute mock implementations for `SetClaimRecipients`, `ClaimRewards`, streamvesting, or bank keeper.

## 17. Knowledge Base

| Topic | Source Path | Function / Type | Usage |
|---|---|---|---|
| Recipient setter | `x/inference/keeper/msg_server_set_claim_recipients.go` | `SetClaimRecipients`, `applyClaimRecipientEntry` | Exact future routing |
| Recipient storage | `x/inference/keeper/claim_recipient_schedule.go` | `ResolveClaimRecipientAddress` | Claim destination |
| Recipient query | `x/inference/keeper/query_list_claim_recipients.go` | `ListClaimRecipients` | Deal lock proof |
| Claim | `x/inference/keeper/msg_server_claim_rewards.go` | `payoutClaim` | Single recipient, both coin types |
| Claim window | Same file | `validateRequest` | E+1 only |
| Claim marker | Same file | `finishSettle` | `claimed=true` caveat |
| Summary creation | `x/inference/keeper/accountsettle.go` | `SettleAccounts` | earned/rewarded mapping |
| Payment routing | `x/inference/keeper/payment_handler.go` | `PayParticipantFromModule` | Direct vs vesting |
| Vesting storage | `x/streamvesting/keeper/keeper.go` | `AddVestedRewards` | Address aggregation |
| Unlock | Same file | `ProcessEpochUnlocks` | Module-to-account transfer |
| Vesting total query | `x/streamvesting/keeper/query.go` | `TotalVestingAmount` | Schedule diagnostics; not release cap |
| Base units | `x/inference/types/coin.go`, `denom.json` | `BaseCoin` | `ngonka`, 10^9 |
| Wasm allowlist | `app/legacy.go` | `AcceptedGrpcQueries` | Sole production patch |
| Native BankMsg example | `contracts/community-sale/src/contract.rs` | native sends | Pattern to adapt |
| Native balance example | `contracts/liquidity-pool/src/contract.rs` | bank query/send | Pattern to adapt |

## 18. Proposed Upstream PR

Scope:

```text
1 production file: inference-chain/app/legacy.go
1 focused test file: app/legacy_forward_marketplace_test.go
4 new AcceptedGrpcQueries registrations
0 protobuf changes
0 state changes
0 reward/claim/vesting changes
```

The PR description must explicitly note that routes are read-only and consumed by immutable per-deal CosmWasm contracts.

## 19. Definition of Done

| Requirement | Complete When |
|---|---|
| SHA pinned | All artifacts display exact full SHA |
| Patch minimal | Diff contains only allowlist/import/test changes |
| Four queries work from Wasm | Real contract decodes valid responses |
| Contract recipient accepted | Real `SetClaimRecipients` tx succeeds |
| Both coin types routed | Claim creates Deal entitlement for Work + Reward |
| Vesting keyed by Deal | Query displays schedule/total for Deal address |
| Unlock spendable | Deal bank balance increments by real tranche |
| No implicit execute | Unlock does not trigger contract entry point |
| Permissionless split works | Buyer/Host balances match cumulative formula |
| Permanent-share exactness | At U = T, original entitlements satisfied exactly; at U > T, identical share continues |
| Donation behavior known | Liquid and late vested donations enter U; new schedule does not reschedule old tranches |
| Failures fail closed | Disabled query, NotFound, and decode errors do not move funds |
| Repeat-safe | No double USDT/GNK/refund |
| No bridge dependency | Golden test uses mock CW20 |
| Reproducible | Single command in clean environment |

## 20. Remaining Blockers

Native integration requirements and their reassessment are recorded in [ADR-0011](decisions/0011-routing-refund-and-claim-expiry-gates.md): query errors must not be treated as proof of claim absence; summary retention, payout/claimed atomicity, and the exact production runtime require verification. [ADR-0013](decisions/0013-network-unconfirmed-emergency-refund.md) defines the raw-query error classification and the deliberate emergency policy from E+3; a separate typed-absence API is not a prerequisite for that policy. The original scope in §§8/18 ("allowlist only, 0 protobuf changes") does not establish these guarantees; any native API changes require separate agreement. [Deployment tooling](deployment-tooling.md) defines the production evidence gates, and the [integration guide](integration-guide.md) documents external calls needed for timely Lock and subsequent payouts. Keeper implementation is outside current contract work.

Non-blocking question to Gonka core team: Is `claimed=true` possible when `earned_coins + rewarded_coins == 0`, and does it definitively guarantee a final zero outcome without positive payout or subsequent claim for Host/E? [ADR-0011](decisions/0011-routing-refund-and-claim-expiry-gates.md) records that pinned `validateRequest` rejects zero claims and that the contract's defensive zero-settlement branch is not proof of native reachability. A core-team response is supplemental confirmation, not a contract-side gate; exact production runtime and real-chain verification remain release requirements. Under the current [settlement flow](integration-guide.md) and [ADR-0014](decisions/0014-independent-usdt-withdrawals.md), a validated claimed zero summary completes the deal and accrues a full Buyer refund, paid separately through `WithdrawUsdt`; a no-sale creates no USDT debt. The inquiry has not been submitted and no core-team response is recorded.

1. Full local E2E not executed in current environment.
2. Core team must confirm contract recipient support as stable semantics.
3. E2E confirmation needed that existing vesting schedules remain independent of new schedules and bank spendability.
4. Re-verify payment/claimed atomicity on exact production binary; source review did not prove reachable an isolated returned-error branch when writing summary.
5. Confirm actual deletion timing at threshold 5 and potential pruning backlog via E2E.
6. Production SHA, CW20 USDT address, and fee recipient required.

Contract-side routing-failure Refund is implemented without protobuf changes: it reuses successful `ListClaimRecipients` bounded to `E..E+4`. Claim-expiry Refund/Expired are implemented from E+2 based on exact `claimed=false` summary, and from E+3 as emergency NetworkUnconfirmed based on explicit raw-query error matrix ADR-0013. This does not resolve allowlist/B3/E2E; mocks validate Rust behavior, not production routing, gas, or native truth. Typed authoritative B2 outcomes remain useful for diagnostics, but no longer block agreed emergency contract policy.

Until items 1–4 are closed, Developer B may implement patch and harness, but architecture remains `GO WITH CAVEATS`.

## 21. A8 Handoff for Developer B

Contract-only A8 does not modify the native API. Current execute/query JSON payloads, sequences for Host/Buyer/permissionless callers, retry policies, golden E2E scenarios, and expected outcomes are detailed in the [integration guide](integration-guide.md). Mapping of requirements against contract tests and open native dependencies is documented in [A8 gap analysis](reviews/a8-contract-gap-analysis.md).

New multi-deal `cw-multi-test` validates isolation across three Factory-created Deals for distinct Host/epoch pairs on mocked protobuf boundaries and real CW20/bank ledgers. B5 must reproduce this property with actual distinct streamvesting schedules; contract-only results do not prove native address aggregation semantics.

As of 2026-09-07, the official CosmWasm advisory index continues to make CWA-2025-007 applicable to pinned `wasmd v0.54.2`. CWA-2026-001 lists other exact affected patch versions, while CWA-2026-002…006 remain published as placeholders without scope. B4 must inspect the running binary and repeat assessment upon publication of details; contract-level Rust supply-chain checks do not clear this Go/runtime gate.
