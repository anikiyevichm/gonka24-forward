# ADR-0004: CreateOffer and Atomic Factory Reply

- Status: Accepted and implemented
- Date: 2026-09-06
- Base: `main@8f6662cbd0766142e52fb89d90d4edc614b6318d`

## Decision

The Factory treats the caller as the sole source of Host identity, validates the offer,
stores a minimal `PendingOffer`, and instantiates the Deal via `SubMsg::reply_on_success`
using compile-time reply ID `1`. The reply handler accepts exactly one response of type
`/cosmwasm.wasm.v1.MsgInstantiateContractResponse`, decodes and validates the address,
atomically writes both indices, increments `NEXT_DEAL_ID` using checked arithmetic,
and clears the pending context.

## Invariant

- Exactly one canonical Deal may exist for any `(Host, target_epoch)`; the index is
  never cleared during terminal transitions.
- `NEXT_DEAL_ID` increments strictly following successful instantiate replies.
- Unexpected, duplicate, or malformed replies never alter the registry.
- Failures during child instantiation or reply processing leave no pending state,
  no indices, no altered counters, and no orphaned child contracts.
- Factory accepts no assets in `CreateOffer`, sends no funds to Deal, and receives
  no migration admin privileges over Deal.

## Gonka and Source Constraints

MVP §3.2 and §6.1 require `target_epoch > current`, lookahead of at most 40,
non-zero price/budget/capacity, caller as Host, Factory config passed to Deal,
compile-time Gonka SHA, `admin: None`, and perpetual uniqueness of Host/epoch pairs.
Factory calls compile-time route `/inference.inference.Query/GetCurrentEpoch` exclusively;
transport, size, and decode errors fail closed without parsing node error strings.
Capacity math and this narrow query are implemented in stateless `marketplace-common`,
but Factory and Deal invoke shared helpers independently and apply their own contract-level
checks. Consequently, Deal constraints cannot be bypassed by forging Factory messages.

## Pinned Runtime and API

`VERSIONS.md` pins `cosmwasm-std 2.2.2`, `cw-utils 2.0.0`, and Gonka `wasmd v0.54.2`.
In CosmWasm 2.x, `SubMsgResponse.msg_responses` is the primary channel for Cosmos SDK
message responses; deprecated `data` is retained for legacy runtimes. The implementation
requires exact `msg_responses` type URLs and decodes protobuf values via version-pinned
`cw_utils::parse_instantiate_response_data`. Empty, multiple, mismatched-type, or
malformed responses are rejected.

## Audited Patterns and Specific Findings

DAO DAO audit base `0b5cae57fecbbadb1045f3dc2bb4ad4fe5a98ee8` included
`contracts/external/cw-payroll-factory` and confirms applicability of the
factory→submessage→reply pattern. Snapshot `0178cf55d358356474e5530cccac6acdccd0d94b`
is post-audit and not assumed audited.

OroSwap HAL-08 regarding stale pending operations relates to `contracts/periphery/pool_initializer`
on assessed base `59f095b…`; [remediation commit `2fa02f9e30c3dbc80f84f47382ef73017ff0e8b9`](https://github.com/oroswap/oroswap-core/commit/2fa02f9e30c3dbc80f84f47382ef73017ff0e8b9)
introduced guards against existing pending operations. We adopt the vulnerability class
and state-machine pattern without copying GPL code or business logic.

## Alignment and Divergence

We align with the factory/reply pattern, but omit reference owner/admin/migration models.
Pending context is a single `Item` because CosmWasm processes submessages and replies
synchronously within a single transaction; concurrent unfinalized `CreateOffer` operations
do not exist across transactions. The guard fails closed if unexpected pending state
is encountered, preventing future overwrites.

All reply validations and checked increments occur prior to first writing to the registry.
Once writing begins, storage errors are safely handled by chain-level transaction rollbacks.
`PENDING_OFFER.remove` executes last; should a late failure occur after removal, the
transaction rollback restores the previous state.

## Test Verification

- Unit: exact instantiate message/config/SHA/admin/funds; invalid terms, lookahead,
  capacity, and native query failures; duplicate Host/epoch; pending overwrites;
  unknown/missing/duplicate/error/empty/multiple/mismatched/malformed replies; invalid
  addresses; pending mismatches; collisions; ID overflows.
- `cw-multi-test`: Factory instantiates Deal, both indices and Open config are accurate,
  IDs are sequential, `ContractInfo.admin = None`, migration to new code is rejected,
  and code ID remains unchanged.
- `cw-multi-test`: deliberate child instantiate failure and reply ID overflow demonstrate
  complete rollback; recorded test address of child contract is absent after failure,
  pending/registry remain empty, and counter retains its prior value.

## Residual Limitations

`cw-multi-test` models wasmd transactionality, but does not replace golden E2E on
live Gonka. Allowlisted native routes, gas/size bounds, and CWA-2025-007 runtime
patches/attestations from `SECURITY.md` remain release blockers. CreateOffer does
not implement funding, Lock, settlement, release, Refund, Expired, or ForwardExcessGnk.
