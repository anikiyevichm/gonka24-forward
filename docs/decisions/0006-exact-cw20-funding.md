# ADR-0006: Exact CW20 Funding and Native Routing Verification

- Status: Accepted and implemented
- Date: 2026-09-07
- Base: `origin/main@129ca30a63b350ab9fb69b5b4da30ec80ccec7ac`
- Pinned Gonka source: `379bebced638aeb5e6077bfd51c986f898443832`

## Decision

The Buyer funds an Open Deal via a single `Cw20ExecuteMsg::Send` covering the entire immutable `buyer_budget_micro_usdt`. The Deal accepts the existing `Receive/Fund` hook exclusively from the configured settlement CW20 contract, validates the original sender as the Buyer, requires `current_epoch < target_epoch`, and verifies that a successful `ListClaimRecipients(Host)` query returns exactly one entry for the target epoch with its recipient equal to the address of this Deal.

All validations complete prior to mutating state. A successful call records only `buyer` and `status = Funded`, leaves configured terms unchanged, creates no outgoing messages, and emits `deal_funded` with Deal, Host, epoch, Buyer, and full budget. The deposit remains in the Deal's CW20 balance.

## Our Invariants

- State can transition to `Funded` only once, and strictly from `Open`.
- The sole recorded Buyer is extracted from `Cw20ReceiveMsg.sender`, but only after verifying that the immediate `info.sender` matches the configured CW20 token contract.
- The actual deposit strictly equals the configured budget down to the smallest atomic unit: `received_amount == buyer_budget_micro_usdt`.
- Funding is permitted only before E (`current_epoch < target_epoch`) and only with unambiguous exact routing `(Host, E) -> env.contract.address`.
- Query errors, malformed/oversized protobuf responses, invalid/mismatched recipients, missing entries, or duplicate target entries do not constitute proof of routing.
- On any error, Deal state and CW20 balances remain unchanged; neither the existing deposit nor the first Buyer can be overwritten by subsequent funding attempts.
- Funding creates no payouts to Host, fee recipient, or Buyer.

## Trust Boundary and Validation Order

`info.sender` and `Cw20ReceiveMsg.sender` represent distinct roles. The former is the immediate contract caller and must be the configured token contract. The latter is the original account that invoked CW20 `Send`; only after verifying the token contract is it treated as the source of the Buyer identity and passed through `addr_validate`. Hook payload fields do not specify Buyer or financial terms.

Fail-closed validation order: absence of native funds; configured CW20; decode hook; valid Buyer; `Open` state; exact amount; current epoch; exact recipient; state write. Epoch and routing validations utilize existing shared/native adapters: `marketplace_common::gonka::query_current_epoch` and `marketplace_deal::gonka::query_claim_recipient`. Financial math and semantic validation logic are not duplicated.

The configured CW20 remains a trusted external dependency: `TokenInfo` and address are validated during Deal creation, but an arbitrarily malicious token contract could falsify its own accounting. The production token address is pinned prior to deployment per the release process.

## Gonka / Source Constraints

MVP §§7.1, 8, 12, and 14 mandate exact amount, `current < E`, a single target entry, and recipient equal to the per-deal contract address. Pinned `ListClaimRecipients` accepts Host in the request, while each response entry contains epoch and recipient. Host identity is therefore verified by constructing the exact request, epoch by filtering for exactly one matching entry, and recipient via `addr_validate` and equality check against `env.contract.address`.

Contract-side limits (32 KiB / 64 entries) and `cw-multi-test` do not prove route registration, real protobuf byte encoding, pruning, or Gonka keeper gas bounds. Golden E2E on real Gonka and chain-side allowlist/gas evidence remain mandatory release gates per ADR-0003 and `SECURITY.md`.

## DAO DAO / Oak Comparison and Audit Finding

The Oak report dated 2023-03-22 was directly reviewed. Page 5 defines audit base `0b5cae57fecbbadb1045f3dc2bb4ad4fe5a98ee8` and scope: `contracts/external/cw-vesting`, `contracts/external/cw-payroll-factory`, and `packages/cw-wormhole`. Finding #2 (Major, pages 10–11) describes a mismatch between configured vesting total and a larger received CW20 amount: the excess remained stuck in the Factory and could be appropriated by a subsequent caller.

The report marks this finding `Resolved` via exact commit `fadf2e4a2bbcc6363ca06c1e6e7bb0c745f00f99`. Inspection of the commit shows that remediation added `receive_msg.amount == instantiate_msg.total` equality checks in `cw-payroll-factory`; post-audit snapshot `0178cf55d358356474e5530cccac6acdccd0d94b` is not considered fully audited.

The applicable pattern is exact equality between the received amount and configured total before funds can be utilized further. Marketplace distinction: our Deal does not spawn a downstream vesting contract or forward the deposit upon funding; it retains the exact budget in an isolated per-deal escrow and additionally validates token identity, Buyer, state, epoch, and Gonka routing. No DAO DAO code was copied. No GPL code from OroSwap or Astroport was used.

## Test Verification

Unit tests verify successful state/event/config immutability and all local boundary conditions: fake/wrong caller, malformed hook, invalid Buyer, under/over amount, repeated funding by the same or different Buyer, E/E+1 boundary, invalid state, missing/mismatched/invalid/duplicate recipient, current/recipient query failure and decode failure, native funds rejection. Any failure preserves `DealState::open()` or previously Funded state without modifications.

`cw-multi-test` uses real Factory, Deal, and `cw20-base`: `CreateOffer -> Deal -> mocked protobuf routing -> Buyer Send`. Success verifies exact Buyer/Deal/Host/fee balances, state, query, and event. The negative test matrix verifies balances before/after each failed real `Send`: CW20 transfer to Deal and hook execution are atomic, so a hook failure rolls back the transfer. Following initial funding, subsequent funding attempts preserve the original Buyer and exactly one budget in Deal escrow.

Native responses in these tests are protobuf-aware mocks. They verify request path, Host, decode/validation, and contract reaction, but do not substitute for a real Gonka test.

## Local Verification

- `cargo fmt --all --check` and format generator tool — pass;
- Clippy workspace/all targets/all features and generator with `-D warnings` — pass;
- Workspace tests — 68 passed; generator tests — pass;
- Regenerated protobuf and contract schema — clean diff;
- Standard release Wasm and both optimized Wasm binaries — `cosmwasm-check 2.2.2` pass;
- Two runs of pinned optimizer image `cosmwasm/optimizer:0.16.1@sha256:b9c92b…69e` produced identical SHA-256 hashes: Deal `a53b327b…ecc497`, Factory `16f0510b…6b1799`;
- `cargo-audit 0.22.2` for both lockfiles — no vulnerability findings; two resolved unmaintained warnings for `derivative 2.2.0` and `paste 1.0.15` remain in the main graph;
- `cargo-deny 0.20.2` for both lockfiles — advisories, bans, licenses, and sources pass; resolved duplicate/unmatched-license warnings remain.

Golden E2E on real Gonka was not performed here: the test harness uses native query mocks. This is an explicit open release gate, not an overlooked local check.

## Scope and Next Step

A3 does not implement `Lock`, `Cancel`, `SettleClaim`, `ReleaseUnlockedGnk`, `Refund`, keeper functionality, or Gonka API changes. The next contract milestone is A4 routing lifecycle; real-chain gates are executed in parallel under workstream B.
