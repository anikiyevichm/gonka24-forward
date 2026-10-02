# Client and keeper guide

Public wire definitions are in the committed
[Factory schema](../contracts/marketplace-factory/schema/marketplace-factory.json)
and [Deal schema](../contracts/marketplace-deal/schema/marketplace-deal.json).
Amounts represented by `Uint128`/`Uint256` are decimal JSON strings; epochs, IDs,
limits and fee bps are JSON numbers. Send no native funds with execute or
instantiate. Example addresses below are placeholders requiring substitution.

## Configure and create an offer

Before use, verify on-chain Factory checksum, code ID, configuration and absent
admin. Verify the configured token and its six decimals; use the configured CW20,
not an assumed denomination or a similarly named token. Each newly created Deal
must have the intended code ID and no admin. See [release guide](release-guide.md).

Factory instantiate configuration:

```json
{"deal_code_id":1,"settlement_cw20":"<CW20>","fee_recipient":"<FEE>","fee_bps":150}
```

Host executes on Factory (here price is 1 USDT/GNK and budget 100 USDT):

```json
{"create_offer":{"target_epoch":42,"price_micro_usdt_per_gnk":"1000000","buyer_budget_micro_usdt":"100000000"}}
```

Choose a future epoch within 40 of the current one. Read the confirmed
`offer_created` event and reconcile the address with Factory:

```json
{"deal_by_host_epoch":{"host":"<HOST>","epoch":42}}
```

The native Host/authorized signer must set `(Host, 42) -> Deal` through Gonka
`MsgSetClaimRecipients` before E. This is a native chain message, not a Factory
or Deal execute. Obtain its exact CLI/protobuf form from the selected Gonka
runtime. Query native routing and the Deal config before funding.

## Fund, lock and settle

Buyer calls `Send` on the configured CW20, not `Transfer` and not a manually
constructed Deal `Receive`. `eyJmdW5kIjp7fX0=` is base64 of `{"fund":{}}`:

```json
{"send":{"contract":"<DEAL>","amount":"100000000","msg":"eyJmdW5kIjp7fX0="}}
```

The authenticated token's hook supplies Buyer identity. Deposit exactly the
immutable budget before E; partial/excess deposits and routing errors reject the
transaction, including the CW20 transfer. Direct token transfers are donations
and do not fund an offer.

At the start of E, any keeper/caller executes:

```json
{"lock":{}}
```

Confirm `Locked` and `recipient_locked=true`. Lock is available only in E..E+4;
do not postpone it to the pruning boundary. A Deal without a Buyer can also Lock.

The native Host/authorized claimant performs `MsgClaimRewards` for E in the
runtime's claim window (the pinned design expects E+1). Automatic native claim
may instead confirm it; inspect the exact Host/E summary before sending another
claim. The Deal does not sign or perform native claims. Once the exact summary
has `claimed=true`, anyone executes:

```json
{"settle_claim":{}}
```

This accrues debts without transfers. Query pending payments and submit each
non-zero role in a **separate confirmed transaction**:

```json
{"usdt_payments":{}}
{"withdraw_usdt":{"role":"host"}}
{"withdraw_usdt":{"role":"fee"}}
{"withdraw_usdt":{"role":"buyer"}}
```

Continue to other roles when one transfer fails, then re-query and retry its debt
after resolving the token restriction. Bundling roles or settlement/withdrawals
in one Cosmos transaction makes them atomic together and defeats this isolation.
Amounts and recipients cannot be supplied by the caller. No-sale creates no
USDT debt; zero settlement creates only the full funded Buyer debt.

## GNK release and alternate outcomes

When the Deal's spendable ngonka balance is positive, execute:

```json
{"release_unlocked_gnk":{}}
```

Read the frozen `gnk_release_policy` and lifetime counters, rather than assuming
Host-only excess or a claim-sized cap. The same policy continues after Completed
and for late donations/unlocks; pending USDT does not block release. In Completed,
the compatible `{"forward_excess_gnk":{}}` alias has identical economics.

No-sale cancellation uses `{"cancel":{}}` while Open. Before E only Host may
call; in E..E+4 anyone may call. In both windows, the native routing query must
successfully prove missing/mismatch. Remove/update an exact route before E first.

`{"refund":{}}` selects a branch by current state:

- Funded: in E..E+4, successful missing/mismatched routing returns the exact budget.
- Locked: from E+2, exact `claimed=false` returns the budget or expires a no-sale.
- Locked: from E+3, permitted summary failures allow `network_unconfirmed`.

Exact `claimed=true` always blocks Refund. Failed current-epoch queries do not
permit it. These branches are atomic single-recipient payments, not pending pull
withdrawals. They freeze HostOnly GNK and never reopen after native recovery.
See [contract behavior](contract-behavior.md) for all guards and risk allocation.

## Queries

| Contract | Message | Interpretation |
| --- | --- | --- |
| Factory | `{"config":{}}` | Deal code ID, token, fee recipient/bps |
| Factory | `{"deal":{"id":1}}` | Registered address; missing ID errors |
| Factory | `{"deal_by_host_epoch":{"host":"<HOST>","epoch":42}}` | Permanent pair index; missing entry errors |
| Factory | `{"list_deals":{"start_after":null,"limit":20}}` | Ascending IDs; cursor is exclusive; default 20, max 50 |
| Deal | `{"config":{}}` | Immutable terms, calculated capacity, Factory/Host/token/fee and protobuf SHA |
| Deal | `{"state":{}}` | Status, Buyer, stored Lock proof, refund reason, accrued accounting and lifetime GNK counters |
| Deal | `{"funding":{}}` | Buyer/funded flag and configured budget/capacity; terms alone do not prove a deposit |
| Deal | `{"entitlements":{}}` | Work/reward/total, original ownership and frozen release policy |
| Deal | `{"release_status":{}}` | Lifetime GNK payments and clamped original remaining amounts |
| Deal | `{"native_status":{}}` | Diagnostic vesting total and liquid balance; may fail even while bank-only release succeeds |
| Deal | `{"usdt_payments":{}}` | Per-role recipient, accrued, paid and pending; Buyer recipient is null when absent |

Statuses serialize as `open`, `funded`, `locked`, `releasing`, `completed`,
`refunded`, `cancelled`, `expired`. Refund reasons are `routing_missing`,
`routing_mismatch`, `claim_expiry`, `network_unconfirmed`. Never treat
`Completed` as proof that USDT has been delivered or all native vesting has ended.

## Events, confirmation and retries

Key events are `offer_created`, `deal_funded`, `deal_locked`, `deal_cancelled`,
`claim_settled`, `usdt_paid`, `usdt_refunded`, `gnk_released`, `deal_completed`,
`deal_refunded`, `deal_expired`, and alias-specific `excess_gnk_forwarded`.
CosmWasm may prefix custom event types with `wasm-` in transaction output.
Settlement events report accrual; payment events report successful delivery.

Index only included successful transactions and reconcile with queries. A sync
broadcast hash is not confirmation. Failed transactions do not finalize events
or application state; their network fee can still be charged. Keep tx hash and
confirm its result before deciding to retry an ambiguous broadcast.

| Observation | Keeper response |
| --- | --- |
| Early epoch | Wait, re-read current state and native epoch |
| Correct Funded routing | Prioritize Lock; routing Refund must reject |
| Query/decode/identity failure | Restore native service; do not infer routing absence or zero claim |
| Permitted summary failure at E+3+ | Consider explicit emergency policy after checking current state; not an automatic action |
| Failed USDT role | Leave its debt pending, continue other roles in separate transactions |
| Failed GNK/refund transfer | Resolve cause, re-query state and balances, retry the atomic operation |
| `NothingToWithdraw` | Role already paid or no obligation; no zero transfer |
| `NothingToRelease` / `NothingToForward` | No spendable GNK; wait for a new balance |
| Pruned routing / E+5+ before Lock | Lock/Cancel/routing Refund unavailable; flag operational failure |

Contracts have no cron or automatic refund/unlock-distribution mechanism.
Keepers should monitor all deals, prioritize timely Lock, confirm native claim,
settle, retry outstanding USDT and distribute newly available GNK. They must
handle terminal releases, coincident addresses and per-deal isolation.
