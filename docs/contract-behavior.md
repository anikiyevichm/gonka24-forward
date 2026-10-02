# Contract behavior and accounting

This document describes implemented behavior. `E` is the immutable target epoch;
`current` is queried during the transaction. Epoch additions are checked for
overflow. Host, Buyer, fee recipient and caller may share addresses, but roles
and their accounting remain distinct.

## Lifecycle

| Operation | Starting state | Conditions | Result and asset movement |
| --- | --- | --- | --- |
| CW20 `Send` with `fund` hook | `Open` | Configured token, exact budget, current < E, exact routing to Deal | `Funded`; incoming budget only; Buyer fixed |
| `Lock` | `Open` or `Funded` | E <= current < E+5; exact routing | `Locked`; persist recipient proof; no transfers |
| `Cancel` | `Open`, no Buyer | Before E: Host only; E..E+4: anyone; successful routing query proves missing/mismatch in both cases | `Cancelled`; no transfers |
| `SettleClaim` | `Locked`, stored proof | Exact valid Host/E summary with claimed=true | Positive total: `Releasing`; zero: `Completed`; accrue USDT debts; no transfers |
| `WithdrawUsdt` | Any state with a non-zero pending debt for the role | Stored amount and recipient; anyone may call | Clear that debt and send one CW20 transfer; status unchanged |
| `Refund` (routing) | `Funded`, pristine accounting | E <= current < E+5; successful routing query proves missing/mismatch | `Refunded`, exact atomic Buyer refund, HostOnly |
| `Refund` (claim expiry) | `Locked`, stored proof, pristine accounting | current >= E+2; exact valid summary with claimed=false | Buyer present: `Refunded` and atomic full refund; absent: `Expired`, no transfer; HostOnly |
| `Refund` (network unconfirmed) | Same as claim expiry | current >= E+3; current summary attempt has an explicitly permitted error | Same financial outcome, distinct reason `network_unconfirmed` |
| `ReleaseUnlockedGnk` | `Releasing`, `Completed`, `Refunded` or `Expired` | Valid frozen policy/canonical counters; positive spendable ngonka | Distribute full bank balance; `Releasing` becomes `Completed` at U >= T |
| `ForwardExcessGnk` | `Completed` only | Same policy/counters and balance guards as release | Compatible release alias with identical shares |

Routing query errors, invalid addresses, duplicate target rows, exact routing and
E+5 or later block Cancel/routing Refund. Even Host cancellation before E must
first remove or change the native route: querying exact routing still rejects it.
`Cancelled` does not permit GNK release or arbitrary asset recovery.

Funding cannot be repeated or performed after Lock. Native reward claim is a
separate Host/authorized chain action, not a Deal execute call. Settlement neither
repeats routing nor has an E+2/E+5 deadline; stored Lock proof survives pruning.
An exact `claimed=true` summary always blocks Refund, even at zero total. A
summary error never becomes a zero settlement. SettleClaim and Refund are
mutually exclusive once finalized. None of these operations is scheduled by the
contract itself.

## Units and settlement formulas

USDT amounts are integer micro-units (6 decimals); GNK amounts are `ngonka`
(1 GNK = 1,000,000,000 ngonka). Price `P` is micro-USDT per whole GNK. Let `D` be
the accepted budget, `W` earned work, `R` reward and `T = checked(W + R)`.

```text
funded capacity C = floor(D * 1,000,000,000 / P)
Buyer entitlement B = min(T, C)
Host entitlement H = T - B
gross G = floor(B * P / 1,000,000,000)
fee F = floor(G * 150 / 10,000)
Host net = G - F
Buyer refund = D - G
Host net + F + Buyer refund = D
```

Budget and price are positive; creation rejects zero capacity. The funded amount
must equal configured budget. With no Buyer, effective `D`, capacity and `B` are
zero even though configured offer terms remain unchanged; all claim GNK belongs
to Host and all USDT obligations are zero. Donations do not enter these formulas.

| Reward GNK | Work GNK | Capacity GNK | Buyer GNK | Host GNK | Gross USDT at price 1 |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 50 | 2 | 100 | 52 | 0 | 52 |
| 90 | 10 | 100 | 100 | 0 | 100 |
| 100 | 100 | 100 | 100 | 100 | 100 |
| 99 | 1 | 100 | 100 | 0 | 100 |
| 10 | 90 | 100 | 100 | 0 | 100 |
| 50,000 | 50,000 | 4,000 | 4,000 | 96,000 | 4,000 |
| 0 | 0 | 100 | 0 | 0 | 0 |

For a 100 USDT budget and gross 80 USDT, fee is 1.2, Host net 78.8 and Buyer
refund 20. A confirmed zero total accrues the entire funded deposit as Buyer
debt and immediately completes; no-sale zero settlement accrues no USDT debt.
The defensive claimed-zero branch does not establish its native reachability.

## Independent USDT delivery

Settlement stores immutable accrued totals in Deal state and pending Host/Fee/
Buyer debts in `PENDING_USDT`. It generates no CW20 or Bank messages. Query
`UsdtPayments` returns recipient, accrued, pending and `paid = accrued - pending`
for each role. Before settlement there are no pending debts. After atomic Refund,
the accrued Buyer refund is already paid and its pending debt is zero.

`WithdrawUsdt` selects only the requested stored role, rejects zero debt with
`NothingToWithdraw`, clears the pending amount before its single CW20 transfer,
and rolls back that clearing if the transfer fails. Submit roles in separate
transactions to retain payment independence. Withdrawals remain available after
`Completed`; they do not require another native claim, settlement or refund.

State's `host_net_usdt`, `fee_usdt` and `buyer_refund_usdt` describe accrued
amounts, not payment receipts. `claim_settled` means accrual; `usdt_paid` and
`usdt_refunded` in successful transactions mean delivery.

## Permanent GNK shares and rounding

For `T > 0`, freeze exact Buyer fraction `B/T`; Host fraction is `(T-B)/T`.
For zero settlement, Refunded and Expired, freeze `HostOnly` as fraction `0/1`
without inventing a non-zero claim total. `Unset` grants no release right.

Let `U_old` be Buyer + Host lifetime released and `A` be current Deal bank balance
in ngonka. First require existing side counters to match their cumulative targets.

```text
U_new = checked(U_old + A)
Buyer target = floor(U_new * B / T)
Host target = U_new - Buyer target
Buyer delta = Buyer target - previous Buyer counter
Host delta = Host target - previous Host counter
Buyer delta + Host delta = A
```

Use the `0/1` policy for HostOnly. Counters are `Uint256`; target products use
`Uint512`; settlement products use `Uint256`. All conversions/additions are
checked, with no floating point arithmetic. Targets are monotonic; rounding
remainder goes to Host. Partitioning the same total volume into different release
calls yields the same final counters.

For `B/T = 1/3`, cumulative volumes 1, 2, 3 ngonka give Buyer targets 0, 0, 1.
For 80/20 shares, a 50 GNK release pays 40/10; an additional 100 GNK pays 80/20
and brings lifetime payouts to 120/30, even if original claim was 100 GNK.

The full spendable balance is distributed without a vesting query or claim cap.
USDT is not charged again. `Releasing` changes to `Completed` once when `U >= T`;
donations can cross this threshold before original vesting finishes. Later
distributions preserve status, policy and counters and emit no second completion.
`*_original_remaining_ngonka` diagnostics clamp original entitlement minus
lifetime payment at zero; they do not limit future payments.

A zero balance yields `NothingToRelease` or alias-specific `NothingToForward`,
with no mutation or zero send. GNK transfer failures roll back all sends and
counters in that release transaction. Other native denoms and CW20 are untouched;
unsupported assets or surplus USDT can remain locked without a recovery method.

## Refund finality and emergency risk

Refund records status, exact typed reason, full original budget and HostOnly
before sending the one Buyer transfer. Failure rolls back the outcome and ledger;
retry may succeed exactly once. No-Buyer expiry generates no zero transfer.
Claim, entitlement and lifetime counters must be pristine zeroes for Refund;
configuration, Buyer and Factory indices remain unchanged.

`NetworkUnconfirmed` evaluates the current query at or after E+3, not the duration
of an outage. Recovery before a successful refund permits normal settlement or
claim expiry. Recovery after finalization does not reopen the deal. If a native
claim occurred but is unverifiable, Buyer still receives the budget, Host and fee
receive no USDT, and all future GNK goes to Host. Current-epoch failures, request
errors, unexpected system errors, arithmetic/configuration errors and transfer
failures do not grant this fallback. See the exact [error matrix](chain-requirements.md#summary-error-policy).

Implementation: [Deal guards/storage/messages](../contracts/marketplace-deal/src/contract.rs),
[math](../packages/marketplace-common/src/math.rs),
[public types](../packages/marketplace-api/src/deal.rs).
