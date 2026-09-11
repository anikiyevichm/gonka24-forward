#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PROTO_ROOT="$REPO_ROOT/packages/gonka-proto/proto/vendor"
CHECKSUM_FILE="$REPO_ROOT/packages/gonka-proto/proto/checksums.sha256"
GENERATOR_MANIFEST="$REPO_ROOT/tools/proto-gen/Cargo.toml"

if [[ ! -f "$CHECKSUM_FILE" ]]; then
    echo "Error: Missing protobuf checksum manifest: $CHECKSUM_FILE" >&2
    exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo is required but not found in PATH" >&2
    exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
    SHA256_CHECK="sha256sum -c"
elif command -v shasum >/dev/null 2>&1; then
    SHA256_CHECK="shasum -a 256 -c"
else
    echo "Error: neither sha256sum nor shasum found" >&2
    exit 1
fi

echo "Verifying vendored protobuf checksums..."
(
    cd "$PROTO_ROOT"
    grep -v '^#' "$CHECKSUM_FILE" | grep -v '^[[:space:]]*$' | $SHA256_CHECK
)

EXPECTED_COUNT=$(grep -v '^#' "$CHECKSUM_FILE" | grep -v '^[[:space:]]*$' | wc -l | tr -d ' ')
ACTUAL_COUNT=$(find "$PROTO_ROOT" -name '*.proto' -type f | wc -l | tr -d ' ')

if [[ "$EXPECTED_COUNT" -ne "$ACTUAL_COUNT" ]]; then
    echo "Error: Vendored protobuf file count differs from checksum manifest: expected $EXPECTED_COUNT, got $ACTUAL_COUNT" >&2
    exit 1
fi

echo "Regenerating protobuf bindings..."
cargo +1.81.0 run --locked --quiet --manifest-path "$GENERATOR_MANIFEST"
cargo +1.81.0 fmt --manifest-path "$GENERATOR_MANIFEST" -- --check

echo "Gonka protobuf bindings are regenerated from the verified snapshot."
