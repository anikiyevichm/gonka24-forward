# ADR-0014: Independent USDT Withdrawals After Settlement

- Status: Implemented
- Supersedes atomic settlement payouts in [ADR-0008](0008-settle-claim-usdt-payouts.md); amount calculations and GNK shares are preserved.
- Motivation: Remediation of R5 settlement recipient blocking finding.

## Decision

Option 2 was selected: `SettleClaim` records entitlements, status, and debts in a single transaction without invoking CW20 transfers. `PendingUsdt` stores Host/Fee/Buyer balances in a dedicated storage item; existing amounts in `DealState` remain accrued cumulative totals. The absence of the storage item signifies no outstanding pull debt: before settlement and after existing atomic Refund branches. Query `UsdtPayments` computes `paid = accrued - pending`.

`WithdrawUsdt { role }` is permissionless and non-payable. It clears only the debt of the specified role before dispatching a single CW20 `Transfer` to the immutable recipient. Failure rolls back this withdrawal; retries are permitted. Zero outstanding debt yields `NothingToWithdraw`. Coincident addresses do not merge roles. Live balance does not affect obligations, and unallocated USDT cannot be withdrawn via any administrative method.

GNK release is permitted upon unlock prior to actual delivery of USDT. This is a deliberate economic design: Host funds are reserved, but may not yet have been delivered. `Completed` concludes the initial GNK phase and does not extinguish USDT obligations. Neither `SettleClaim` nor `Refund` can be repeated once the outcome is finalized. Refund branches remain atomic; their recipient is singular, representing a distinct behavioral domain.

Event `claim_settled` denotes accrual; `usdt_paid`/`usdt_refunded` are emitted in the successful transaction of the respective payout. Clients must query `UsdtPayments` rather than interpreting amounts in `State` as proof of delivery.

The keeper submits each role in a separate confirmed transaction, continues after any role failure, and re-queries state prior to the next pass. The acceptance helper `withdraw_pending_usdt` implements this pass; repeating native claim or settlement is not required for payouts. Local acceptance command `withdraw-usdt --context ...` executes a retry pass. Bundling multiple roles or settlement with payouts into a single transaction re-couples them atomically.

## Verification and Deployment

`cw-multi-test` validates persistent blocking of each role, retention of debt upon failure, successful payment to remaining parties, GNK release prior to debt clearance, and retry after `Completed`. Additionally verified: zero claim, dust, identical addresses, external transfers, permissionless callers, native funds rejection, and isolation across multiple deals.
Given a budget of 100 USDT and gross of 80 USDT, the current 150 bps fee yields 78.8 Host, 1.2 Fee, and 20 Buyer refund; protocol rates were not altered.

API schemas and acceptance flow are updated. This is a code change for new deployments: active instances are instantiated without an admin, and migration entry points are absent. Existing instances will not receive this update automatically. Previous release/evidence artifacts do not confirm this new behavior; fresh release builds and chain acceptance are required before production release. Local Wasm compilation does not substitute for optimizer/release evidence.

A token-wide escrow freeze can still block all USDT delivery. Recipient isolation does not override global token restrictions or non-standard token semantics.
