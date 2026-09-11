# ADR-0013: Emergency Refund for Unconfirmed Locked Deals

- Date: 2026-09-07.
- Status: Accepted and implemented contract-side; native/real-chain acceptance remains an open release gate.
- Contract base: `origin/main@5f4ed672b397f49cacd81c211e7e2417e1d0b987` after PR #14 merge.
- Gonka source: `042758f4aa911606d34fc90fc1f3f257c09d55b8`.
- Protobuf snapshot: `379bebced638aeb5e6077bfd51c986f898443832`.
- Runtime source: `wasmd v0.54.2@e9bff8b543a68b2e33b01cc957de07dc237be05e`, `wasmvm v2.2.4@45efbe55f3874020a1ba16effadf2f6737a69da1`.

## Decision

The existing permissionless `Refund {}` entrypoint receives a third, strictly Locked-state branch. Following a successfully read current epoch and `current >= checked(E+3)`, the contract can terminally close the deal with `RefundReason::NetworkUnconfirmed` if the exact immutable summary request for stored Host/E returned one of the explicitly permitted response or availability errors.

This is not proof of absent native claims, nor does it verify continuity of failure. The contract evaluates strictly the current query result after a fixed deadline. If a native claim actually took place but the summary is currently unreachable, the risk is deliberately distributed identically: the exact initial USDT deposit is refunded to the Buyer, Host and fee recipient receive 0 USDT, and all present and future GNK tokens belong to the Host via existing `HostOnly` policy. Gonka community compensation is not guaranteed by the protocol.

Without a Buyer, the result is `Expired` without transfers. With a Buyer, the result is `Refunded` with a single CW20 transfer of the configured deposit. No dedicated execute entrypoint or mutable admin parameter is added.

## Order of Gates

1. Pristine `Locked` deals with persisted `recipient_locked=true` and initial zero accounting are required.
2. `GetCurrentEpoch` must successfully return a valid response.
3. `E+2` and `E+3` are computed using named constants and `checked_add`; any overflow rejects Refund.
4. Prior to E+2, Refund is rejected without querying the summary.
5. Starting at E+2, an exact summary with `claimed=false` yields standard `ClaimExpiry`.
6. An exact summary with `claimed=true` prohibits Refund across all epochs and mandates `SettleClaim`.
7. Strictly starting at E+3, an explicitly permitted error on the current summary query yields `NetworkUnconfirmed`. Every retry executes the query anew; past errors are not stored as persistent entitlement to a refund.

`Funded` routing Refund and all other states retain their preexisting behavior. Terminal outcomes are never re-evaluated following network recovery.

## Exact Runtime Trace and Classification

The Deal constructs a protobuf request from immutable config and invokes `Querier::raw_query` through a narrow summary adapter. `cosmwasm-std 2.2.2` returns `SystemResult<ContractResult<Binary>>` without flattening.

The verified source path:

1. `wasmvm` Rust `query_raw` invokes FFI `query_external`.
2. `cQueryExternal` invokes `types.RustQuery`; malformed JSON requests here become typed `InvalidRequest`.
3. `wasmd QueryHandler.Query` instantiates a gas-limited cached sub-context and invokes `QueryPlugins.HandleQuery`.
4. `AcceptListGrpcQuerier` validates the compile-time path, resolves the router handler, executes it, and protobuf-decodes the response using the expected constructor.
5. Typed `UnsupportedRequest` is preserved as `SystemError`. Standard handler errors, NotFound, or codec errors after redaction become `ContractResult::Err`.
6. Malformed JSON envelopes from Go into Rust become typed `InvalidResponse`.

Error strings are not parsed and exert no influence over route, Host, epoch, or payload.

| Raw / Validation Summary Result | Emergency Trigger After E+3 |
|---|---:|
| `ContractResult::Err` from native handler, including redacted NotFound | Yes |
| Typed `UnsupportedRequest` | Yes |
| Typed `InvalidResponse` from Go response decode | Yes |
| Malformed or oversized protobuf, missing nested summary | Yes |
| Wrong Host/E, invalid participant identity | Yes |
| Request protobuf or JSON encoding failure | No |
| Typed `InvalidRequest` | No |
| `Unknown`, `NoSuchContract`, `NoSuchCode`, and future unexpected system errors | No |
| Current epoch error, state/config/accounting error, deadline overflow | No |
| CW20 or Bank transfer error | No; entire transaction rolls back |

Gas exhaustion, VM traps, and Go panics are not returned to the contract as standard raw query results. `wasmvm` `recoverPanic` maps gas panics to `GoError::OutOfGas` and other panics to `GoError::Panic`; Rust `GoError::into_result` returns backend errors that abort VM execution. `wasmd` additionally raises exhaustion of the outer gas meter as a panic. Consequently, a caller or calling contract/submessage cannot convert a healthy summary query into a successful emergency Refund simply by throttling gas.

## Financial and Security Invariants

- The caller cannot configure amount or recipient, nor can they select query path or payload.
- Refund strictly equals configured deposit, not live CW20 balance; donations and other assets remain in Deal escrow.
- State, reason, exact refund accounting, and `HostOnly` are persisted prior to dispatching the CW20 message.
- Message failures roll back state and transfers; a retry pays out exactly once.
- `host_net_usdt = fee_usdt = 0`; claim and entitlement accounting remain zero.
- No-sale deals generate no zero transfers.
- The Factory Host/E index is preserved; terminal states mutually exclude settlement and refund.
- Following `Refunded`/`Expired`, cumulative release allocates 100% of all GNK to the Host, including late inflows; the Buyer receives no GNK.

DAO DAO audit base `0b5cae57…` was referenced solely for exact funding and accounting-before-message/rollback patterns. OroSwap assessed scope `9042989…` and separate HAL-08 scope `59f095b…` served as prompts for permissionless timing, griefing vectors, and terminal exclusivity; no GPL code was copied. Astroport audited Maker `1f50cab…`/Vesting `042b076…` were referenced for zero-transfer suppression and event handling. No external audit validates Gonka semantics or this contract.

## Verification and Residual Risk

The unit test matrix covers E+1/E+2/E+3/late, overflow on both deadlines, claimed true zero/positive cases, all permitted and forbidden raw error categories, current-epoch query failures, summary recovery, and pristine accounting verification. `cw-multi-test` covers Buyer/no Buyer, exact refunds despite CW20 donations, other assets, Factory index preservation, CW20 rollback and retry, terminal exclusivity, the real-claim-but-unavailable-summary scenario, and late `HostOnly` GNK release.

Because the local development environment lacks a Go toolchain, a focused native Wasm test with real `wasmd/wasmvm`, constrained gas, and contract/submessage callers was not executed. The source trace detailed above is more authoritative than mock `QueryFailed` tests, but does not substitute for testing against the real Gonka binary. Mandatory prior to production: exact deployed binary and version verification, native gas regression benchmarks, golden E2E across all error categories, and balance/state receipts. Chain halts, unavailable `GetCurrentEpoch`, VM aborts, and failed CW20 transfers can still impede refunds. Refunds are not automated: an external keeper or caller must submit the transaction.
