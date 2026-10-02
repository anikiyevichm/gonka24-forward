# Working in Forward Contracts

This repository contains the Rust/CosmWasm contracts, their API/protobuf packages,
local unit/property/`cw-multi-test` tests, and release/deployment tooling.

The independent acceptance runner lives in
[gonka24/forward-e2e](https://github.com/gonka24/forward-e2e).
Read that repository's AGENTS.md before changing the runner. Do not restore runner
modules, Docker network templates or Python acceptance tests here.

## Contract changes

- Keep behavior changes and their Rust tests in the same PR.
- Preserve the fail-closed query boundary, accounting invariants and independent
  USDT withdrawals described in SECURITY.md and docs/decisions/.
- Keep committed schemas, protobuf snapshots and lockfiles reproducible.
- The three small crates under tests/contracts/ remain inputs to the E2E build:
  the runner builds them from the same selected contracts SHA. They are not
  production contracts.

## Validation

Run the checks appropriate to the change; CI defines the full release checks:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
python3 -B -m unittest discover -s scripts/tests -p 'test_*.py' -v
```

Release tooling tests use unittest and offline fixtures. Do not claim local
unit or cw-multi-test results as proof of compatibility with a live Gonka chain.
A release requires fresh E2E evidence for the exact full contracts SHA, Gonka SHA
and runner image. See docs/e2e-validation.md.

scripts/a9_release.py is the canonical product release/deployment helper. The
E2E runner contains a separately pinned copy hashed into its run lock; changes
here require an explicit reviewed import there, not runtime loading from a
contracts checkout.

## GitHub access

For GitHub Actions, PR or repository operations that fail in the sandbox because
gh cannot read the Windows keyring, retry the same narrow gh or git command with
scoped elevated permissions. Do not treat sandbox-only credential errors as a
repository-access failure before that retry.
