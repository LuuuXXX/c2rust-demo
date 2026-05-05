#!/usr/bin/env bash
# =============================================================================
# validate-cjson.sh — Manual validation of c2rust-demo against DaveGamble/cJSON
# =============================================================================
#
# PURPOSE
#   This script is a **manual** validation helper only.  It is NOT run as part
#   of the normal CI test suite.  Its sole purpose is to let a developer
#   quickly verify that `c2rust-demo init` and `c2rust-demo merge` work against
#   a real-world C project (cJSON).
#
# PREREQUISITES
#   - Linux (LD_PRELOAD hook required)
#   - Rust / cargo  (>= 1.82)
#   - gcc
#   - make
#   - clang
#   - bindgen  (`cargo install bindgen-cli`)
#   - git
#
# USAGE
#   bash scripts/validate-cjson.sh [--keep-workdir]
#
#   --keep-workdir   Do not delete the temporary working directory on exit.
#                    Useful when you want to inspect the generated output.
#
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

KEEP_WORKDIR=false
for arg in "$@"; do
    case "$arg" in
        --keep-workdir) KEEP_WORKDIR=true ;;
        *) echo "Unknown argument: $arg" >&2; exit 1 ;;
    esac
done

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

info()    { echo -e "\033[1;34m[INFO]\033[0m  $*"; }
success() { echo -e "\033[1;32m[OK]\033[0m    $*"; }
warn()    { echo -e "\033[1;33m[WARN]\033[0m  $*"; }
die()     { echo -e "\033[1;31m[ERROR]\033[0m $*" >&2; exit 1; }

require_tool() {
    if ! command -v "$1" &>/dev/null; then
        die "Required tool not found: $1.  Please install it and retry."
    fi
}

# ---------------------------------------------------------------------------
# Check prerequisites
# ---------------------------------------------------------------------------

info "Checking prerequisites..."
require_tool git
require_tool cargo
require_tool gcc
require_tool make
require_tool clang
require_tool bindgen

success "All required tools are present."

# ---------------------------------------------------------------------------
# Build c2rust-demo
# ---------------------------------------------------------------------------

info "Building c2rust-demo (cargo build --release)..."
cargo build --release --manifest-path "${REPO_ROOT}/Cargo.toml"
C2RUST_DEMO_BIN="${REPO_ROOT}/target/release/c2rust-demo"
success "Binary built at ${C2RUST_DEMO_BIN}"

# ---------------------------------------------------------------------------
# Create working directory
# ---------------------------------------------------------------------------

WORKDIR="$(mktemp -d -t c2rust-demo-cjson-XXXXXX)"
info "Working directory: ${WORKDIR}"

cleanup() {
    if [ "${KEEP_WORKDIR}" = "false" ]; then
        info "Cleaning up ${WORKDIR}"
        rm -rf "${WORKDIR}"
    else
        info "Working directory kept: ${WORKDIR}"
    fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Clone cJSON
# ---------------------------------------------------------------------------

CJSON_DIR="${WORKDIR}/cJSON"
info "Cloning DaveGamble/cJSON into ${CJSON_DIR}..."
git clone --depth=1 https://github.com/DaveGamble/cJSON.git "${CJSON_DIR}"
success "cJSON cloned."

# ---------------------------------------------------------------------------
# Run c2rust-demo init
# ---------------------------------------------------------------------------

# We run init inside the cJSON directory so that c2rust-demo can discover the
# project root automatically and write output under cJSON/.c2rust/.
info "Running: c2rust-demo init -- make"
echo "------------------------------------------------------------"
(
    cd "${CJSON_DIR}"
    "${C2RUST_DEMO_BIN}" init -- make
)
echo "------------------------------------------------------------"
success "c2rust-demo init completed."

# ---------------------------------------------------------------------------
# Show init output structure
# ---------------------------------------------------------------------------

FEATURE_ROOT="${CJSON_DIR}/.c2rust/default"

info "Output structure after init:"
echo ""
if command -v tree &>/dev/null; then
    tree -a --dirsfirst -L 4 "${FEATURE_ROOT}" 2>/dev/null || true
else
    find "${FEATURE_ROOT}" -maxdepth 4 | sort
fi
echo ""

# Basic assertions
[ -d "${FEATURE_ROOT}/c"    ] && success "c/          directory exists" \
                               || warn    "c/          directory MISSING"
[ -d "${FEATURE_ROOT}/meta" ] && success "meta/       directory exists" \
                               || warn    "meta/       directory MISSING"
[ -f "${FEATURE_ROOT}/meta/build_cmd.txt"       ] && success "meta/build_cmd.txt exists" \
                                                   || warn    "meta/build_cmd.txt MISSING"
[ -f "${FEATURE_ROOT}/meta/selected_files.json" ] && success "meta/selected_files.json exists" \
                                                   || warn    "meta/selected_files.json MISSING"
[ -d "${FEATURE_ROOT}/rust" ] && success "rust/       directory exists" \
                               || warn    "rust/       directory MISSING (init may have stopped early)"
if [ -d "${FEATURE_ROOT}/rust" ]; then
    [ -f "${FEATURE_ROOT}/rust/Cargo.toml"     ] && success "rust/Cargo.toml exists" \
                                                  || warn    "rust/Cargo.toml MISSING"
    [ -f "${FEATURE_ROOT}/rust/src/lib.rs"     ] && success "rust/src/lib.rs exists" \
                                                  || warn    "rust/src/lib.rs MISSING"
    MOD_COUNT=$(find "${FEATURE_ROOT}/rust/src" -maxdepth 1 -type d -name 'mod_*' | wc -l)
    if [ "${MOD_COUNT}" -gt 0 ]; then
        success "Found ${MOD_COUNT} mod_* directories under rust/src/"
    else
        warn "No mod_* directories found under rust/src/"
    fi
fi

# Print selected_files.json for reference
if [ -f "${FEATURE_ROOT}/meta/selected_files.json" ]; then
    info "Contents of meta/selected_files.json:"
    cat "${FEATURE_ROOT}/meta/selected_files.json"
    echo ""
fi

# ---------------------------------------------------------------------------
# Run c2rust-demo merge
# ---------------------------------------------------------------------------

if [ -d "${FEATURE_ROOT}/rust" ]; then
    info "Running: c2rust-demo merge"
    echo "------------------------------------------------------------"
    (
        cd "${CJSON_DIR}"
        "${C2RUST_DEMO_BIN}" merge
    )
    echo "------------------------------------------------------------"
    success "c2rust-demo merge completed."

    SRC2="${FEATURE_ROOT}/rust/src.2"
    info "Output structure after merge (rust/src.2/):"
    echo ""
    if [ -d "${SRC2}" ]; then
        if command -v tree &>/dev/null; then
            tree --dirsfirst -L 3 "${SRC2}" 2>/dev/null || true
        else
            find "${SRC2}" -maxdepth 3 | sort
        fi
    else
        warn "rust/src.2/ not found – merge may not have produced output"
    fi
    echo ""
else
    warn "Skipping merge: rust/ directory was not created by init."
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo ""
echo "=============================="
echo "  Validation complete"
echo "=============================="
if [ "${KEEP_WORKDIR}" = "true" ]; then
    echo ""
    echo "Working directory: ${WORKDIR}"
    echo "  cJSON source  : ${CJSON_DIR}"
    echo "  Feature root  : ${FEATURE_ROOT}"
fi
