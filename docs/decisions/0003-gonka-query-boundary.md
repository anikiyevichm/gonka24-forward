# ADR-0003: Safe Gonka Query Boundary

- Status: Implemented; ADR-0011 supersedes the blanket prohibition for positive exact summaries; `NotFound` remains a fail-closed liveness gap
- Date: 2026-09-06
- Gonka source: `gonka-ai/gonka@379bebced638aeb5e6077bfd51c986f898443832`

## Context

The Deal contract must make financial decisions based on native Gonka module state.
Custom gRPC returns protobuf bytes, but valid wire format alone does not establish
that the response relates to the expected Host, epoch, or Deal.

## Invariant

A state transition cannot consume Gonka or native balance results until a unified
internal boundary has verified the route, payload size, decode integrity, participant
identity, denom, and amount. Any error or ambiguity halts the transition.

## Specification Requirements

Four exact gRPC paths are authorized:

1. `GetCurrentEpoch`;
2. `ListClaimRecipients`;
3. `EpochPerformanceSummaryByParticipant`;
4. `TotalVestingAmount`.

The `ngonka` balance is read via standard `BankQuery::Balance`. Responses must be
validated against exact participant/epoch/address/denom fields; duplicate target
records and malformed or oversized payloads are rejected.

## Gonka Runtime Constraints

- Gonka runs wasmd `v0.54.2` / wasmvm `v2.2.4`.
- Pinned `AcceptedGrpcQueries()` does not yet register the four Marketplace routes.
- `MaxClaimRecipientLookahead = 40`, pruning threshold is `5`, but `ListClaimRecipients`
  lacks pagination and iterates the complete participant collection.
- The keeper returns gRPC `NotFound` when current epochs or performance summaries
  are absent. `TotalVestingAmount` without a schedule returns an empty coin list successfully.

## Official API and Source Behavior

Utilizes `GrpcQuery` capability from `cosmwasm_2_0`. `QuerierWrapper::query_grpc`
returns protobuf `Binary`, but flattens system and module errors into `StdError`.
wasmd additionally redacts non-deterministic subquery errors. Consequently, stable
gRPC status codes cannot be recovered by parsing error strings.

## Audited Reference Comparison

- DAO DAO: External boundaries and actual liquidity must be verified prior to
  payout; the adapter reads verified native balances independently.
- OroSwap finding regarding unbounded schedules/queries: Enforced limits of 32 KiB
  and 64 recipient entries; chain-side bounds remain a release prerequisite.
- Astroport findings regarding zero and amount handling: An empty vesting list holds
  an explicitly defined value of zero, while malformed, negative, or overflowing
  amounts are rejected.

These audits did not evaluate Gonka custom gRPC and do not establish wire compatibility.
GPL code was not copied.

## Decision

1. Generate bindings from committed recursive `.proto` closures, pinned to commit
   SHA and upstream Buf dependency commits.
2. Exclude the generator from contract workspaces and builds; committed Rust bindings
   are reproducible via an isolated host-only tool.
3. Re-export strictly necessary Marketplace wire types from `gonka-proto`.
4. Enforce semantic validation within `marketplace-deal::gonka`.
5. Prohibit accepting routes as public inputs.
6. Treat any node query failure as `QueryFailed` and fail closed.
7. Isolate `ClaimRecipientNotFound` strictly from successfully decoded lists;
   never classify node errors via string inspection.
8. Use `Uint128` and checked addition for financial sums.
9. Return `work_amount`, `reward_amount`, and `total_claim_amount` separately:
   the state machine must store both original components alongside their checked sum.

## Negative Scenarios and Proofs

Unit tests cover:

- Invalid route and request payloads;
- System failures without leaking node error text;
- Malformed and oversized protobuf payloads;
- Missing, duplicate, invalid, and mismatched recipients;
- Entry count boundaries;
- Missing or mismatched performance summaries;
- Unexpected or duplicate denoms;
- Empty, negative, non-numeric, and overflowing amounts;
- Checked arithmetic overflow;
- Standard `ngonka` Bank balance queries.

Golden tests validate encoding and decoding across all four request/response pairs
against bytes compiled by pinned `protoc` from snapshot `.proto` files.

Final local verification under Rust `1.81.0` produced 22 passing tests: 4 golden
protobuf tests, 15 Deal tests, and 3 Factory tests. `fmt`, Clippy under `-D warnings`,
both release Wasm compilations, and `cosmwasm-check 2.2.2` succeeded. Deal Wasm
increased from 243,178 to 246,241 bytes (`+3,063`), while Factory remained at 243,203
bytes. This minimal footprint confirms that the linker pruned unused elements from
the large generated proto closure.

Regeneration reproducibility is asserted by comparing SHA-256 hashes of all committed
generated files before and after execution. `cargo audit` and `cargo deny` pass for
both contract and generator lockfiles; documented allowed warnings for `derivative 2.2.0`
and `paste 1.0.15` remain in the main lockfile.

## Residual Risk and Release Gate

Mocks validate byte handling, but not route registration or live Gonka keeper dynamics.
Prior to production release, an upstream Gonka PR with exact allowlists, Go tests,
proven gas bounds for recipient lists, and golden E2E is required.

Limits of 32 KiB and 64 entries protect only Wasm decoding and subsequent execution.
The Gonka keeper builds the full list prior to invocation, so local bounds do not
restrict chain-side workload. A lookahead of `40` epochs does not bound historical
entries if pruning lags. Chain-side gas/size bounds remain a mandatory release gate,
not an eliminated risk.

This reflects the liveness and gas risks documented for unbounded processing in
Astroport finding #2 and OroSwap HAL-02/HAL-16. These audit findings confirm the
vulnerability class, but do not prove specific safety bounds for the Gonka keeper.

The specification requires distinguishing missing performance summaries from
transport/decode failures for `Refund/Expired`. Consequently, prior to integrating
those transitions, one of two paths is required:

1. Gonka returns an explicit `found`/optional payload within successful protobuf
   responses (or provides a dedicated deterministic route), verified by adapters and E2E;
2. The specification adjusts `Refund/Expired` preconditions to avoid relying on
   summary absence as proof for refunds.

The current generic `QueryFailed` safely halts payouts, but does not guarantee
refund liveness. This PR alone is therefore insufficient justification to implement
`Refund/Expired`. String parsing is rejected as brittle and non-deterministic.
