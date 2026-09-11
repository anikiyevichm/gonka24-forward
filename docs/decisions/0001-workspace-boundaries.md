# ADR-0001: Initial Workspace Boundaries

- Status: Accepted
- Date: 2026-09-06

## Context

Gonka `streamvesting` aggregates schedules by recipient address. A shared escrow across multiple deals would therefore violate deal isolation.

## Decision

1. Deploy an isolated `marketplace-deal` for each deal via `marketplace-factory`.
2. Maintain the public wire API in the types-only crate `marketplace-api`.
3. Isolate generated Gonka protobuf in `gonka-proto`; keep reusable
   stateless calculations and validated native queries in `marketplace-common`.
   State-machine-specific validations remain in Factory and Deal.
4. Locate system `cw-multi-test` tests alongside Factory, as it governs Deal lifecycles.
5. Generate JSON Schemas independently for each deployable contract.
6. Do not turn `marketplace-common` into a third contract or shared state layer:
   the crate contains no storage, authorization, transitions, transfers, or
   transaction construction. Its code is statically linked into the Wasm of each contract.

## Decision Verification

- Unique Deal address requirement: MVP specification.
- Factory/per-instance pattern: DAO DAO `cw-payroll-factory` and `cw-vesting`.
- Types-only shared API: OroSwap and Astroport packages.
- `marketplace-common` introduced following demonstrated reuse of
   `funded_capacity_ngonka` and `query_current_epoch` in A1.
- Audit classes addressed by tests: stale pending state, improper amount validation, unbounded iteration, rounding errors, and privilege risks.

## Consequences

The workspace contains more crate boundaries, but dependencies flow unidirectionally and explicitly. The trade-off is requiring synchronized schema generation and cross-contract API compatibility testing.
