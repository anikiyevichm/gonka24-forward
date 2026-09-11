# ADR-0011: Routing Refund and Positive Claim-Expiry Proof

- Date: 2026-09-07.
- Status: Accepted; error policy for Locked deals is extended by ADR-0013.
- Contract base: `origin/main@606c2a1`.
- Pinned protobuf source: Gonka `379bebced638aeb5e6077bfd51c986f898443832`.
- Assessed chain PR head: `042758f4aa911606d34fc90fc1f3f257c09d55b8`.

## Decision

The existing permissionless `Refund {}` entrypoint supports two independent proof paths:

1. Routing failure: `Funded -> Refunded` at `E <= current < E+5` upon a successfully validated `ListClaimRecipients` query demonstrating missing or mismatched recipient. This previously implemented branch is unchanged.
2. Claim expiry: strictly from `Locked`, at `current >= checked(E+2)`, upon a successfully retrieved performance summary with exact immutable `Host/E` and `claimed=false`.

Claim expiry with a Buyer transitions the Deal to `Refunded`, records `RefundReason::ClaimExpiry`, refunds exactly the configured deposit to the recorded Buyer, and sets `GnkReleasePolicy::HostOnly`. Claim expiry without a Buyer transitions the Deal to `Expired` with the same typed reason and `HostOnly`, but creates no transfer messages.

Prior to the emergency deadline, query errors, missing optional fields, malformed/default/oversized responses, and identity mismatches reject the operation. Starting at `checked(E+3)`, a narrowly classified subset of these errors permits a terminal `NetworkUnconfirmed` refund per ADR-0013. `claimed=true` and inconsistent Locked accounting reject Refund in all cases. The text of gRPC error messages is not parsed. A separate `Expire` execute handler is not added; Open/Funded deals lacking a successful Lock snapshot do not receive an emergency transition.

## Review of Earlier Gates

The previous revision blocked all claim-expiry handling due to four broader questions. A re-review separated these concerns into those applicable to positive `claimed=false` proof and those applicable solely to absent summaries.

### Claim Window

`msg_server_claim_rewards.go::validateRequest` permits claiming for `E` strictly during effective epoch `E+1`. At `E+2` and beyond, the same request is rejected prior to payout. Therefore, after checked `E+2`, `claimed=false` can no longer be mutated by an ordinary late claim for E.

### Summary and Settle Amount

`accountsettle.go::SettleAccounts` generates a summary for each loaded active participant with `Claimed=false`, including `earned=0, rewarded=0`. A true zero payment is subsequently skipped prior to recording the settle amount. Summaries, participant resets, and non-zero settle amounts reside in the same `CacheContext` and are committed via a single `writeFn`.

`ClaimRewards::validateRequest` independently rejects zero settle amounts prior to payout and `finishSettle`, so a legitimate zero result remains `claimed=false` and legitimately utilizes claim-expiry starting at E+2. The `claimed=true, total=0` branch in `SettleClaim` is preserved solely as defensive compatibility; the current native source does not prove its reachability.

### Successful Claim and Claimed Write

Both payment legs, deletion of the settle amount, and writing `Claimed=true` take place within the shared payout `CacheContext`. Native atomicity tests verify rollback upon work/reward payment failures as well as the happy path `claimed=true`.

`finishSettle` logs any returned error from `SetEpochPerformanceSummary`, but the exact runtime environment does not make this abstract branch reachable for a correctly persisted summary:

- The key is already derived from a valid bech32 participant;
- Generated `EpochPerformanceSummary.MarshalToSizedBuffer` for scalar fields terminates with `(n, nil)`;
- `codec.CollValue` invokes this protobuf marshal;
- `collections.Map.Set` subsequently calls runtime KV `Set`;
- Gonka SDK runtime `coreKVStore.Set` invokes underlying `KVStore.Set` and returns `nil`; an underlying storage failure causes a panic, rather than an ignored returned error.

Therefore, the review does not consider the hypothetical returned-error branch a verified vulnerability. A panic or consensus failure constitutes a node-level crash, rather than a state with a successfully committed payout retaining a stale `claimed=false`.

### Retention and Alternative Paths

A search across the production tree at `042758f…` identified two write paths: summary creation in `SettleAccounts` and updating `claimed=true` in `finishSettle`. The helper `RemoveEpochPerformanceSummary` exists, but its caller is found only in unit tests; pruning removes claim-recipient schedules at E+5, rather than performance summaries. This is source-level evidence at an exact SHA, not a perpetual guarantee across future upgrades; the release pin and E2E tests must verify this behavior continuously.

## Absent or Inaccessible Summary Scenario

`GetEpochPerformanceSummary` folds not-found, invalid participant, and any `Map.Get`/decode error into `found=false`; queries convert this into gRPC `NotFound`. The contract receives only a generic query failure and does not treat it as proof of an absent claim.

A summary may be genuinely absent if the Host was not included in active participants for epoch E, the participant row was missing during `GetParticipants`, the active set was not established, or `SettleAccounts` failed as a whole. Module lifecycle logs settlement errors and proceeds with epoch formation. Previously, a Locked Deal remained permanently Locked in such situations. ADR-0013 introduces an agreed risk allocation: following an additional waiting epoch at E+3, narrowly classified unavailability of the current exact summary request permits an emergency Refund. This does not claim that the claim never took place, nor does it require a native API patch.

## Financial and State Invariants

1. The caller cannot supply amount or recipient.
2. CW20 refund strictly equals configured budget; donations and other assets remain in Deal escrow.
3. Claim, entitlement, release, gross, fee, and Host payout remain zero.
4. Terminal state, typed reason, accounting, and `HostOnly` are persisted prior to dispatching messages; a CW20 failure rolls back everything and allows one successful retry.
5. `claimed=true` always prohibits Refund regardless of amounts and mandates `SettleClaim`.
6. Repeated Refund and competing SettleClaim do not generate duplicate payouts.
7. The Factory `(Host, E)` index is preserved.
8. Following `Refunded`/`Expired`, all current and future `ngonka` tokens accrue to the Host per ADR-0010.
9. Routing-failure refunds, successful-claim shares, rounding, and `Completed` remain unmodified.

## Reference and Security Comparison

- DAO DAO audit base `0b5cae57…`, scope `cw-payroll-factory`, `cw-vesting`, `cw-wormhole`: exact funding and accounting-before-messaging applied; owner cancellation not ported.
- OroSwap assessed `9042989…`: permissionless griefing finding utilized to verify deterministic conditions and replay safety; no GPL code copied.
- Astroport Maker `1f50cab…`, Vesting `042b076…`: zero-send and event findings applied; `Expired` creates no transfer, and both outcomes emit typed events.

No external audit covers the Marketplace or native Gonka claim proof.

## Test Verification and Model Boundary

Unit tests: E+1/E+2+, overflow, Buyer/no Buyer, zero/positive unclaimed, `claimed=true` zero/positive, missing/default/malformed/oversized/query errors, mismatched Host/E, corrupted accounting, and terminal repeats.

`cw-multi-test`: exact deposit returned despite donations, other assets untouched, Factory index preserved, rollback/retry, mutual exclusion between settlement and refund, no-sale without transfers, and `HostOnly` release following `Refunded`/`Expired`. Legacy routing tests serve as regression verification.

Mocks verify contract reaction logic, but do not guarantee veracity of native `claimed` state, gas usage, production binaries, or real streamvesting/bank semantics. Golden Gonka E2E remains a mandatory shared milestone under workstream B.

The supplementary E+3 error matrix, raw query traces, gas boundaries, and modified economics are documented and tested in ADR-0013.
