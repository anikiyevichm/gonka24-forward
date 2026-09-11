# ADR-0002: Dedicated Toolchain for Security CLIs

- Status: Accepted
- Date: 2026-09-06

## Context

The contract compiler is pinned to Rust 1.81.0 to align with CosmWasm optimizer 0.16.1. Initially, security CLIs were also selected with an MSRV of 1.81.0.

In practice, `cargo-audit 0.21.2` and `cargo-deny 0.17.0` can no longer read the active RustSec database because new advisory records adopt CVSS 4.0. Freezing a legacy database snapshot would leave new vulnerabilities undetected.

## Decision

1. Contract Rust remains `1.81.0` unchanged.
2. `cargo-audit 0.22.2` and `cargo-deny 0.20.2` are compiled using a separate minimal Rust `1.88.0`.
3. The host security toolchain is never used for `cargo build`, `cargo test`, or Wasm release compilation.
4. `cargo-deny` rejects vulnerability/unsound advisories, but reports unmaintained transitive crates as warnings. For direct workspace dependencies, unmaintained advisories remain hard errors.

## Known Warnings

RustSec flags `derivative 2.2.0` and `paste 1.0.15` as unmaintained. They enter transitively via the pinned CosmWasm 2.2.2 graph; no safe drop-in patch exists within the selected stack. These are not vulnerability advisories, but warnings must be reviewed on each CosmWasm upgrade.

## Decision Verification

- `cargo audit` reads the active RustSec database and exits successfully with the two documented warnings.
- `cargo deny check` validates `advisories`, `bans`, `licenses`, and `sources` successfully.
- Wasm continues to compile strictly under `cargo 1.81.0` via `rust-toolchain.toml`.
