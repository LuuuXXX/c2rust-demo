#!/usr/bin/env bash
# validate-cjson.sh
#
# Validate c2rust-demo against DaveGamble/cJSON by running `init` and `merge`.
# This script mirrors what the CI workflow (.github/workflows/validate-cjson.yml)
# does, so it can be run locally as well.
#
# Usage:
#   ./scripts/validate-cjson.sh
#
# Prerequisites: gcc, clang, bindgen (cargo install bindgen-cli), cargo
#
# The script exits non-zero if any step fails.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CJSON_DIR="${TMPDIR:-/tmp}/cjson-validate"
BINARY="$REPO_ROOT/target/release/c2rust-demo"

echo "=== validate-cjson.sh ==="
echo "c2rust-demo repo : $REPO_ROOT"
echo "cJSON clone dir  : $CJSON_DIR"
echo ""

# -----------------------------------------------------------------------
# Step 1: build c2rust-demo
# -----------------------------------------------------------------------
echo "--- Building c2rust-demo ---"
cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml"
echo ""

# -----------------------------------------------------------------------
# Step 2: clone cJSON (or reuse an existing clone)
# -----------------------------------------------------------------------
echo "--- Cloning DaveGamble/cJSON ---"
if [ -d "$CJSON_DIR/.git" ]; then
    echo "Reusing existing clone at $CJSON_DIR"
    git -C "$CJSON_DIR" pull --ff-only
else
    rm -rf "$CJSON_DIR"
    git clone https://github.com/DaveGamble/cJSON.git "$CJSON_DIR"
fi
echo ""

# -----------------------------------------------------------------------
# Step 3: run c2rust-demo init inside the cJSON repo
# -----------------------------------------------------------------------
echo "--- Running c2rust-demo init ---"
cd "$CJSON_DIR"
"$BINARY" init -- gcc -c cJSON.c -I.
echo ""

# -----------------------------------------------------------------------
# Step 4: run c2rust-demo merge
# -----------------------------------------------------------------------
echo "--- Running c2rust-demo merge ---"
"$BINARY" merge
echo ""

# -----------------------------------------------------------------------
# Step 5: print the generated output tree for inspection
# -----------------------------------------------------------------------
echo "--- Generated .c2rust output tree ---"
find .c2rust -type f | sort
echo ""

echo "=== Validation complete ==="
