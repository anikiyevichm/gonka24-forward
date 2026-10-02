# Gonka Forward Marketplace

> New agreed GNK model: [permanent shares, formulas, and examples](docs/decisions/0010-permanent-gnk-shares.md).
> The contract implements this model: all available `ngonka` are perpetually split according to
> frozen exact shares, and the previous cap based on claim/vesting has been removed.


CosmWasm workspace for forward marketplace on the Gonka network.

The independent live acceptance runner is maintained in
[gonka24/forward-e2e](https://github.com/gonka24/forward-e2e).
This repository retains the contracts, local Rust tests and release tooling;
see the [E2E validation handoff](docs/e2e-validation.md).

## License

Original Gonka24 code is source-available under [BUSL-1.1](LICENSE), with each
version converting to Apache-2.0 one calendar year after its first public
distribution. New versions do not extend earlier versions' periods. The license
permits non-production use and includes additional permissions for commercial
use of official Gonka24 deployments and integrations. Other production
deployments require a separate license until the applicable Change Date.
See the [version policy](docs/licensing.md) and [third-party terms](THIRD_PARTY.md).
Licensor: Mikita Anikiyevich. Contact: support@gonka24.com.

## Authors

Developed by the Gonka24 team:

- Mikita Anikiyevich
- Nikolay Tverdokhlebov
- Hleb Dapkiunas

> [!NOTE]
> This repository is prepared for public release with a clean history. Development logs,
> historical pull requests, and raw testing environments are retained in the private archive.
> Verified architectural invariants and decisions (including [ADR-0010](docs/decisions/0010-permanent-gnk-shares.md)
> and [ADR-0014](docs/decisions/0014-independent-usdt-withdrawals.md)) are fully preserved.

## Current State

This is a compilable Marketplace workspace with immutable configuration and a safe
internal Gonka query boundary. The Factory creates and indexes an isolated
Deal for each Host/epoch pair. The Deal supports exact CW20 funding, Lock,
Cancel, and SettleClaim with independent USDT payouts, cumulative GNK release according
to permanent shares, and late distributions after Completed. `Refund {}` implements both
routing-failure `Funded -> Refunded` within the `E..E+4` window, and Locked completion:
regular claim expiry from E+2 based on an exact `claimed=false` summary, or emergency
`NetworkUnconfirmed` from E+3 upon explicitly classified summary unavailability.
With a Buyer the outcome is `Refunded`, without a Buyer — `Expired`.

The A8 contract-only check adds the complete Factory→Deal lifecycle, joint operation
of both release entry points after Completed, and concurrent deals for multiple
Host/epoch pairs with separate CW20/bank ledgers and lifetime counters.
Production code and economics were not altered. The complete [gap analysis and
evidence matrix](docs/reviews/a8-contract-gap-analysis.md) and compact
[integration guide](docs/integration-guide.md) separate what has been proven locally from
mandatory real-chain E2E testing.

- `marketplace-factory` — creates an isolated Deal without an admin and permanently indexes it by id and Host/epoch.
- `marketplace-deal` — stores the state of a single deal, accepts exact CW20 budget, implements fail-closed routing transitions, and records settlement upon confirmed native claim.
- `marketplace-api` — public messages and responses only; no storage or business logic.
- `gonka-proto` — protobuf types only, generated from pinned Gonka source.

## Local Verification

```bash
# Generate schemas and protobuf bindings (on Linux/macOS):
./scripts/generate-proto.sh
./scripts/generate-schema.sh

# Or on Windows PowerShell:
# powershell -ExecutionPolicy Bypass -File scripts/generate-proto.ps1
# powershell -ExecutionPolicy Bypass -File scripts/generate-schema.ps1

cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build -p marketplace-factory -p marketplace-deal --release --target wasm32-unknown-unknown --locked
cosmwasm-check target/wasm32-unknown-unknown/release/marketplace_factory.wasm target/wasm32-unknown-unknown/release/marketplace_deal.wasm
cargo audit
cargo deny check
```

End-to-end contract ledger scenarios alone can be quickly rerun separately:

```bash
cargo test -p marketplace-factory --test integration --locked
```

`rust-toolchain.toml` automatically selects Rust 1.81.0 and installs the target Wasm format. The full version matrix and rationale are documented in `VERSIONS.md`.

Security CLIs are built with a separate Rust 1.88.0 because they are not compiled into the contract and must be able to read the current RustSec database with CVSS 4.0. This does not change the compiler used for Wasm code.

## Critical Boundary

Successful unit and `cw-multi-test` tests do not prove compatibility with a real Gonka chain. Custom gRPC queries and `streamvesting` must be additionally verified with golden E2E tests on an actual node.

On the pinned Gonka source commit, four Marketplace routes are not yet added to
`AcceptedGrpcQueries()`. The verified Gonka PR head `042758f…` opens them without
protobuf changes, but the production binary SHA and real-chain invocations must still be
confirmed. Fixtures and mocks prove Rust reaction, not release readiness.

`Lock` and permissionless `Cancel` are restricted to the provable window
`E <= current < E+5`. Starting from `E+5`, the recipient row may have been deleted by pruning,
so an empty response no longer proves historical absence of routing.

Routing-failure `Refund` uses only a successful validated
`ListClaimRecipients`: missing or another valid recipient permits an exact
refund of the Buyer's original deposit. Correct routing, query/decode errors, E-1, and
E+5+ are rejected. The current CW20 balance does not determine the refund amount, so donations do
not increase the refund.

Claim expiry is permitted from `current >= E+2` based on a successfully obtained exact Host/E
summary with `claimed=false`. `claimed=true` always blocks Refund. Prior to E+3, any
summary error blocks the transition; from E+3 onwards, a normal native query error, expected
typed unavailable route, or invalid/unverifiable response yields a separate
`NetworkUnconfirmed`. Request/current-epoch/config/accounting/system errors
remain fail-closed; gRPC error text is not parsed.

Additionally, pinned Gonka `wasmd v0.54.2` is affected by CWA-2025-007; production
release requires confirmation of a patched, running binary. The presence of
contract-only A8 tests does not resolve this node-level blocker.

## Example: Routing-Failure Refund

After funding, if within `E <= current < E+5` a successful routing response proves
missing/mismatch, any caller can send:

```json
{"refund": {}}
```

The caller does not provide an amount or recipient. The Deal records `Refunded`, the exact
`refund_reason`, the original budget in accounting, and the `HostOnly` GNK policy, then
returns the deposit to the recorded Buyer. Any CW20 transfer failure rolls everything back.

## Example: Claim-Expiry Refund / Expired

After a successful Lock and the closure of the single native claim window, any caller
sends the same message:

```json
{"refund": {}}
```

The Deal requires `current >= E+2` and an exact `claimed=false` summary. A Funded Deal
returns the configured deposit to the Buyer and becomes `Refunded`; a no-sale Deal
becomes `Expired` without a transfer. Both outcomes record
`refund_reason = claim_expiry` and `HostOnly` for current and future GNK.

If an exact summary cannot be obtained or verified, the same message is permitted
only from `current >= E+3` and only for the ADR-0013 error matrix. In that case, the reason is
`network_unconfirmed`. This does not prove the absence of a claim: the Buyer receives USDT,
and the Host receives 100% of all GNK as they unlock. Refund requires an external
transaction and is not triggered automatically.

## Example: SettleClaim

After native `MsgClaimRewards` from the Host and a successful `Lock`, any caller can
send an empty permissionless message to the Deal:

```json
{"settle_claim": {}}
```

The caller supplies no claim amount, recipients, or proofs. The Deal reads the
exact immutable Host/E summary, requires `claimed=true`, and calculates entitlements.
`SettleClaim` records amounts without transfers: a positive claim enters `Releasing`,
and a zero claim enters `Completed` with the entire funded deposit owed to Buyer.
GNK release follows unlock rules even while USDT payments remain pending.

Any caller queries `{"usdt_payments":{}}` and sends each non-zero role in a
**separate transaction**:

```json
{"withdraw_usdt":{"role":"host"}}
{"withdraw_usdt":{"role":"fee"}}
{"withdraw_usdt":{"role":"buyer"}}
```

Amounts and recipients come from storage. A failed payment preserves its debt;
previously confirmed transactions remain committed. Withdrawals remain available
after `Completed`; already paid roles reject repeats. The query exposes immutable
`recipient`, `accrued_micro_usdt`, `paid_micro_usdt`, and `pending_micro_usdt`.
`claim_settled` records accrual; `usdt_paid`/`usdt_refunded` record successful delivery.
A token-wide escrow freeze can still block all USDT delivery.

## GNK Release by Permanent Shares

Once spendable GNK becomes available, any caller sends to the Deal:

```json
{"release_unlocked_gnk": {}}
```

The caller does not pass an amount or recipient. The Deal checks the frozen policy and lifetime
counters, reads only the current `ngonka` bank balance, and distributes the entire balance
according to the exact ratio `B/T` established at settlement. Donations, late unlocks,
and amounts exceeding the initial claim follow the same share; USDT is not charged again.
A repeated call with zero balance returns `NothingToRelease` and changes nothing.

When the lifetime total reaches `U >= T`, the state transitions once to `Completed`, but release
remains permitted. The legacy message is preserved only as a compatible alias in
`Completed` and uses the same shares and counters:

```json
{"forward_excess_gnk": {}}
```

A zero balance via the alias returns `NothingToForward` without a zero `BankMsg`.
For zero settlement, an explicit Host-only policy `0/1` applies; a positive
Buyer share is never bypassed. CW20 and other native denoms are not touched by this
operation. Examples are shown as JSON payloads
for execute calls; specific chain CLI, sender, and gas flags depend on the
deployment environment.
