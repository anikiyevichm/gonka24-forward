# ADR-0009: Cumulative GNK Release and Forwarding Excess

> The release/cap/excess decision was superseded and implemented in PR #10 under [ADR-0010](0010-permanent-gnk-shares.md) dated 2026-09-07. Below is the historical description of the earlier implementation, rather than current active requirements. Capacity and USDT settlement are unchanged.

- Status: Accepted and implemented
- Date: 2026-09-07
- Base: `origin/main@b11838f445910baf44f5d6de8b25f670a0df8159`
- Pinned Gonka source: `379bebced638aeb5e6077bfd51c986f898443832`

## Decision

Permissionless `ReleaseUnlockedGnk` is permitted strictly from `Releasing` and does not accept amounts or recipients from the caller. The contract uses frozen claim, entitlements, and release counters persisted during settlement. Performance summary and recipient routing are not re-queried.

Prior to native queries, the following invariants are checked:

    work + reward = total_claim
    buyer_entitlement + host_entitlement = total_claim
    buyer_released + host_released = released_total

Following this, `TotalVestingAmount(Deal)` returns remaining vesting, standard `BankQuery::Balance(Deal, ngonka)` returns available bank balance, and the single calculation formula resides in A2 `calculate_cumulative_release`. The contract boundary independently verifies `released_total`, because the A2 helper derives previous total from the two side counters and does not take the stored `released_total` field as input.

New cumulative targets and `Completed` status are persisted prior to dispatching outgoing `BankMsg` messages. Only non-zero Buyer deltas followed by Host deltas are sent. A non-zero Buyer delta without a recorded Buyer is an error. Any query/decode/denom/amount, state, or accounting error aborts execution without mutations or messages. `NoNewRelease` from A2 is mapped to typed `NothingToRelease`.

`ForwardExcessGnk` is permitted strictly in `Completed`, requires exact final accounting, and forwards the entire current `ngonka` balance exclusively to the immutable Host. CW20 tokens, other denominations, and frozen fields remain untouched. A zero balance returns the selected A6 typed error `NothingToForward` without emitting a zero `BankMsg` or dummy event. Later arrivals can be forwarded via subsequent calls.

## Why Bank Balance Applies Specifically to Pinned Gonka

This is not a generic assumption that any Cosmos bank balance is spendable. Pinned source code retains locked streamvesting funds within a module account:

- `x/inference/keeper/payment_handler.go::PayParticipantFromModule` invokes `AddVestedRewards` when vesting period is positive;
- `x/streamvesting/keeper/keeper.go::AddVestedRewards` transfers coins from the `inference` module to the `streamvesting` module and stores the schedule by address;
- `ProcessEpochUnlocks` transfers only the first unlocked tranche from the `streamvesting` module to the Deal account, then prunes that tranche;
- `x/streamvesting/keeper/query.go::TotalVestingAmount` sums remaining schedule entries held in the module.

Consequently, the locked amount does not reside in the Deal's bank account, and incoming `ngonka` balance is spendable via `BankMsg::Send` in this model. Pinned `contracts/liquidity-pool/src/contract.rs` uses the same `query_balance` plus `BankMsg::Send` pattern. `cw-multi-test` does not implement a real streamvesting keeper, so the B5 golden E2E test must confirm transferability of the real unlocked balance; until then, this conclusion is source-positive, not chain-verified.

## Cumulative Accounting and Rounding

For cumulative released total `U`, Buyer target equals `floor(U * buyer_entitlement / total_claim)`, and Host target is the remainder of `U`. The current transfer is the difference between target and previous counter. Rounding each tranche separately is prohibited: that would make the final outcome dependent on the number of invocations. At `U = total_claim`, the Buyer receives exact entitlement, and the Host receives the entire rounding remainder.

A liquid donation increases balance, but not the unlock cap. A vested donation could increase remaining vesting above total or push the current cap below what has already been distributed. This yields `NothingToRelease`, not an underflow: counters are monotonic.

## Security and Reference Review

### DAO DAO

The Oak report dated 2023-03-22 verified base `0b5cae57fecbbadb1045f3dc2bb4ad4fe5a98ee8`; exact scope: `contracts/external/cw-vesting`, `contracts/external/cw-payroll-factory`, and `packages/cw-wormhole`. In post-audit snapshot `0178cf55d358356474e5530cccac6acdccd0d94b`, `Payment::distributable` caps payouts simultaneously by liquid amount and claimable entitlement, while `distribute` updates claimed state prior to issuing the transfer message. The snapshot includes further changes and is not considered fully audited.

Finding #2 (Major) showed stranded or stealable excess CW20 when configured total did not match received amount; remediation `fadf2e4a2bbcc6363ca06c1e6e7bb0c745f00f99` enforced exact equality. For A6, this risk class translates to strict coupling between frozen entitlement, counters, and liquidity. Distinction: Gonka unlocks are governed by native streamvesting, rather than time or DAO DAO owner policy. Tests verify insufficient balance, donations, exact final counters, and atomic rollback of both `BankMsg` transfers.

### Astroport

The Oak report dated 2023-04-04 covered only Maker changes at `1f50cabf6738f6ad57b6ed7b1d56f1276fe6d526` and Vesting changes at `042b0768951422099f5d77224c320978cbfa92cc`. Vesting tracks cumulative `released_amount`; this serves as a pattern comparison, not a source of Gonka formulas. Finding #2 (Minor) regarding unbounded iteration over schedules was marked `Resolved`, but no remediation SHA is cited. A6 performs constant-time operations with two fixed recipients. Finding #8 (Informational) regarding confusing zero native withdrawals was also marked `Resolved` without a remediation SHA; A6 returns `NothingToRelease` or `NothingToForward` before messaging. Events `gnk_released`, `deal_completed`, and `excess_gnk_forwarded` provide complete observability. No Astroport GPL code was copied.

### OroSwap

The Halborn report (2025) assessed the dedicated pool-initializer base `59f095b...` and primary Factory/Vesting scope `9042989f8fd00b6524b470a5850ec03f8e5a2e4e` (162 files, including `contracts/tokenomics/vesting`). HAL-01 (Medium) showed that permissionless collect with a caller-controlled small limit could update the cooldown and grief keepers; remediation `f7566310fd7ab86c080f4c7aa77deee4eb94407f` eliminated this vector. An A6 caller cannot configure amount, cap, recipient, or progress marker, and zero progress does not mutate state.

HAL-10 regarding missing balance validation was fixed in `e44c386b90d390f4482540453a1e6fb32d1f6382`; A6 always bounds releases by actual Deal balance. HAL-11 regarding schedule limit bypass was fixed in `345aa61cb98e2cca83a25783ba3d75c458091be8`; A6 accepts no schedules and maintains strictly two fixed recipients. OroSwap time-based vesting and admin powers are not ported, and no GPL code was copied.

## Test Verification

Unit tests verify state gates, native funds rejection, targets/deltas/events, Buyer-only scenarios, no-sale deals, suppression of zero transfers, vested-donation no-ops, absence of Buyer, three boundary accounting mismatch checks, and forwarding without mutation.

`cw-multi-test` exercises real Factory, Deal, CW20, and bank ledger:

- CreateOffer -> Fund -> Lock -> SettleClaim -> multiple Releases -> Completed -> ForwardExcessGnk;
- Diverse splits of a single total, mixed/Buyer-only/Host-only, and coincident recipients;
- Insufficient liquidity, liquid/vested donations, late excess, zero settlement, CW20 and unrelated denom preservation;
- Malformed/query/denom/amount failures in vesting and bank queries;
- Test-only bank wrapper injects failures on the first or second non-zero release send, including the second after a successful first; counters, Completed state, and balances roll back completely, after which a retry succeeds without double-payment;
- Forwarding failure preserves balances and accounting and permits exact retry.

The fault wrapper is isolated strictly to integration tests and delegates successful operations to the real `cw-multi-test::BankKeeper`.

## Local Verification

- Workspace and proto-generator format / Clippy with `-D warnings` — pass;
- Workspace suite — 104 tests passed; proto-generator tests — pass;
- Regenerated 43 protobuf inputs and both contract schemas — clean diff;
- Standard release Wasm and both optimized Wasm binaries — `cosmwasm-check 2.2.2` pass;
- Two runs of pinned optimizer `cosmwasm/optimizer:0.16.1@sha256:b9c92b…69e` produced identical SHA-256 hashes: Deal `de087632…b1e49`, Factory `16f0510b…b1799`;
- `cargo-audit 0.22.2` for both lockfiles — no vulnerability findings; two resolved unmaintained warnings for `derivative 2.2.0` and `paste 1.0.15` remain;
- `cargo-deny 0.20.2` for both lockfiles — advisories, bans, licenses, and sources pass; documented duplicate/unmatched-license warnings remain.

## Residual Risks and Scope Boundary

A6 does not modify the Gonka API, nor does it implement native claim, Refund/Expired, keeper functionality, or arbitrary withdrawals. B1/B5 verification, CWA-2025-007 runtime gate, real contract-recipient/bank-send E2E, and acceptance of vested-donation liveness remain mandatory prior to production. Unit, property, and multi-test suites do not substitute for golden E2E testing on real Gonka.
