# CosmWasm Technical Specification: Gonka Forward Marketplace MVP

Workstream: Developer A
Status: Target design following source review, `GO WITH CAVEATS`
Pinned Gonka SHA: `379bebced638aeb5e6077bfd51c986f898443832`

## 1. Scope of Responsibility

Developer A implements two external CosmWasm contracts:

1. Factory — instantiates and indexes deals.
2. Deal Contract — isolated immutable dual escrow for one Host, one Buyer, and one epoch.

The Marketplace does not modify Gonka reward calculations and does not broadcast native messages on behalf of the Host. The Host independently signs:

- `MsgSetClaimRecipients` prior to epoch E;
- `MsgClaimRewards` during epoch E+1.

The contract reads only consensus state and never accepts claim amounts from callers/backends.

## 2. Architectural Design

### 2.1 One Address per Deal

A single global escrow is prohibited. Gonka streamvesting aggregates schedules by recipient address and does not retain marketplace deal IDs. To ensure GNK isolation, each deal receives a unique Deal Contract address.

```text
Factory
  -> Deal(host_1, epoch_10)
  -> Deal(host_2, epoch_10)
  -> Deal(host_1, epoch_11)
```

The Factory holds neither USDT nor GNK for deals. All assets reside within the respective Deal Contract.

### 2.2 Dual Escrow

```text
USDT: Buyer -> Deal -> Host + fee recipient + Buyer refund
GNK:  Gonka -> streamvesting(Deal) -> Deal balance -> Buyer + Host
```

### 2.3 Trust Model

- No owner/admin settlement.
- No arbitrary withdrawal.
- No manual input of `RewardCoins`, `WorkCoins`, claim status, or vesting amounts.
- No migration admin on Factory or Deal instances.
- Permissionless callers only trigger deterministic transitions.
- Any ambiguous query error fails closed.

## 3. Configuration

### 3.1 Factory Instantiate

```rust
pub struct FactoryInstantiateMsg {
    pub deal_code_id: u64,
    pub settlement_cw20: String,
    pub fee_recipient: String,
    pub fee_bps: u16, // must equal 150
}
```

Validation checks:

- Addresses are valid;
- `fee_bps == 150`;
- `settlement_cw20` responds to `TokenInfo`;
- `decimals == 6`;
- Configuration is immutable;
- Factory is instantiated with `admin = None`.

### 3.2 Deal Immutable Config

```rust
pub struct DealInstantiateMsg {
    pub factory: String,
    pub host: String,
    pub target_epoch: u64,
    pub price_micro_usdt_per_gnk: Uint128,
    pub buyer_budget_micro_usdt: Uint128,
    pub settlement_cw20: String,
    pub fee_recipient: String,
    pub fee_bps: u16,
    pub pinned_gonka_sha: String,
}
```

Validation checks:

- `host`, `factory`, token, and fee recipient are valid addresses;
- `price > 0`, `budget > 0`, `fee_bps == 150`;
- `target_epoch > GetCurrentEpoch()`;
- `target_epoch <= current + 40`;
- Checked capacity is greater than zero;
- `pinned_gonka_sha` matches compile-time constant;
- Deal is instantiated with `admin = None`.

`deal_address` is obtained from `env.contract.address`; it is not supplied by the caller and represents the expected claim recipient.

## 4. Economics

### 4.1 Units

```text
USDT: microUSDT, 6 decimals
GNK:  ngonka, 9 decimals
GNK_SCALE = 1_000_000_000
```

Price is stored as integer `microUSDT` per one GNK. This provides six decimal places of price precision and aligns with the settlement token.

### 4.2 Buyer Capacity

```text
funded_capacity_ngonka = floor(
  buyer_budget_micro_usdt * GNK_SCALE
  / price_micro_usdt_per_gnk
)
```

### 4.3 Claim Total and Entitlement

Following exact query `EpochPerformanceSummaryByParticipant(Host, E)`:

```text
work_ngonka   = summary.earned_coins
reward_ngonka = summary.rewarded_coins

total_claim_ngonka = checked_add(work_ngonka, reward_ngonka)

buyer_entitlement = min(total_claim_ngonka, funded_capacity_ngonka)
host_entitlement  = total_claim_ngonka - buyer_entitlement
```

If Buyer is absent, budget/capacity/buyer entitlement equal zero, and the entire claim belongs to the Host.

### 4.4 USDT Settlement

```text
gross_usdt = floor(
  buyer_entitlement * price_micro_usdt_per_gnk / GNK_SCALE
)

protocol_fee = floor(gross_usdt * 150 / 10_000)
host_net_usdt = gross_usdt - protocol_fee
buyer_refund  = buyer_budget_micro_usdt - gross_usdt
```

Invariant:

```text
host_net_usdt + protocol_fee + buyer_refund = buyer_budget_micro_usdt
```

All multiplications are executed in `Uint256`, and all conversions to `Uint128` are checked. Floating point operations are prohibited.

### 4.5 Mandatory Examples

| Reward | Work | Capacity | Buyer GNK | Host GNK | Gross at 1 USDT/GNK |
|---:|---:|---:|---:|---:|---:|
| 50 | 2 | 100 | 52 | 0 | 52 |
| 90 | 10 | 100 | 100 | 0 | 100 |
| 100 | 100 | 100 | 100 | 100 | 100 |
| 99 | 1 | 100 | 100 | 0 | 100 |
| 10 | 90 | 100 | 100 | 0 | 100 |
| 50,000 | 50,000 | 4,000 | 4,000 | 96,000 | 4,000 |
| 0 | 0 | 100 | 0 | 0 | 0 |

## 5. State

```rust
pub enum DealStatus {
    Open,
    Funded,
    Locked,
    Releasing,
    Completed,
    Refunded,
    Cancelled,
    Expired,
}

pub struct DealState {
    pub status: DealStatus,
    pub refund_reason: Option<RefundReason>,
    pub buyer: Option<Addr>,
    pub recipient_locked: bool,

    pub work_ngonka: Uint128,
    pub reward_ngonka: Uint128,
    pub total_claim_ngonka: Uint128,
    pub buyer_entitlement_ngonka: Uint128,
    pub host_entitlement_ngonka: Uint128,
    pub gnk_release_policy: GnkReleasePolicy,

    pub released_total_ngonka: Uint256,
    pub buyer_released_ngonka: Uint256,
    pub host_released_ngonka: Uint256,

    pub gross_usdt: Uint128,
    pub fee_usdt: Uint128,
    pub host_net_usdt: Uint128,
    pub buyer_refund_usdt: Uint128,
}
```

`GnkReleasePolicy` has variants `Unset`, `Proportional { B, T }`, and `HostOnly`. Prior to `SettleClaim`, claim/entitlement/release fields are zero, and policy is `Unset`. Following settlement, claim fields and policy are immutable, and wide lifetime counters may grow above `Uint128::MAX` through sequential distributions. `RefundReason` includes `RoutingMissing`, `RoutingMismatch`, `ClaimExpiry`, and `NetworkUnconfirmed`.

Factory state:

```rust
CONFIG: Item<FactoryConfig>
NEXT_DEAL_ID: Item<u64>
DEALS: Map<u64, Addr>
DEAL_BY_HOST_EPOCH: Map<(Addr, u64), Addr>
```

A duplicate offer for the same `(Host, epoch)` is rejected.

## 6. Execute Interface

### 6.1 Factory

```rust
pub enum FactoryExecuteMsg {
    CreateOffer {
        target_epoch: u64,
        price_micro_usdt_per_gnk: Uint128,
        buyer_budget_micro_usdt: Uint128,
    },
}
```

`CreateOffer`:

1. Caller becomes Host.
2. Epoch, price, budget, and uniqueness of Host/E are validated.
3. Factory sends `WasmMsg::Instantiate` with `admin = None`.
4. Reply extracts Deal address and records indices.
5. Event emits deal id, address, Host, E, price, and budget.

### 6.2 Deal

```rust
pub enum DealExecuteMsg {
    Receive(Cw20ReceiveMsg),
    Lock {},
    SettleClaim {},
    WithdrawUsdt { role: UsdtRole },
    ReleaseUnlockedGnk {},
    Refund {},
    Cancel {},
    ForwardExcessGnk {},
}

pub enum Cw20HookMsg {
    Fund {},
}
```

`SetClaimRecipients` and `ClaimRewards` are not Deal execute messages.

## 7. State Transitions

### 7.1 `Open -> Funded`

Caller: Settlement CW20 via `Send` hook.
Conditions:

- `status == Open`;
- `info.sender == settlement_cw20`;
- Amount exactly equals budget;
- Sender is designated future Buyer;
- `current < E`;
- Current recipient schedule contains exact `(Host, E) -> env.contract.address`.

Asset movement: CW20 remains in Deal balance.
Mutation: Persist Buyer and `Funded`.

Repeated, partial, and late funding are rejected.

### 7.2 `Open/Funded -> Locked`

Caller: Any.
Conditions:

- `E <= current < E+5` on pinned source, while recipient row is not yet eligible for pruning deletion;
- Exact recipient entry is still accessible;
- Recipient equals Deal address.

Mutation: `recipient_locked = true`, `status = Locked`.

If Buyer is absent, this is a no-sale passthrough: future claim belongs entirely to the Host.

If the deal is funded but recipient in the provably pre-pruning window is missing or mismatched, `Lock` must not silently fail indefinitely. Permissionless `Refund` performs a full Buyer refund and transitions state to `Refunded`.

If keepers miss the entire provable window and the row may have already been pruned, absence of the record is ambiguous. The contract never fabricates historical routing and does not execute refund or settlement. This constitutes a liveness failure and a release gate for immutable v1.

### 7.3 `Open -> Cancelled`

Caller: Host before E; any caller at `current >= E`.
Conditions:

- Buyer is absent;
- Query proves recipient for E no longer equals Deal address.

Before E, the Host must first remove or update the native recipient. After E, a permissionless caller can close a no-sale offer only within the provable pre-pruning window. This prevents future claims from routing to a decommissioned contract without inferring status from pruned history.

### 7.4 `Locked -> Releasing/Completed` via `SettleClaim`

Caller: Any.
Conditions:

- Exact summary for Host/E exists;
- `summary.claimed == true`;
- Summary Host/E matches config;
- Checked `total_claim = earned_coins + rewarded_coins`; zero claim is acceptable.

Settlement freezes Work, Reward, total claim, Buyer/Host entitlements,
`Proportional { B, T }` or `HostOnly`, and outstanding Host/Fee/Buyer USDT obligations
without outgoing calls. Positive total enters `Releasing`; zero enters `Completed`.

Permissionless `WithdrawUsdt { role }` reads the amount and immutable recipient
from storage, clears only that role, and emits one CW20 Transfer. Failure rolls
back that withdrawal and preserves its debt. Use separate transactions per role.
`UsdtPayments {}` reports accrued, paid, and pending amounts. Withdrawals remain
available after `Completed`; zero debt is rejected.

With `claimed=true,total_claim=0`, Buyer acquires zero GNK, gross/fee/host_net and
GNK counters are zero, and the entire deposit is owed to Buyer for a separate
`WithdrawUsdt { role: Buyer }`. No E+2 wait or Refund is required. No-sale settlement
creates no USDT obligations. `claim_settled` records accrual, while
`usdt_paid`/`usdt_refunded` record successful individual payments.

Repeated SettleClaim/Refund cannot reallocate obligations. GNK release does not
wait for USDT delivery; this separation is intentional. After zero settlement,
subsequent GNK belongs entirely to Host under ADR-0010.

This specifies Marketplace behavior: pinned Gonka in normal native execution rejects zero claim before setting claimed=true. If a zero claim does not occur, claim expiry §7.5 applies starting from E+2. Checks for exact Host/E, response validity, and trust assumptions of authoritative summary are mandatory for both paths; query errors do not evaluate to zero, but after E+3 may produce a dedicated emergency outcome per ADR-0013.

### 7.5 `Locked/Funded -> Refunded`

Caller: Any.

Path A, routing failure:

- `E <= current < E+5` and row remains within provable pre-pruning window;
- Deal is funded;
- Recipient is provably absent or not equal to Deal.

Implemented: State records `Refunded`, `RefundReason`, exact initial budget in `buyer_refund_usdt`, and `HostOnly` prior to CW20 transfer. Claim/entitlement/counters, gross/fee/Host payout remain zero. Donations do not increase refund.

Path B, claim expiry:

- Deal is locked and funded;
- `current >= E+2`;
- Successfully retrieved exact Host/E summary with `claimed == false`;
- `claimed == true` at any amount requires SettleClaim.

Implemented for positively read unclaimed summary. Prior to E+3, missing summary, query/decode/identity failures do not permit refund. With `current >= checked(E+3)`, normal native handler errors, typed unavailable route/invalid responses, malformed/oversized/missing responses, or incorrect Host/E identity permit `NetworkUnconfirmed`. This is deliberate risk allocation, not proof of claim absence. Request/current-epoch/config/accounting/overflow and unexpected system errors remain fail-closed; `claimed=true` prohibits Refund regardless of epoch and amount.

Asset movement: Full CW20 budget -> Buyer.
Mutation: `Refunded` prior to transfer message.

State, reason, and accounting are persisted before transfer; CW20 failure rolls everything back. Each attempt re-queries summary anew. Recovered `claimed=true` requires settlement, recovered `claimed=false` yields standard ClaimExpiry.

### 7.6 `Locked -> Expired`

Caller: Any via `Refund {}`; a separate Expire execute message is not added.
Conditions: No-sale deal and either `current >= E+2` with exact Host/E `claimed=false`, or `current >= E+3` with permitted ADR-0013 summary error.
Asset movement: None.
Mutation: `Expired`.

### 7.7 `Releasing -> Completed`

Caller: Any via `ReleaseUnlockedGnk`.
Condition: `released_total >= total_claim` for positive claims. Completed signifies successful fulfillment of original terms, but release remains operative with the same shares and counters; completion event is emitted once.
Mutation: `Completed`.

## 8. Native Query Adapter

```text
/inference.inference.Query/GetCurrentEpoch
/inference.inference.Query/ListClaimRecipients
/inference.inference.Query/EpochPerformanceSummaryByParticipant
/inference.streamvesting.Query/TotalVestingAmount
```

Additionally, standard `BankQuery::Balance` for `ngonka` is used.

Adapter rules:

- Responses are decoded with generated protobuf types from pinned SHA;
- Exact participant, epoch, address, and denom are validated;
- Duplicate target recipient entries are treated as errors;
- Normal handler errors, including implicit NotFound, are preserved separately from typed system results, but error text is not parsed;
- Only the narrow summary adapter consumes raw two-level results for the E+3 error matrix; GetCurrentEpoch and other callers maintain fail-closed policy;
- Unknown or oversized values fail closed;
- Query results are not cached across transactions, except for explicit immutable snapshots.

## 9. `ReleaseUnlockedGnk`: Permanent Shares

Active and implemented policy per PR #10: [ADR-0010](decisions/0010-permanent-gnk-shares.md).

Following claim with T > 0, exact shares B/T and (T-B)/T are frozen, where B is original buyer entitlement. These are not rounded percentages and not caps on future distributions. USDT settlement is never repeated; Buyer pays nothing for additional GNK.

```text
P = buyer_paid + host_paid = released_total
A = available spendable ngonka balance
U = P + A
buyer_target = floor(U * B / T)
host_target = U - buyer_target
buyer_delta = buyer_target - buyer_paid
host_delta = host_target - host_paid
```

Validate existing counters; checked wide arithmetic for lifetime turnover. When A = 0, returns NothingToRelease without mutation. The sum of deltas equals A. Counters and state are saved first, then non-zero BankMsg transfers are dispatched. Any error rolls back all sends and state. CW20 and other denoms are untouched.

Remaining vesting does not restrict release and is not a required query. At U = T, original entitlements are satisfied exactly. At U > T, distributions continue under the same shares, and Releasing transitions to Completed once. Release remains permitted after Completed; counters are not reset. Gifts may fulfill initial obligations earlier: Completed does not imply completion of native vesting.

Under zero settlement and following legitimate Refunded/Expired, GNK split is 0% Buyer, 100% Host, without division by zero. Prior to confirmed outcome, withdrawals are prohibited. Cancelled asset policy is not extended by this decision and requires a separate decision.

## 10. External GNK Transfers

All available ngonka, including pre-settlement donations and late distributions, are split according to frozen shares. Liquid donations become spendable immediately upon a permitted outcome; vested donations unlock sequentially. New native schedules do not reschedule old tranches; aggregate remaining vesting does not delay our distributions.

ForwardExcessGnk is prohibited from routing the full balance to the Host when a positive Buyer share exists. It is retained as a compatible alias for general release logic strictly in Completed, without bypassing shares, counters, or state gates. Host-only fallback is preserved strictly for agreed zero/no-claim outcomes. Examples of 80/20 splits, late deposits, rounding, and zero claims are in ADR-0010.

## 11. Query Interface

Factory:

```rust
pub enum FactoryQueryMsg {
    Config {},
    Deal { id: u64 },
    DealByHostEpoch { host: String, epoch: u64 },
    ListDeals { start_after: Option<u64>, limit: Option<u32> },
}
```

Deal:

```rust
pub enum DealQueryMsg {
    Config {},
    State {},
    Funding {},
    Entitlements {},
    ReleaseStatus {},
    NativeStatus {},
}
```

`NativeStatus` is a read-only convenience query and does not substitute for execute validations. `State`, `Entitlements`, and `ReleaseStatus` explicitly return frozen `GnkReleasePolicy`. Lifetime payout counters in `State`/`ReleaseStatus` have type `Uint256`. `ReleaseStatus.*_original_remaining_ngonka` clamps to zero once initial entitlement is exceeded, serving as diagnostics rather than payout caps. An error in the vesting segment of `NativeStatus` does not affect `ReleaseUnlockedGnk`.

## 12. Events

Minimal event types:

```text
offer_created
deal_funded
deal_locked
claim_settled
usdt_paid
usdt_refunded
gnk_released
deal_completed
deal_refunded
deal_cancelled
deal_expired
excess_gnk_forwarded
```

Every event contains deal address, Host, E, and relevant amounts. Routing `deal_refunded` additionally includes `reason`, `status=refunded`, Buyer, and exact deposit; mismatch also includes validated `observed_recipient`. For `gnk_released`, attributes include exact share numerator/denominator, Buyer/Host deltas, lifetime totals, available balance, and actual entry point. The alias additionally emits `excess_gnk_forwarded`; `deal_completed` is emitted only upon initial successful transition to Completed: via positive-claim release from Releasing or via zero settlement from Locked. No event serves as source of truth for subsequent state transitions.

## 13. Errors

Dedicated typed errors are required for at least:

- unauthorized;
- invalid epoch/lookahead;
- duplicate Host/E;
- invalid/zero price or budget;
- wrong CW20;
- wrong amount;
- late funding;
- invalid state;
- recipient missing/mismatch/duplicate;
- query unavailable/decode/not-found;
- claim not confirmed;
- claim window not expired;
- arithmetic overflow;
- unsupported denom;
- nothing to release;
- terminal deal;
- excess forwarding before completion;
- invalid frozen release policy or non-canonical lifetime counters.

## 14. Security Invariants

1. One Deal address serves only one Host/E deal.
2. Buyer funds strictly prior to E and strictly with exact budget.
3. Host USDT payout is possible only after `claimed=true`.
4. Caller provides no native amounts.
5. `total_claim = rewarded + earned` is computed with checked arithmetic.
6. Initial Buyer entitlement does not exceed capacity; lifetime GNK payouts follow frozen shares and may exceed entitlement.
7. Host share = (total_claim - buyer_entitlement) / total_claim; fallback per ADR-0010.
8. `released_total` may exceed `total_claim`; agreed shares alone govern payouts.
9. Buyer/Host release counters sum to released total.
10. Repeated calls produce no double spend.
11. Any available GNK donation is split per frozen shares; remaining vesting does not restrict release.
12. CW20 transfer failure rolls back state.
13. BankMsg failure rolls back state.
14. Refund and successful settlement are mutually exclusive.
15. No privileged recipient override or arbitrary withdrawal.
16. NetworkUnconfirmed is possible only from pristine Locked after checked E+3; gas exhaustion/VM abort is not a successful query error.

## 15. Testing

### 15.1 Unit Tests

- Price/capacity at boundaries and all rounding directions.
- Checked overflow for add/multiply/conversion.
- All mandatory economic examples.
- `Reward-heavy` and `Work-heavy` yield identical results for identical totals.
- Fee floor and accounting equality.
- Cumulative split across random T/B and random tranche sequences.
- Exact Buyer/Host entitlements at U = T and continuation of identical share at U > T.
- Zero/one-ngonka/dust cases.
- State transition matrix.
- Wrong/duplicate recipient entries.
- NotFound isolated from transport/decode errors.
- Repeated settle/refund/release calls.
- Liquid/vested donation models, pre-settlement gifts, and late release after Completed.

### 15.2 Property Tests

Across random valid inputs:

```text
buyer + host = total
gross <= budget
net + fee + refund = budget
buyer_released = floor(released_total * B / T)
host_released = released_total - buyer_released
buyer_released + host_released = released_total
sum(new_deltas) = available_balance
```

Across any partitioning of arbitrary U into tranches, cumulative results match, including U > T; at T = 0, Host-only fallback applies.

### 15.3 `cw-multi-test`

- Factory instantiate and reply address indexing.
- CW20 exact funding.
- Settlement with three CW20 transfers.
- Native balance release via two BankMsgs.
- Rollback upon failure of secondary transfer.
- No-sale passthrough.
- Late GNK after Completed maintains split; zero/no-claim fallback 100% Host.
- Absence of admin/migrate paths.

### 15.4 Real-Chain E2E Dependency

`cw-multi-test` does not prove custom gRPC, contract recipients, streamvesting, or native bank semantics. Golden E2E from workstream B is a mandatory release gate.

## 16. Reusable Code and Audit References

No external contract may be copy-pasted in its entirety. Priority remains with pinned Gonka source. External projects serve to validate patterns and test invariants.

| Reference | Exact File / Function | Applicable Content | Classification | Caveat |
|---|---|---|---|---|
| [`gonka-ai/gonka`](https://github.com/gonka-ai/gonka) pinned SHA | `contracts/liquidity-pool/src/contract.rs`, native query/bank paths | Gonka-compatible CosmWasm dependencies, native balance, and `BankMsg::Send` | `COPY/ADAPT` | Query paths/types strictly from pinned proto |
| [`gonka-ai/gonka`](https://github.com/gonka-ai/gonka) pinned SHA | `contracts/wrapped-token` | Mock CW20 USDT with 6 decimals | `COPY/ADAPT` | Local E2E only, not bridge golden path |
| [`DA0-DA0/dao-contracts`](https://github.com/DA0-DA0/dao-contracts) snapshot `0178cf55d358356474e5530cccac6acdccd0d94b` | `contracts/external/cw-payroll-factory/src/contract.rs::instantiate_contract` and reply | Factory instantiates isolated indexed vesting instances | `ADAPT` | In our MVP, code id/config are immutable, `admin = None` |
| Same repository | `contracts/external/cw-vesting/src/vesting.rs::Payment::distributable`, `distribute` | Permissionless distribution bounded by entitlement and liquid balance | `LEARN` | Native Gonka vesting remains source of truth |
| Same repository | `contracts/external/cw-vesting/SECURITY.md` | Explicit invariants and requirement of real-chain tests for SDK behavior | `LEARN` | Root README references [Oak audit reports](https://github.com/oak-security/audit-reports/tree/master/DAO%20DAO), but exact coverage of current snapshot is not assumed |
| [`CosmWasm/cw-tokens`](https://github.com/CosmWasm/cw-tokens) snapshot `1db4b7387953538d7a0123d3732385981d18db57` | `contracts/cw20-escrow/src/contract.rs::execute_receive`, `execute_refund` | CW20 receive validation and terminal refund | `ADAPT` | Repository explicitly warns: not audited/production-ready |
| Same repository | `contracts/cw20-streams/src/contract.rs::execute_withdraw` | Cumulative `vested - claimed` delta pattern | `LEARN` | Time-based streams cannot replace Gonka schedule |

External reviews reinforce three core decisions: dedicated contract instance per agreement, state update prior to outgoing transfer, and cumulative accounting. They do not replace Gonka real-chain proof.

## 17. Definition of Done

| Requirement | Evidence |
|---|---|
| Factory creates unique Deal per Host/E | Contract tests and indexed queries |
| Deal is immutable | Instantiate admin absent; migrate entry point absent |
| CW20 validated and exact-funded | Unit + multi-test + real chain |
| Recipient = Deal verified | Generated gRPC adapter + E2E |
| Total claim includes Reward + Work | Unit + E2E balances |
| Under/overproduction handled correctly | Mandatory examples + property tests |
| USDT accounting conserved | Property tests + balance E2E |
| Cumulative release exact | Property tests + 180-epoch E2E / accelerated params |
| Donations distributed per frozen shares, including U > T | Adversarial tests ADR-0010 |
| Repeat calls idempotent / fail-safe | Unit, multi-test, E2E |
| Query errors fail closed | Adapter tests + disabled-route E2E |
| No arbitrary withdrawal / admin | Code review + chain contract info |
| Pinned SHA matches | Build manifest and generated headers |

## 18. Excluded from Developer A Scope

- Alteration of Gonka reward/claim/vesting keepers.
- Signing of native Host messages.
- Price oracles.
- Pools, partial fills, multiple Buyers.
- Distinct trading of WorkCoins.
- Bridge in golden E2E.
- Off-chain trusted settlement.

## 19. Blockers Before Immutable Deployment

- Real Gonka accepts Deal address as claim recipient.
- Unlocks appear as spendable contract bank balance.
- `TotalVestingAmount` accessible from Wasm for `NativeStatus` diagnostics after allowlist patch; release does not depend on it.
- `BankMsg::Send` splits real unlocked GNK.
- E2E confirms new vesting schedules do not reschedule old tranches and spendable balance releases independently of aggregate remaining balance.
- Rare `claimed=true` write-error risk resolved.
- Production CW20 USDT, fee recipient, and Gonka release SHA pinned.

Until these items are resolved, contracts may be developed and tested, but cannot be deemed production-ready.

## 20. A8 Contract-Only Evidence

End-to-end verification following PR #11 merge is documented in [A8 gap analysis](reviews/a8-contract-gap-analysis.md). Exact generated-schema payloads and handoffs for B are located in [integration guide](integration-guide.md). The contract-only suite covers Factory-created happy paths, economic matrix, no-sale/zero/cancel/routing-refund, terminal indices, rollbacks, permanent shares, both release entry points, and multi-Host/epoch Deal isolation.

Claim-expiry Refund/no-sale Expired under positive unclaimed summaries and emergency NetworkUnconfirmed after E+3 are part of the contract-only result. Real-chain golden E2E, native gas regressions, and remaining blockers from §19 retain `GO WITH CAVEATS` status.
