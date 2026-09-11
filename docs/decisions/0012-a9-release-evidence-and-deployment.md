# ADR-0012: A9 Release Evidence and Fail-Closed Deployment

- Date: 2026-09-07.
- Status: Preparatory tooling implemented; production release gated.
- Base: `main@fd6c7f5603cd0827cb6c7284c99ed7fad504a241` following PR #12 merge.
- Pinned Gonka source: `379bebced638aeb5e6077bfd51c986f898443832`.

## Decision

Build manifest, deployment config, and receipt have distinct versioned schemas. Release builds are executed twice using the pinned optimizer image digest on `linux/amd64` from two clean `git archive` exports of the same committed `HEAD`. Wasm hashes must match identically and pass pinned `cosmwasm-check`.

Deployment utilizes documented `inferenced`/wasmd CLI argv arrays without shell execution. Read-only preflight is strictly decoupled from explicit broadcast. Following a `sync` broadcast, the tool queries the transaction by hash, accepts only confirmed `code=0`, writes receipts after every state mutation, and never automatically retries an ambiguous transaction. Factory instantiation passes exactly one explicit `--no-admin` flag and forbids `--admin`: absence of `--admin` by itself is insufficient for wasmd `v0.54.2`. Post-deployment queries must confirm `ContractInfo.admin=None`. An independent verifier validates chain ID, artifact/on-chain checksums, code IDs, admin status, Factory config, and CW20 decimals. Standalone `verify-deal` fully reuses this common verification flow before validating Deal state.

## Invariant

No unconfirmed assumption is accepted as release or deployment evidence:

- Pinned source SHA does not equate to proof of the deployed binary;
- A missing build check does not equal `pass`;
- A `sync` response does not constitute DeliverTx success;
- An event without on-chain query confirmation does not constitute verified deployment;
- A timeout never permits automatic re-broadcasting;
- An existing receipt blocks duplicate execution;
- Omission of `--admin` does not equal an explicit immutable configuration: `--no-admin` is mandatory, and specifying both flags simultaneously is forbidden;
- Absence of a `migrate` entrypoint does not substitute for `ContractInfo.admin=None`.

### Clarification on Release Evidence 1.0.0

**Decision:** Production configuration accepts strictly versioned release-evidence with hashed per-gate reports; nullable legacy evidence remains permissible only in non-production environments.

**Our Invariant:** `runtime_evidence_complete=true` strictly when B1, B3, B4, B5, and B6 possess individual `PASS` reports featuring attestation, assertions, and hashed primary artifacts, cryptographically bound to all semantic deployment parameters, runtime binary identity, and exact deployable Deal/Factory Wasm binaries. The `block_id.hash` at height 1 retrieved from the connected node must match the target network fingerprint. Pinned Gonka interface/protobuf SHA is not compared against runtime source SHA.

**Gonka / Source Constraint:** Current backlog designates B1/B3/B4/B5/B6 as production dependencies, with B2 optional. Local A8 testing may reuse the same chain ID; therefore, a chain ID alone without HTTPS endpoint, network fingerprint, and hashes of deployable artifacts is insufficient.

**Relevant Audit Finding:** A8 review R4 forbids treating checkout/source provenance as proof of the runtime binary. A structural report cannot substitute for real-chain/runtime attestation.

**Testing:** The A9 unit suite rejects empty/null/invalid types, absent/FAIL/NOT_RUN/PARTIAL gates, foreign network/runtime/Wasm artifacts, and missing or tampered files; it also proves that deploy generates no transactions on error and accepts strictly synthetic positive fixtures.

## Gonka / Source Constraint

Pinned Gonka documentation specifies the `inferenced` binary and `tx wasm store/instantiate` commands. The Factory API requires `fee_bps=150`, settlement CW20 decimals 6, and Deal code ID; the Factory instantiates Deals with `admin=None` and compile-time pinned Gonka SHA. Pinned runtime remains vulnerable to CWA-2025-007, and B1–B6 remain open.

## Reference and Audit Comparison

The DAO DAO / Oak audit base `0b5cae57fecbbadb1045f3dc2bb4ad4fe5a98ee8` covered factory/vesting scope and highlights the necessity of verifying actual deployed instances and atomic execution flows; owner/admin powers are not ported. OroSwap HAL-08 relates to a separate pool-initializer scope/base `59f095b…`, where remediation `2fa02f9e30c3dbc80f84f47382ef73017ff0e8b9` illustrates the risk of stale pending operations: our countermeasure is durable receipts persisted prior to any subsequent external action. Astroport / Oak findings regarding events/observability reinforce structured results plus read-only queries, but no external audit covers our deployment tooling or Gonka. No GPL code was copied.

## Rationale

`git archive` guarantees that the optimizer sees only the committed tree rather than arbitrary working directory artifacts. Two independent output directories detect non-determinism. The digest pins image contents, and the platform flag enforces the target build architecture. Receipts persist side effects into explicit state, making restarts fail closed. The independent verifier can be executed later by an independent operator.

## Negative Test Coverage

The Python unit suite verifies tampered hashes, divergent build outputs, missing fields/evidence, mismatched chain IDs, failed confirmed transactions, ambiguous timeouts without resend, checksum/code ID/admin/config/decimals discrepancies, receipt replay guards, read-only dry-runs, and existing Deal verification. CLI regression tests verify mandatory `--no-admin`, fake rejection of omitted/dual admin flags, and complete successful deployments. The `verify-deal` entrypoint tests success, wrong node chain, tampered Wasm, stale manifest/config hashes, foreign receipt bindings, Factory checksum/code/admin/config, CW20 decimals, and Deal terms/index; every failure mode remains read-only and never outputs a success result. Fixtures do not substitute for fork E2E tests.

## Residual Risk

Locally, without an active Docker engine, current optimizer hashes cannot be reproduced. Without a fork, CLI/event JSON, runtime binary identity, allowlist enforcement, claim/streamvesting mechanics, and transaction lifecycles on Gonka cannot be confirmed. These gaps remain explicitly open and do not block the merge of the self-contained preparatory scope A9.
