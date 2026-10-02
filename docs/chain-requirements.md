# Gonka integration and native trust requirements

## Interface source versus running runtime

The committed [protobuf provenance](../packages/gonka-proto/proto/PROVENANCE.toml)
pins interface source `379bebced638aeb5e6077bfd51c986f898443832`, 43 vendored
protobuf inputs and their checksums. Generated types, rather than handwritten
wire layouts, define requests and responses. Regeneration is offline with
respect to upstream sources; a cold Cargo cache can require dependency downloads.

This interface pin is not the runtime source SHA or proof of a deployed binary.
A selected Gonka revision may include the needed allowlist or runtime patches
without changing protobuf. Release evidence must identify and explain the exact
running runtime separately. The native source paths below preserve the review
map from the original design; recheck their semantics on the selected revision.
This repository alone cannot attest to native source or a running network.

## Narrow query boundary

| gRPC path | Immutable request input | Contract validation/use |
| --- | --- | --- |
| `/inference.inference.Query/GetCurrentEpoch` | Empty request | Epoch for creation, funding and timed transitions |
| `/inference.inference.Query/ListClaimRecipients` | participant = Host | At most one target-E row; valid recipient; positive exact routing or validated missing/mismatch |
| `/inference.inference.Query/EpochPerformanceSummaryByParticipant` | epoch_index = E, participant_id = Host | Required nested summary; exact Host/E; valid address; claimed and checked earned/rewarded total |
| `/inference.streamvesting.Query/TotalVestingAmount` | participant_address = Deal | NativeStatus diagnostic; only ngonka, at most one denom entry, valid Uint128 amount; empty list means zero |

No route, Host or epoch comes from execute-call evidence. All gRPC responses are
limited to 32 KiB before protobuf decoding. Recipient lists are limited to 64
decoded entries; duplicate target rows reject the operation. Entries for other
epochs do not prove the target route. Bank release uses ordinary
`BankQuery::Balance` for ngonka; it needs no custom gRPC allowlist entry and does
not depend on successful vesting diagnostics.

The selected chain must expose these four exact routes to Wasm with correct
protobuf response constructors. Avoid broad summary-all, Params or full vesting
schedule routes as a substitute. Preserve existing wrapped-token routes. A
read-only allowlist change does not by itself establish gas safety, summary
retention, claim atomicity or contract-address spendability.

Contract-side size checks happen after native response construction and do not
bound keeper memory/gas. ListClaimRecipients is unpaginated in the pinned
interface. Historical/pruning backlog can exceed the future lookahead window;
native construction and gas/size limits must be demonstrated independently.

Implementation: [shared epoch query](../packages/marketplace-common/src/gonka.rs),
[Deal adapters](../contracts/marketplace-deal/src/gonka.rs).

## Summary error policy

Only the summary adapter preserves `SystemResult<ContractResult<Binary>>` through
`raw_query`. It never parses node error text. Its emergency classification is used
only for pristine Locked Refund with a successfully queried epoch at E+3 or later.
Every error still blocks SettleClaim; prior to E+3 it also blocks Locked Refund.

| Current summary result | NetworkUnconfirmed at E+3+ |
| --- | --- |
| Native `ContractResult::Err`, including an untyped/redacted NotFound | Allowed |
| Typed `UnsupportedRequest` | Allowed |
| Typed `InvalidResponse` | Allowed |
| Oversized/malformed protobuf, absent nested summary | Allowed |
| Wrong Host/E or invalid participant address | Allowed |
| Request protobuf/JSON encoding failure | Rejected |
| `InvalidRequest`, `Unknown`, `NoSuchContract`, `NoSuchCode`, other unexpected system errors | Rejected |
| Arithmetic overflow, bad state/configuration, epoch query failure/deadline overflow | Rejected |
| CW20/Bank transfer failure | Transaction rolls back |

An exact healthy summary with `claimed=false` gives ClaimExpiry from E+2; with
`claimed=true` it always blocks Refund. Ordinary native handler errors do not
carry a reliable typed absence distinction in the reviewed runtime model.
An authoritative typed-absence/retention API could improve diagnostics, but is not
a prerequisite for the agreed emergency policy.

VM traps, gas exhaustion and Go panic paths require native tests: the contract
cannot establish that a backend abort is delivered as a regular query result.
Healthy queries under constrained gas, both direct and via a contract/submessage,
must never turn into successful emergency refunds. Synthetic ContractResult
errors test contract reaction only, not actual VM/FFI behavior.

## Native semantics to demonstrate

The Marketplace depends on these properties of the selected chain:

1. Native recipient assignment authenticates Host/participant authority, accepts
   a valid Deal address, supports future E up to lookahead 40, and rejects editing
   E once it starts. Batch mutations are atomic.
2. Exact `(Host, E)` routing remains usable for E..E+4; it may be pruned from E+5.
   Native pruning lag does not extend the contract's safe proof window.
3. The native claim window closes before the contract's E+2 claim-expiry refund.
   The pinned design expects claim for E at current E+1 only. Native automatic
   claim and restart behavior must also be reconciled with this assumption.
4. Both work and reward components go to the same resolved recipient. Summary
   `earned_coins`/`rewarded_coins` are the authoritative corresponding amounts,
   encoded as u64; `claimed=true` reflects successful native payout atomically.
   Partial payment, claim-marker failure and retry must not mint duplicate rights.
5. Exact summaries are retained and truthfully report claim status. Missing rows
   are not evidence of a zero claim. The contract's defensive claimed-zero path
   needs a separate native reachability assessment; unclaimed zero is an expiry
   scenario, not a mocked claim success.
6. Streamvesting keeps locked funds on its module account and transfers eligible
   tranches into the Deal's spendable ngonka balance. Unlock is a bank transfer,
   not an implicit Wasm execute. Existing tranches are not postponed by additions.
7. Schedules aggregate by recipient address; different Deal addresses remain
   isolated. Work/reward vesting periods may differ or be zero and must be read
   from the actual runtime, not hardcoded from historical genesis values.

Useful source paths, relative to Gonka `inference-chain/`:

| Concern | Review target |
| --- | --- |
| Allowlist and runtime | `app/legacy.go`, `app/app.go`, `go.mod` |
| Recipient authorization and immutability | `x/inference/keeper/msg_server_set_claim_recipients.go`, `claim_recipient_schedule.go` |
| Routing query and pruning | `x/inference/keeper/query_list_claim_recipients.go`, `pruning.go` |
| Claim window, recipient and finalization | `x/inference/keeper/msg_server_claim_rewards.go` (`validateRequest`, `payoutClaim`, `finishSettle`) |
| Summary generation and native payments | `x/inference/keeper/accountsettle.go`, `payment_handler.go` |
| Vesting storage/unlocks/diagnostics | `x/streamvesting/keeper/keeper.go`, `query.go` |
| Denom and scale | `x/inference/types/coin.go`, `denom.json` |

Runtime safety is a release gate. The previously reviewed wasmd v0.54.2 is in
the affected range of [CWA-2025-007](https://github.com/CosmWasm/advisories/blob/main/CWAs/CWA-2025-007.md),
whose patched 0.54 branch version is v0.54.3 and requires a coordinated consensus
upgrade. Check all currently applicable advisories for the actual binary; a
contract Rust dependency audit cannot clear Go/runtime vulnerabilities. Keep
source/build metadata and validator/operator attestation, not just version text.

Full acceptance obligations and proof levels are in [validation](validation.md).
