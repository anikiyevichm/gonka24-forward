# Architecture and design decisions

## Components and ownership

| Component | Responsibility |
| --- | --- |
| `marketplace-factory` | Create one Deal per Host/epoch; permanently index its address |
| `marketplace-deal` | Isolated USDT escrow, lifecycle, settlement debts and lifetime GNK distribution |
| `marketplace-api` | Serializable public messages and responses; no storage or business logic |
| `marketplace-common` | Pure financial calculations and validated current-epoch query; no storage, authorization, transitions or messages |
| `gonka-proto` | Generated bindings from a pinned, checksummed protobuf snapshot |
| `scripts/a9_release.py` | Product build, deployment and independent verification helper |
| `tests/contracts/` | Three test-only Wasm fixtures built by the acceptance runner from the selected contracts SHA |

`marketplace-common` is statically linked into the contracts, not deployed as a
third contract. Factory integration tests use real CW20 and bank ledgers in
`cw-multi-test`; custom Gonka queries are modeled. Native acceptance, Docker
networks, Go/Wasm probes and external Kotlin/Python harnesses live in the separate
[forward-e2e repository](https://github.com/gonka24/forward-e2e).

## Why each deal has its own address

A Deal fixes one Host, one target epoch, one settlement CW20, one fee recipient,
one price and one budget. Buyer identity is fixed by successful funding.
Gonka vesting is expected to aggregate by recipient address, without a Deal ID.
Separate addresses prevent schedules, deposits and release counters from being
shared between deals. A global escrow is outside this design.

Both earned work and rewarded GNK form one claim total. The Deal splits ownership
after native claim confirmation; it does not ask the native chain to distribute
the components separately between Buyer and Host. All spendable `ngonka` arriving
at the Deal follows its frozen policy, including donations and another Host's
misrouted claim. USDT donations do not create additional settlement rights.

## Factory creation and permanent registry

Factory configuration fixes a positive Deal code ID, validated CW20 and fee
addresses, and fee `150` bps. Token `TokenInfo.decimals` must be `6`. The Host is
the `CreateOffer` sender; callers cannot supply a different Host.

An offer requires positive price and budget, non-zero calculated capacity and
`current_epoch < target_epoch <= checked(current_epoch + 40)`. Each `(Host, E)`
can be registered once, even if its Deal is later cancelled, refunded or
completed. IDs start at `1` and increment with checked arithmetic.

The Factory stores a single pending context before dispatching an empty-funds
`WasmMsg::Instantiate` with `admin: None`. A second pending offer is rejected.
Reply accepts only ID `1`, exactly one typed
`/cosmwasm.wasm.v1.MsgInstantiateContractResponse`, a decoded valid address and
matching unused registry IDs. Success registers both indices, increments the ID
and removes pending context. Child instantiation or reply failure rolls back the
child, pending context and registry within the transaction.

## Immutable deployment

Neither production contract exports a migration entry point or administrative
withdrawal/configuration operation. Nevertheless, chain-level `ContractInfo.admin`
must be absent for both Factory and Deals: an admin could replace code with a
different implementation. Deploy Factory with explicit `--no-admin`, then query
the chain to verify it. Factory-created Deals use `admin: None`.

Deal instantiation additionally authenticates the supplied Factory against the
immediate sender, checks the embedded protobuf source SHA, verifies the token,
terms and epoch bounds, and starts in `Open`. Instantiate and every execute call
reject attached native funds. Normal bank sends can still deposit assets without
calling execute.

New code applies to new deployments. Existing immutable instances do not acquire
new behavior automatically. Settlement-token correctness and the actual chain
runtime remain trust dependencies; six decimals alone do not prove a compliant
or unrestricted token.

## Economic decisions

Funding accepts exactly the immutable budget through authenticated CW20 `Send`.
This prevents another deposit or live-balance contamination from changing Buyer
identity, capacity or obligations.

`SettleClaim` accrues USDT debts without transfers. Permissionless withdrawals
pay one immutable role at a time, so a blocked recipient does not roll back
settlement or a separately confirmed payment to another role. Roles stay
separate even when addresses coincide. Token-wide escrow restrictions can still
block all delivery. Refund branches retain a single atomic Buyer transfer.

Positive settlement freezes permanent GNK shares `B/T`. Lifetime cumulative
rounding distributes all available bank balance and supports late unlocks and
donations beyond the initial claim. Remaining vesting is diagnostic, not a
distribution cap. `Completed` marks the initial GNK threshold; it neither closes
future GNK distributions nor cancels unpaid USDT debts.

The E+3 `NetworkUnconfirmed` fallback is deliberate risk allocation: a permitted
summary failure may refund the Buyer even when a claim actually occurred. The
Host then owns all GNK. It is not proof of claim absence or a promise of external
compensation. Routing and all other queries retain fail-closed boundaries.

Detailed formulas and guards are in [contract behavior](contract-behavior.md).
The explicit summary classification is in [chain requirements](chain-requirements.md).

## Boundaries of the MVP

One Deal supports one Buyer and one Host/epoch. Partial funding, multiple Buyers,
cross-epoch netting, order books, bridges, arbitrary asset recovery and mutable
administration are not implemented. Two-party cumulative rounding must not be
generalized to N recipients by simply assigning the remainder to the last:
such targets can decrease as cumulative volume grows.

Third-party audits previously informed risk classes such as exact funding,
pending-state safety, bounded iteration, zero transfers and observability.
They do not audit Marketplace code, its formulas or Gonka runtime. Applicable
upstream notices remain governed by [THIRD_PARTY.md](../THIRD_PARTY.md).

Implementation anchors: [Factory](../contracts/marketplace-factory/src/contract.rs),
[reply](../contracts/marketplace-factory/src/reply.rs),
[Deal](../contracts/marketplace-deal/src/contract.rs),
[math](../packages/marketplace-common/src/math.rs).
