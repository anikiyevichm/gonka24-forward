# Proto Snapshots

`vendor/` contains the recursive import closure for the four Marketplace protobuf
entrypoints from Gonka commit `379bebced638aeb5e6077bfd51c986f898443832`.

- `PROVENANCE.toml` records the source, entrypoints, and generator versions.
- `checksums.sha256` protects every `.proto` from undetected modification.
- `upstream.buf.lock` records exact BSR dependency commits used during
  the original closure export.

The snapshot is preserved byte-for-byte, including the original mix of LF and CRLF. Therefore,
`.gitattributes` marks strictly `proto/vendor/**` as `-text`: Git must not alter line endings
for these files upon checkout. The rest of the project text is normalized to LF. Modifying or
auto-formatting vendored `.proto` files is prohibited — doing so deliberately triggers a checksum error.

Why there are more than four files: `inference/inference/query.proto` defines many
query messages in a single file and imports types from the entire inference module. We
preserve the exact upstream source rather than manually copy-pasting individual field
tags. Externally, `gonka-proto` still exports only the required Marketplace types.
