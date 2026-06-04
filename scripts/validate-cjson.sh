#!/usr/bin/env bash
# validate-cjson.sh
#
# Validate c2rust-demo against DaveGamble/cJSON by running `init` and `merge`,
# then asserting that all expected outputs are present and correct.
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
FEATURE_ROOT="$CJSON_DIR/.c2rust/default"
PASS=0
FAIL=0

echo "=== validate-cjson.sh ==="
echo "c2rust-demo repo : $REPO_ROOT"
echo "cJSON clone dir  : $CJSON_DIR"
echo ""

# -----------------------------------------------------------------------
# Helper: assert that a path exists (file or directory or symlink)
# -----------------------------------------------------------------------
assert_exists() {
    local path="$1"
    local label="${2:-$1}"
    if [ -e "$path" ] || [ -L "$path" ]; then
        echo "  [PASS] $label"
        PASS=$((PASS + 1))
    else
        echo "  [FAIL] $label  -- NOT FOUND: $path"
        FAIL=$((FAIL + 1))
    fi
}

# Assert that a path is a symbolic link
assert_symlink() {
    local path="$1"
    local label="${2:-$1}"
    if [ -L "$path" ]; then
        echo "  [PASS] $label (symlink)"
        PASS=$((PASS + 1))
    else
        echo "  [FAIL] $label  -- expected a symlink at: $path"
        FAIL=$((FAIL + 1))
    fi
}

# Assert that a directory contains at least one file matching a glob pattern
assert_nonempty_glob() {
    local dir="$1"
    local pattern="$2"
    local label="${3:-$dir/$pattern}"
    if compgen -G "$dir/$pattern" > /dev/null 2>&1; then
        local count
        count=$(find "$dir" -maxdepth 1 -name "$pattern" | wc -l)
        echo "  [PASS] $label ($count match(es))"
        PASS=$((PASS + 1))
    else
        echo "  [FAIL] $label  -- no files matching '$pattern' found in $dir"
        FAIL=$((FAIL + 1))
    fi
}

# Assert that a file contains a given substring
assert_contains() {
    local file="$1"
    local substring="$2"
    local label="${3:-$file contains '$substring'}"
    if [ -f "$file" ] && grep -qF "$substring" "$file"; then
        echo "  [PASS] $label"
        PASS=$((PASS + 1))
    else
        echo "  [FAIL] $label  -- '$substring' not found in $file"
        FAIL=$((FAIL + 1))
    fi
}

# -----------------------------------------------------------------------
# Step 1: build c2rust-demo
# -----------------------------------------------------------------------
echo "--- Step 1: Building c2rust-demo ---"
cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml"
echo ""

# -----------------------------------------------------------------------
# Step 2: clone cJSON (or reuse an existing clone)
# -----------------------------------------------------------------------
echo "--- Step 2: Cloning DaveGamble/cJSON ---"
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
echo "--- Step 3: Running c2rust-demo init ---"
cd "$CJSON_DIR"
"$BINARY" init -- gcc -c cJSON.c -I.
echo ""

# -----------------------------------------------------------------------
# Step 4: validate init output
# -----------------------------------------------------------------------
echo "--- Step 4: Validating init output ---"
assert_exists "$FEATURE_ROOT"                                    "feature root .c2rust/default/"
assert_exists "$FEATURE_ROOT/meta"                               "meta/ directory"
assert_exists "$FEATURE_ROOT/meta/build_cmd.txt"                 "meta/build_cmd.txt"
assert_contains "$FEATURE_ROOT/meta/build_cmd.txt" "gcc"         "build_cmd.txt contains 'gcc'"
assert_exists "$FEATURE_ROOT/meta/selected_files.json"           "meta/selected_files.json"
assert_exists "$FEATURE_ROOT/c"                                  "c/ directory (captured .c2rust files)"
assert_nonempty_glob "$FEATURE_ROOT/c" "*.c2rust"               "at least one .c2rust capture file"
assert_exists "$FEATURE_ROOT/rust"                               "rust/ directory"
assert_exists "$FEATURE_ROOT/rust/Cargo.toml"                    "rust/Cargo.toml"
assert_exists "$FEATURE_ROOT/rust/src"                           "rust/src/"
assert_exists "$FEATURE_ROOT/rust/src/lib.rs"                    "rust/src/lib.rs"
assert_exists "$FEATURE_ROOT/rust/src/lib.normalized"            "rust/src/lib.normalized"
assert_exists "$FEATURE_ROOT/meta/init-interface-report.md"      "meta/init-interface-report.md"
# Check that at least one mod_* directory exists under rust/src/
if find "$FEATURE_ROOT/rust/src" -maxdepth 1 -type d -name "mod_*" | grep -q .; then
    echo "  [PASS] at least one mod_* directory under rust/src/"
    PASS=$((PASS + 1))
else
    echo "  [FAIL] no mod_* directories found under rust/src/"
    FAIL=$((FAIL + 1))
fi
echo ""

# -----------------------------------------------------------------------
# Step 5: run c2rust-demo merge
# -----------------------------------------------------------------------
echo "--- Step 5: Running c2rust-demo merge ---"
"$BINARY" merge
echo ""

# -----------------------------------------------------------------------
# Step 6: validate merge output
# -----------------------------------------------------------------------
echo "--- Step 6: Validating merge output ---"
assert_exists   "$FEATURE_ROOT/rust/src.1"                        "rust/src.1/ (init backup)"
assert_exists   "$FEATURE_ROOT/rust/src.2"                        "rust/src.2/ (merged output)"
assert_symlink  "$FEATURE_ROOT/rust/src"                          "rust/src -> src.2 symlink"
assert_exists   "$FEATURE_ROOT/meta/merge-interface-report.md"    "meta/merge-interface-report.md"
# src.2 must contain at least one .rs file
if find "$FEATURE_ROOT/rust/src.2" -name "*.rs" | grep -q .; then
    rs_count=$(find "$FEATURE_ROOT/rust/src.2" -name "*.rs" | wc -l)
    echo "  [PASS] src.2/ contains $rs_count .rs file(s)"
    PASS=$((PASS + 1))
else
    echo "  [FAIL] src.2/ contains no .rs files"
    FAIL=$((FAIL + 1))
fi
# lib.rs must exist in src.2
assert_exists "$FEATURE_ROOT/rust/src.2/lib.rs"                   "rust/src.2/lib.rs"
echo ""

# -----------------------------------------------------------------------
# Step 7: print the generated output tree for inspection
# -----------------------------------------------------------------------
echo "--- Generated .c2rust output tree ---"
find .c2rust -type f | sort
echo ""

# -----------------------------------------------------------------------
# Step 8: cargo check the generated Rust project
# RUSTC_BOOTSTRAP=1 is required because the generated lib.rs uses
# #![feature(linkage)] (documented in the generated file's comment).
# -----------------------------------------------------------------------
echo "--- Step 8: Running cargo check on generated Rust project ---"
RUSTC_BOOTSTRAP=1 cargo check --manifest-path "$FEATURE_ROOT/rust/Cargo.toml" 2>&1
if [ $? -eq 0 ]; then
    echo "  [PASS] cargo check passed"
    PASS=$((PASS + 1))
else
    echo "  [FAIL] cargo check failed on generated Rust project"
    FAIL=$((FAIL + 1))
fi
echo ""

# -----------------------------------------------------------------------
# Summary
# -----------------------------------------------------------------------
echo "=== Validation summary: $PASS passed, $FAIL failed ==="
if [ "$FAIL" -gt 0 ]; then
    echo "VALIDATION FAILED"
    exit 1
fi
echo "=== Validation complete ==="
