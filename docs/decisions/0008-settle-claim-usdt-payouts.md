# ADR-0008: SettleClaim and Atomic USDT Payouts

- Status: Historical; atomic settlement payouts are superseded by [ADR-0014](0014-independent-usdt-withdrawals.md)
- Date: 2026-09-07
- Base: `origin/main@90f088abe69e0e284a03b40f374ce78d412b8b0a`
- Pinned Gonka source: `379bebced638aeb5e6077bfd51c986f898443832`

## Decision

Permissionless `SettleClaim {}` is permitted strictly from the `Locked` state with persisted `recipient_locked=true`. It does not execute the native claim: the Host must independently sign `MsgClaimRewards` on Gonka. The Deal queries the existing exact `EpochPerformanceSummaryByParticipant` for immutable Host/E, requires `claimed=true`, and passes only validated Work/Reward amounts into `marketplace_common::math::calculate_claim_settlement`.

When a Buyer is present, variant `BuyerFunding::Funded` uses the immutable configured budget, which A3 accepted strictly as the exact amount. The current CW20 balance does not participate in accounting: an accidental transfer does not increase capacity, Buyer rights, or payout amounts. In the absence of a Buyer, `BuyerFunding::NoBuyer` is used; configured offer terms remain unchanged, effective funding/USDT amounts are zero, and the entire GNK claim belongs to the Host.

The Deal persists Work, Reward, total, entitlements, gross, fee, Host net, and refund prior to generating outgoing messages. With a positive total, it transitions into `Releasing` with zero release counters and transfers no GNK. When `claimed=true, total=0`, it avoids calling the release formula, returns 100% of the deposit to the funded Buyer, and transitions immediately to `Completed`; a no-sale deal completes without transfers. Settlement has no E+2/E+5 deadline and does not repeat the recipient query: persisted Lock proof survives native pruning.

Non-zero CW20 transfers are sequenced in order: Host net, protocol fee, Buyer refund. Zero-amount messages and corresponding dummy events are suppressed. Failure of any submessage rolls back state and all preceding transfers within a single atomic Cosmos SDK transaction. Subsequent invocation attempts are rejected by the state gate.

## Invariants and Events

- The Host receives USDT strictly after a valid exact `claimed=true` summary is proven.
- `total_claim = checked Work + Reward`; the caller cannot supply evidence, amounts, or recipients.
- `buyer_entitlement + host_entitlement = total_claim`.
- `host_net + fee + refund = exact funded budget`; for no-sale deals, both funding parts and all USDT amounts equal zero.
- Release counters following A5 are initialized to zero; entitlement does not imply already available spendable GNK balance.
- Query/decode/size/identity failure, missing summary, and `claimed=false` leave state and balances unchanged and are never treated as zero-claim or refund evidence.
- The `claim_settled` event records all frozen amounts. `usdt_paid` is emitted separately for non-zero Host net and fee, `usdt_refunded` for non-zero refund, and zero settlement additionally emits `deal_completed`.

Generated protobuf specifies `earned_coins` and `rewarded_coins` as `u64`, so a separate string parsing amount error for performance summary is impossible: a malformed wire value triggers a decode error, and each decoded `u64` converts losslessly to `Uint128`. Checked addition and subsequent wide calculations remain mandatory.

## Security and Reference Review

### DAO DAO Exact Funding and Isolation

The Oak report dated 2023-03-22 verified base `0b5cae57fecbbadb1045f3dc2bb4ad4fe5a98ee8` and scope `contracts/external/cw-vesting`, `contracts/external/cw-payroll-factory`, and `packages/cw-wormhole`. Finding #2 (Major) showed stranded or stealable CW20 tokens when configured total differed from actual received amount; remediation `fadf2e4a2bbcc6363ca06c1e6e7bb0c745f00f99` enforced exact equality.

Our invariant matches this risk class: A3 accepts strictly the budget, A5 derives accounting from this fact, and dedicated Deals isolate deposits per instance. Distinction: DAO DAO distributes time-based vesting under owner policy, whereas Gonka claim and the permissionless state machine are governed by Marketplace rules. Snapshot `0178cf55d358356474e5530cccac6acdccd0d94b` is not considered fully audited. Tests verify that random direct CW20 transfers remain excess without altering settlement amounts, and Factory→Send→Lock→Settle validates exact ledger balances.

### OroSwap Fee Conservation

The Halborn review of the primary Factory/Vesting scope assessed commit `9042989f8fd00b6524b470a5850ec03f8e5a2e4e`. HAL-06 regarding fee over-allocation was resolved in remediation `308d8c64b8861524c17c796754091494f530d1a0`. HAL-10 regarding missing balance validation was resolved in `e44c386b90d390f4482540453a1e6fb32d1f6382`.

Our solution fixes the fee at 150 bps and applies the proven A2 conservation formula. Unlike OroSwap, the live CW20 balance intentionally does not dictate payouts: it may include donations, and liquidity shortage manifests safely as a transfer error triggering complete rollback. Tests verify conservation across under/exact/overproduction, dust, and coincident recipients. OroSwap is licensed under GPL-3.0-only; no code was copied, and findings serve solely as risk classifications.

### Astroport Zero Transfers and Observability

The Oak report dated 2023-04-04 covered only Maker changes at `1f50cabf6738f6ad57b6ed7b1d56f1276fe6d526` and Vesting changes at `042b0768951422099f5d77224c320978cbfa92cc`. Finding #8 mandated clear results prior to zero native withdrawals and was marked `Resolved`, but no specific remediation commit was listed. Finding #5 noted missing events; again no remediation SHA was provided.

Marketplace suppresses zero CW20 messages and events while emitting distinct financial events. Distinction: A5 distributes CW20 upon native evidence rather than Astroport time-based withdrawals. Tests cover zero fee, zero gross, no-sale, and funded zero-settlement scenarios. No Astroport GPL code was copied.

None of these audits cover our formulas, Gonka custom gRPC, or this contract. They do not constitute an audit of A5.

## Test Verification and Model Boundary

Unit tests verify state/proof/claimed gates, exact frozen accounting, message ordering, positive no-sale, funded/no-sale zero cases, emitted events, suppression of zero transfers, native funds rejection, and repeat settlement rejection.

`cw-multi-test` exercises real Factory, Deal, and CW20 contracts:

- CreateOffer → Send/Fund → Lock → mocked successful claim → SettleClaim;
- Under/exact/overproduction, Work/Reward variations, dust/rounding, and coincident recipients;
- Late settlement after E+5 without recipient row, and arbitrary direct CW20 transfers;
- `claimed=false`, missing/wrong/malformed/oversized/query-failed summaries;
- Open/Funded/Completed/repeat/native-funds failure cases;
- Test-only CW20 wrapper with real ledger simulating failures on 1st, 2nd, or 3rd non-zero transfer: state and all balances roll back completely, and removing the fault permits exactly one successful retry;
- Dedicated rollback test for full zero-claim refund.

Native performance and routing adapters remain protobuf-aware mocks. They verify Rust request/decode/validation logic and contract reactions, but do not prove reachability of `claimed=true, total=0`, custom gRPC allowlist, actual Gonka payout/retention, or real Cosmos SDK runtime behavior. Golden Gonka E2E and open B4/B5 trust dependencies remain mandatory release gates.

## Local Verification

- Workspace and proto-generator formatting — pass;
- Workspace/all-targets/all-features and proto-generator Clippy with `-D warnings` — pass;
- Workspace suite — 91 tests passed; proto-generator tests — pass;
- Regenerated 43 protobuf inputs and both contract schemas — clean diff;
- Standard release Wasm and both optimized Wasm binaries — `cosmwasm-check 2.2.2` pass;
- Two runs of pinned optimizer `cosmwasm/optimizer:0.16.1@sha256:b9c92b…69e` produced identical SHA-256 hashes: Deal `36e7147f…c150da`, Factory `16f0510b…6b1799`;
- `cargo-audit 0.22.2` for both lockfiles — no vulnerability findings; two resolved unmaintained warnings for `derivative 2.2.0` and `paste 1.0.15` remain;
- `cargo-deny 0.20.2` for both lockfiles — advisories, bans, licenses, and sources pass; documented duplicate/unmatched-license warnings remain.

## Scope

A5 does not implement native claim execution, `ReleaseUnlockedGnk`, `Refund/Expired`, `ForwardExcessGnk`, keeper functionality, or Gonka API modifications. Zero settlement transitions directly to `Completed`, compatible with future `ForwardExcessGnk`.
