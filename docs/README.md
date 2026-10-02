# Forward Marketplace documentation

These documents describe the contracts and tooling in this repository. Contract
behavior is reconciled with the Rust implementation; JSON wire types come from
the committed schemas. Chain requirements describe what a selected Gonka runtime
must demonstrate, rather than certifying a running network.

| Document | Purpose |
| --- | --- |
| [Architecture](architecture.md) | Component boundaries, immutable deployment and design rationale |
| [Contract behavior](contract-behavior.md) | State transitions, economic formulas and accounting |
| [Client guide](client-guide.md) | Messages, queries, events and keeper operation |
| [Chain requirements](chain-requirements.md) | Native routing, claim, vesting and query trust boundaries |
| [Release guide](release-guide.md) | Reproducible builds, production gates and deployment commands |
| [Validation](validation.md) | Local checks, required live scenarios and evidence retention |
| [License policy](license-policy.md) | Publication dates and redistribution procedure |

[SECURITY.md](../SECURITY.md) defines the security policy and reporting channel.
[VERSIONS.md](../VERSIONS.md) records the pinned stack. The independent acceptance
runner belongs to [forward-e2e](https://github.com/gonka24/forward-e2e).

Historical ADRs, reviews and September 2026 run records are removed from the
current documentation tree. Their original bytes remain in preceding Git
revisions. Their test verdicts do not certify this revision. Current releases
require evidence for their exact sources, binaries and runner image as described
in [validation](validation.md).
