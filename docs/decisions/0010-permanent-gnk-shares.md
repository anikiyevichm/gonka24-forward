# ADR-0010: Permanent Proportional Shares for All GNK Inflows

- Date: 2026-09-07.
- Status: Accepted; implemented in PR #10.
- Supersedes release/excess policy from ADR-0005 and ADR-0009. Capacity and USDT settlement formulas from ADR-0005/0008 are preserved.
- Native source: Gonka `379bebced638aeb5e6077bfd51c986f898443832`.

## Decision and Economic Rationale

Following a successful native claim, `SettleClaim` fixes exact proportional shares for Buyer and Host. All available `ngonka` at the Deal address are distributed according to these shares perpetually, including donations prior to settlement and any late arrivals. The Buyer pays no additional USDT for extra GNK. The origin of individual coins is not tracked.

Initial entitlements determine the relative shares and USDT settlement, but no longer cap cumulative GNK payouts. Percentages are not rounded to basis points: exact integer numerators and denominators or equivalent frozen claim fields are preserved. A caller cannot alter shares, recipients, denomination, or payout amounts.

## Share Fixing

For a successful positive claim:

```text
T = work + reward > 0
B = min(T, funded_capacity) when Buyer is present; otherwise B = 0
H = T - B
buyer_share = B / T
host_share = H / T
```

These are post-claim shares, not promised percentages beforehand. Configured terms and frozen T/B/H are thereafter immutable. USDT gross, fee, and refund are computed once under the previous formula and do not depend on donations.

On valid `claimed=true, total=0`, the Buyer receives a full refund inside `SettleClaim`, the Deal transitions to `Completed`, and the GNK split is defined separately: Buyer 0%, Host 100%. For no-sale deals, the outcome is the same without USDT transfers.

Upon proven absence of a successful claim, a full refund is executed under agreed refund reasons and epoch gates, not on a single query error. Following a successful refund, the status is `Refunded` (or `Expired` without a Buyer); any available GNK belongs 100% to the Host. The same Host-only rule applies to legitimately terminated routing-failure refunds. These terminal states are not named `Completed`. Routing-failure and positive-summary claim-expiry/Expired are implemented per ADR-0011 under this economic rule. Absent-summary outcomes remain a native gap. `Cancelled` did not acquire new withdrawal rights in this decision: its asset policy remains an open item; execute handlers must not be widened automatically.

## Cumulative Release: Two Parties

```text
P = buyer_paid + host_paid = released_total
A = current available spendable ngonka balance
U = P + A
buyer_target = floor(U * B / T)
host_target = U - buyer_target
buyer_delta = buyer_target - buyer_paid
host_delta = host_target - host_paid
```

For Host-only fallbacks, use shares 0/1 and 1/1 without dividing by zero T. Frozen claim accounting during fallbacks remains zero; lifetime payout counters can grow. When A=0, return `NothingToRelease` without state mutations, outgoing messages, or financial events.

Invariants: previous counters are consistent with the same formula; sum of deltas equals A; each party's counter is monotonically non-decreasing. A non-zero Buyer delta requires a recorded Buyer. Only non-zero `BankMsg` transfers are dispatched. Counters and status transitions are persisted before dispatching messages; failure of any send rolls back all preceding sends and state.

`remaining_vesting` is excluded from calculations and is not a condition for permitting release. There are no caps `U <= T`, `buyer_paid <= B`, or `host_paid <= H`. There is no repetitive native performance or routing query: settlement proof is already persisted. CW20 tokens and other denominations do not participate. Spendability of the bank balance is proved via real-chain E2E testing.

Lifetime counters can exceed initial claim amounts and single-transaction maximum balances. The implementation must use sufficiently wide checked counters and intermediate calculations. If counters are `Uint256`, multiplying by weight cannot be assumed to fit in `Uint256`: use wider intermediates (`Uint512`) or safe mul/div factorizations. Overflow never resets counters and never generates payouts. Storage and schema changes must be explicit; merely removing the cap in the legacy helper is insufficient.

## Completed and Late Inflows

`Completed` indicates that initial Deal conditions have been successfully met. For T > 0, the transition `Releasing -> Completed` occurs when `U >= T`; event `deal_completed` is emitted exactly once in the same atomic transaction. For zero settlements, completion occurs following successful refund. `Completed` does not prove completion of native vesting: a donation can advance U to T prematurely.

Release remains available in `Releasing` and `Completed`, and Host-only payouts remain available following valid `Refunded` or `Expired`. Shares and counters are never reset. USDT settlement is never repeated. Future unlocks and donations require new invocations; contracts do not execute on a timer. Keepers gain no ability to alter distribution.

`ForwardExcessGnk` no longer grants the entire balance to the Host when Buyer share is positive. The preferred interface is the unified `ReleaseUnlockedGnk`. The legacy execute message is preserved as a backward-compatible alias of the shared logic restricted to `Completed`, with identical shares, counters, and atomic rollback. The old Host-only branch for successful positive claims is removed.

## Examples

All figures below are expressed in GNK for readability; code computes atomic integer `ngonka` (1 GNK = 10^9 ngonka).

### 80/20 Split, Donation, and Completion

Claim T=100, B=80, H=20. First available=25: Buyer 20, Host 5, P=25.
Then available=125 (75 remaining original + 50 donation): U=150, targets=120/30, new payouts=100/25. Deal becomes `Completed`; Buyer receives 120 without extra payment.
Later available=10: U=160, targets=128/32, new payouts=8/2.
Status remains `Completed`, no duplicate completion event.

### New Vesting Does Not Defer Old Schedule

Hypothetically, original 100 unlocks at 1 per epoch. After 50 epochs, an additional 100 is added over the next 100 epochs. The next 50 unlocks yield 2 per epoch (1 old + 1 new), followed by 50 unlocks of 1 new each. The original remaining 50 is not delayed.
With 80/20 shares, every available 2 yields 1.6/0.4 GNK. Total remaining vesting may rise to 150, but no longer blocks distribution of available balances.

### Rounding on Atomic Units

T=3 ngonka, B=1, H=2. Successive inflows of 1 ngonka each:

| U | Buyer Target | Host Target | Buyer Delta | Host Delta |
|---|---|---|---|---|
| 1 | 0 | 1 | 0 | 1 |
| 2 | 0 | 2 | 0 | 1 |
| 3 | 1 | 2 | 1 | 0 |
| 4 | 1 | 3 | 0 | 1 |

A single call at U=3 yields the same 1/2 result. Buyer deviation from exact share is strictly less than one ngonka. Never round each tranche separately.

### Zero / No Claim

Buyer deposits 100 USDT, confirmed claim=0: refund=100 USDT, `Completed`.
Later, 7 GNK arrives: all 7 to Host, Buyer 0. Under proven no-claim outcomes, an analogous GNK split applies post-`Refunded`; query errors do not grant this status.

## Why We Replaced the Previous Decision; Reference Review

The previous cap enforced the economic rule: "third-party tokens neither accelerate nor increase payouts." This rule was intentionally abandoned. On a single account, Gonka aggregates future epoch amounts; `AddVestedRewards`/`applyVestingSchedule` append new amounts to existing ones, while `ProcessEpochUnlocks` transfers the first tranche from the module account to the address. The old calendar is not deferred; delay was caused by our calculation `T - min(T, remaining_total)`. The new model eliminates this dependency.

DAO DAO / Oak (base `0b5cae57fecbbadb1045f3dc2bb4ad4fe5a98ee8`; `cw-vesting`, `cw-payroll-factory`, `cw-wormhole`) — exact funding and atomic accounting are preserved, but bounded entitlements are intentionally superseded. Finding #2 and fix `fadf2e4a2bbcc6363ca06c1e6e7bb0c745f00f99` apply to immutable USDT deposits.
OroSwap / Halborn (main scope `9042989f8fd00b6524b470a5850ec03f8e5a2e4e`): HAL-01 / `f7566310fd7ab86c080f4c7aa77deee4eb94407f` highlights permissionless griefing; instead of keeper authorization, we prove that the split is invariant to call frequency. HAL-06 / `308d8c64b8861524c17c796754091494f530d1a0` — conservation.
Astroport / Oak update scope Maker `1f50cabf6738f6ad57b6ed7b1d56f1276fe6d526`, Vesting `042b0768951422099f5d77224c320978cbfa92cc`: findings 5/8 — events / zero transfers, report does not provide individual remediation SHAs. These audits do not validate our new formula; no GPL code was copied. Legacy native and runtime gates remain active.

## Mandatory New Evidence

- Property tests: split is invariant to partitioning of available amounts; conservation and monotonic counters.
- U=T-1, U=T, U>T, donation skipping over T, late release after `Completed` without repeated events.
- Shares: 0%, 100%, exact fractions; dust; wide lifetime volume and checked overflow.
- Zero/no-sale/no-claim Host-only; prevention of withdrawals prior to legitimate outcomes.
- Increasing remaining vesting does not prevent release; failures of non-mandatory vesting queries do not block payouts.
- Rollback of 1st/2nd `BankMsg`, including initial completion, and safe retry.
- Coincident recipients; native funds rejection; preservation of CW20 and unrelated denoms.
- `ForwardExcess` alias respects shares, states, and counters without bypass.
- Real Gonka E2E: available balance post-unlock and independence of old schedules from new schedules.

## V2 and Boundaries

Deals isolate individual Host/epoch instances; settlements across epochs are not netted.
N Buyers requires a distinct algorithm: one cannot simply round N-1 shares and allocate the remainder to the last party. For three equal shares, targets of 0/0/2 at U=2 and 1/1/1 at U=3 would cause the last party's counter to decrease. N-party rounding and bounded/pull distributions are not automatically achieved by generalizing two-party logic.

## Implementation and Public Boundary

PR #10 records `GnkReleasePolicy::{Unset, Proportional, HostOnly}` explicitly.
`Proportional` stores initial `buyer_share_numerator=B` and `share_denominator=T`; `HostOnly` employs mathematical fallback `0/1` without altering zero claim fields. Lifetime `released_total`, `buyer_released`, and `host_released` are widened to `Uint256`, with `Uint512` used for intermediate target multiplication. This is a deliberate storage and JSON schema update; no migration is necessary since deployed instances and admin keys do not exist.

`State`, `Entitlements`, and `ReleaseStatus` expose the frozen policy; payout counters in `State`/`ReleaseStatus` serialize as wide decimal strings. Fields `*_original_remaining_ngonka` in `ReleaseStatus` provide clamp-to-zero diagnostics of original obligations, not caps on future payouts. `gnk_released` publishes shares, lifetime totals, current deltas, available balance, and actual entrypoint. The alias additionally emits `excess_gnk_forwarded` while maintaining identical economics.

Evidence includes the exact 100/80 test case, U<T/U=T/U>T, late donations and release post-`Completed`, 0/1, 1/1, 1/3 dust, arbitrary partitions above T, wide counters, overflow, malformed policy/accounting, bank fault rollback, zero settlement, `Refunded`/`Expired` fixtures, and alias split parity. Real Gonka E2E remains a distinct release gate.

Local gates: 104 workspace tests and proto golden tests; code formatting; workspace/proto-gen `clippy -D warnings`; release Wasm and `cosmwasm-check 2.2.2`; `cargo audit`/`cargo deny` for both lockfiles. Two builds using pinned optimizer `0.16.1@sha256:b9c92b…69e` yielded identical SHA-256 hashes:

```text
marketplace_deal.wasm    9829f5e4a0c8e2b413d23bea1bbe17c420e1e65e58632d01fa1b21d1c08d01ce
marketplace_factory.wasm 8366a874619c03e54ed36edf6b0ebbde4001afcf6c1b02ed40f9762350e6d82b
```
