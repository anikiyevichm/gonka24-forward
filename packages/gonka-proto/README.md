# Gonka Protobuf Bindings

This crate contains protobuf wire types only. It has no awareness of whether a
received response is trusted: semantic validation is performed by `marketplace-deal::gonka`.

## Pinned Source

- Repository: `https://github.com/gonka-ai/gonka`
- Commit: `379bebced638aeb5e6077bfd51c986f898443832`
- Runtime codec: `prost 0.12.6`
- Generator: `prost-build 0.12.6`
- Vendored protoc provider: `protoc-bin-vendored 3.2.0`

Upstream `query.proto` imports a significant portion of the inference schema, so the
generated source contains the complete requisite closure. The public API of this crate
deliberately re-exports only Marketplace types from `src/lib.rs`.

## Regeneration

From repository root:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/generate-proto.ps1
```

The command verifies `proto/checksums.sha256`, uses committed snapshots, and does not
require system `protoc`, `buf`, or network downloads of upstream `.proto` files. On a clean
machine, Cargo fetches locked host-generator dependencies once; subsequent runs use the
local Cargo cache. Provenance details are recorded in `proto/PROVENANCE.toml`, and the exact
Buf dependency graph is preserved in `proto/upstream.buf.lock`.

Rules:

1. Never define protobuf fields manually.
2. Do not place semantic validation or contract storage here.
3. Commit required `.proto` snapshots and generated Rust so normal builds require neither `protoc` nor network access.
4. Do not edit `src/generated/*.rs`; changes are made exclusively via upstream snapshots and the generator.
