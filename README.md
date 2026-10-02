# Gonka Forward Marketplace

CosmWasm contracts for a forward marketplace on Gonka. The Factory creates an
isolated immutable Deal for each Host/epoch; Buyer deposits a fixed CW20 budget,
native claim confirms GNK ownership, and the Deal accrues independent USDT debts
and distributes spendable GNK according to permanent shares.

## Documentation

Start with [docs/README.md](docs/README.md). The core references are:

- [Architecture](docs/architecture.md): components, immutable registry and design decisions.
- [Contract behavior](docs/contract-behavior.md): lifecycle, formulas, refunds and payouts.
- [Client guide](docs/client-guide.md): JSON messages, queries and keeper operation.
- [Chain requirements](docs/chain-requirements.md): native trust and query boundaries.
- [Release guide](docs/release-guide.md) and [validation](docs/validation.md): builds,
  deployment, required acceptance and evidence.

Security invariants and reporting are in [SECURITY.md](SECURITY.md); the pinned
stack is recorded in [VERSIONS.md](VERSIONS.md). Historical reviews and run logs
are accessible in preceding Git revisions, not current-release proof.

## Implemented behavior

Factory permanently indexes one no-admin Deal per Host/epoch. Deal accepts exact
six-decimal CW20 funding before the target epoch with exact native routing.
Permissionless Lock stores routing proof during E..E+4. Confirmed native claim
allows SettleClaim, which records debts without transfers; separate WithdrawUsdt
transactions pay Host, fee and Buyer independently, including after Completed.

GNK release distributes the entire spendable ngonka balance using frozen lifetime
shares, including late donations and unlocks. Refund supports validated routing
failure while Funded, exact unclaimed summaries from E+2 while Locked, and an
explicit NetworkUnconfirmed policy for permitted summary failures from E+3.
The Factory pair index is never released.

The independent live acceptance runner belongs to
[gonka24/forward-e2e](https://github.com/gonka24/forward-e2e). This repository keeps
production contracts/packages, local tests, schemas/protobuf, release tooling and
three test-only fixture contracts. A new release needs fresh evidence for its
exact contracts/Gonka/runner identities and Wasm binaries. Local tests do not
prove live-chain compatibility or production readiness.

## Local verification

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
python3 -B -m unittest discover -s scripts/tests -p 'test_*.py' -v
```

On Windows use `python` for the Python command. Generate protobuf/schemas with
the platform scripts described in [scripts/README.md](scripts/README.md).
CI also verifies reproducible generated files, the separate proto-generator,
dual optimizer outputs, Wasm validation and both dependency graphs. Ordinary
Wasm compilation does not replace release-build or native acceptance evidence.

## License

Original Gonka24 material is source-available under [BUSL-1.1](LICENSE), with
version 0.1.0 changing to Apache-2.0 on 2027-09-11 under the shipped parameters.
The Additional Use Grant covers commercial interaction/integration with official
Licensor deployments and their Factory-created Deals. Separate production
deployments require applicable license rights. See
[publication policy](docs/license-policy.md) and [third-party terms](THIRD_PARTY.md).
Licensor: Mikita Anikiyevich. Contact: support@gonka24.com.

## Authors

Developed by the Gonka24 team:

- Mikita Anikiyevich
- Nikolay Tverdokhlebov
- Hleb Dapkiunas
