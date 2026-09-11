#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo is required but not found in PATH" >&2
    exit 1
fi

cd "$REPO_ROOT"

cargo run --locked --example factory-schema -p marketplace-factory
cargo run --locked --example deal-schema -p marketplace-deal

echo "Marketplace contract schemas regenerated."
