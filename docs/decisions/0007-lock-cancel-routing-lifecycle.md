# ADR-0007: Lock/Cancel and Routing Lifecycle Proof

- Status: Accepted and implemented
- Date: 2026-09-07
- Base: `origin/main@73420bd3d4091d7f110e3b25acb85f6bebea3e2c`
- Pinned Gonka source: `379bebced638aeb5e6077bfd51c986f898443832`

## Decision

The existing `Lock {}` and `Cancel {}` entrypoints are implemented without changing the public schema. Both execute handlers first validate state, caller, and epoch, then fetch a single shared validated result from `ListClaimRecipients(Host)`:

- `Missing` — target epoch is absent from the successfully decoded response;
- `RoutedTo(validated Addr)` — target epoch is present exactly once and the recipient is a valid address;
- `Err` — query/decode/size/entry-limit/address/duplicate failure.

`Lock` accepts only `RoutedTo(env.contract.address)`, strictly from `Open` or `Funded` states, and strictly within `E <= current < E+5`. Success sets `recipient_locked = true` and `status = Locked`. This provides an explicit snapshot of positive routing proof: future settlement of a Locked deal does not need to re-query the recipient row, which may later be deleted by pruning.

`Cancel` accepts strictly `Open` deals without a Buyer. Before E (`current_epoch < target_epoch`), the caller must be the immutable Host; within `E <= current < E+5`, the caller may be anyone (permissionless). Success requires `Missing` or a valid recipient address different from the Deal. Exact routing to the Deal forbids Cancel. A technical `Err` is never converted into permission to cancel.

Both transitions complete all validations before `STATE.save`, create no outgoing messages, and emit either `deal_locked` or `deal_cancelled` with Deal, Host, and target epoch. Configured budget/capacity, Buyer, and financial counters are unmodified. Cancel does not call the Factory and therefore preserves the permanent `(Host, E)` index.

## Our Invariants

- `Locked` is reachable only with fresh, unambiguous exact `(Host, E) -> Deal` routing proof.
- No-sale `Open -> Locked` preserves the absence of a Buyer and deposit, without zeroing out immutable configured budget/capacity.
- `Cancelled` is reachable strictly from a no-sale `Open` deal when a successful native response proves missing or mismatched routing.
- A permissionless caller cannot select recipient, Buyer, Host, amount, or new state: the empty execute message merely triggers a deterministic verification.
- `current >= E+5` never treats the absence of a target row as historical proof, even if the response is successfully empty.
- Repeated Lock/Cancel attempts and funding after terminal routing transitions are rejected by the state machine; CW20 `Send` rolls back atomically along with the hook error.
- Any native funds are rejected prior to dispatch via `nonpayable`.
- `E+5` is computed using `checked_add`; overflow yields a typed rejection before any mutation.

## Pinned Gonka Source Constraints

Verified against exact commit `379bebced638aeb5e6077bfd51c986f898443832`:

- `x/inference/keeper/msg_server_set_claim_recipients.go::applyClaimRecipientEntry` requires `entry.Epoch > currentEpoch`; once epoch E starts, the Host can no longer modify or delete the routing for E;
- The same code permits an empty recipient only as a deletion of a future entry, and limits lookahead with a constant of `40`;
- `x/inference/keeper/pruning.go::GetClaimRecipientPruner` uses `ClaimRecipientPruningThreshold = 5`;
- `getEpochsToPrune` computes the last eligible epoch as `currentEpochIndex - threshold`, so epoch E becomes eligible for deletion for the first time at `current = E+5`;
- `x/inference/keeper/query_list_claim_recipients.go::ListClaimRecipients` returns a successful empty list when rows are absent, but returns a gRPC error upon an error reading schedule.

Thus, `E..E+4` is the only window where the row is already immutable under native rules and not yet eligible for pruning. Pruning is limited to 1,000 deletions per block and may lag, but that does not make subsequent absence a proof: at `E+5`, the row *could* already have been pruned. The actual deletion timing and gRPC route availability still require the Gonka golden E2E test.

## Audited Reference Review

The DAO DAO Oak audit dated 2023-03-22 verified exact base `0b5cae57fecbbadb1045f3dc2bb4ad4fe5a98ee8` and scope `contracts/external/cw-payroll-factory`, `contracts/external/cw-vesting`, and `packages/cw-wormhole`. In the exact audited `cw-vesting`, cancellation by the source owner passes authorization and triggers payment cancellation. The general pattern of explicit state transitions and transaction rollback is applicable, but this business policy is intentionally not adopted: Marketplace cancels only unfunded `Open` deals, transfers nothing, and provides no right for the Buyer to cancel a purchase.

OroSwap Halborn review for Factory/Vesting assessed commit `9042989f8fd00b6524b470a5850ec03f8e5a2e4e`. HAL-01 describes griefing via permissionless `Collect`; remediation commit `f7566310fd7ab86c080f4c7aa77deee4eb94407f` adds authorized keepers for Tokenomics/Maker. HAL-02 describes gas DoS via external schedules; remediation commit `57590dd2bd720432430510206789b34d23f2d626` introduces a schedule creation fee in Tokenomics/Incentives.

Marketplace does not mechanically copy either fix and does not introduce authorized keepers: permissionless Lock/Cancel are necessary for liveness, their message bodies are empty, and state, epoch, and routing are strictly determined by the contract. The caller cannot choose asset, transfer, or schedule. Each transition performs a separate current-epoch query and one recipient-list query. Limits of 32 KiB / 64 entries constrain only the response payload, decoding, and further processing in Wasm; they do not bound the native keeper's workload when assembling the full list, so chain-side gas bounds remain an open release gate. OroSwap is licensed under GPL-3.0-only; none of its code was copied.

Astroport Maker/Vesting audited commits `1f50cabf6738f6ad57b6ed7b1d56f1276fe6d526` and `042b0768951422099f5d77224c320978cbfa92cc`: finding 1 notes time policy (partially resolved), finding 2 notes unbounded schedule iteration, and finding 5 notes missing instantiate events. They confirm the need for explicit windows, bounded processing, and observable events, but do not prove Gonka pruning. The audit report does not specify separate remediation commit SHAs for these findings, so they are not derived from post-audit history. Time-based vesting/cancellation logic is not ported to the Gonka lifecycle.

These audits do not include Gonka custom gRPC or pruning, nor do they validate our native semantics; the source of truth remains the MVP specification and the pinned Gonka codebase.

## Risks and Mitigations

| Risk | Fail-Closed Solution | Negative Test Proof |
|---|---|---|
| Arbitrary caller prematurely closes offer | Prior to E, Cancel requires exact Host caller | Non-host caller at E-1 is rejected without mutation |
| Permissionless griefing after E | Cancel permitted strictly for no-sale Open and strictly upon proven missing/mismatch routing | Funded/wrong/repeated states rejected |
| Query failure masked as absence | `Result::Err` cleanly separated from `Ok(Missing)` | Query/decode/oversize failures block Lock/Cancel |
| Duplicate or invalid target creates false proof | Target must be unique and address-validated | Duplicate/invalid tests block both transitions |
| Pruning erases historical evidence | Lock snapshot is persisted; Cancel window closes at E+5 | E+5 with empty response is rejected |
| Arithmetic wraparound widens window | `checked_add(5)` | Target near `u64::MAX` produces `RoutingWindowOverflow` |
| Repeated or late CW20 Send modifies escrow | Status gate and atomic CW20 hook rollback | Real `cw20-base::Send` preserves state/balances |
| Cancel frees `(Host, E)` slot | Factory index is immutable | Query returns same Deal; duplicate offer rejected |

## Test Verification

Unit tests cover Open/no-sale and Funded Lock; E-1/E/E+4/E+5; Host-only pre-E Cancel; permissionless E/E+4 Cancel; missing/mismatch/exact routing; invalid/duplicate/too-many/oversized/malformed/query failures; current-epoch query failures; wrong/repeated/terminal states; funded Cancel; native funds rejection; checked overflow; config/Buyer/accounting immutability and emitted events.

`cw-multi-test` instantiates real Deals via the Factory and executes real `cw20-base` Send for funded Lock and post-Lock/post-Cancel rollback. Tests verify exact state/config/CW20/native balances, specific negative error reasons, emitted events, permanent Factory index persistence, and rejection of duplicate `CreateOffer(Host, E)`.

The native adapter remains a protobuf-aware mock. This verifies request paths, Host, decoding/validation, and contract reaction, but does not substitute for golden E2E testing on real Gonka.

## Local Verification

- `cargo fmt --all --check` and format host generator — pass;
- Clippy workspace/all targets/all features and generator with `-D warnings` — pass;
- Workspace suite — 78 passed; generator tests — pass;
- Regenerated 43 protobuf inputs and both contract schemas — clean diff;
- Standard release Wasm and both optimized Wasm binaries — `cosmwasm-check 2.2.2` pass;
- Two runs of pinned optimizer `cosmwasm/optimizer:0.16.1@sha256:b9c92b…69e` produced identical SHA-256 hashes: Deal `304f5662…66ef51`, Factory `16f0510b…6b1799`;
- `cargo-audit 0.22.2` for both lockfiles — no vulnerability findings; two resolved unmaintained warnings for `derivative 2.2.0` and `paste 1.0.15` remain;
- `cargo-deny 0.20.2` for both lockfiles — advisories, bans, licenses, and sources pass; documented duplicate/unmatched-license warnings remain.

Golden E2E on real Gonka was not performed and remains an open release gate.

## Scope and Next Step

A4 does not implement SettleClaim, ReleaseUnlockedGnk, Refund/Expired, ForwardExcessGnk, keeper functionality, or native Host messages. Missing the Lock window provides no automatic refund or historical recovery. The next contract milestone is A5 settlement; Refund/Expired remains blocked by a separate native performance-summary absence dependency.
