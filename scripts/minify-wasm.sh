#!/usr/bin/env bash
# Minify the ranked_voting template WASM for all four feature combinations and print a
# size table. Run from the repository root:
#
#   ./scripts/minify-wasm.sh
#
# Each "before" build goes to target/wasm32-unknown-unknown/release/ranked_voting.wasm,
# matching the README build table. The minified artifact is
# target/wasm32-unknown-unknown/release/ranked_voting.<feature>.min.wasm — publish that
# (the README "Publishing" section covers the workflow).
#
# This script exits non-zero if wasm-opt is missing (install it with
# `sudo apt-get install binaryen` or `cargo install wasm-opt`) or if the minified default
# build exceeds the size budget below — treat it as a failure in any automation.
#
# Note on wasm-opt feature flags: the rustc wasm32-unknown-unknown target emits bulk
# memory instructions (memory.copy / memory.fill), which wasm-opt rejects unless
# `--enable-bulk-memory` is passed. If a future toolchain emits other features,
# add the relevant `--enable-<feature>` flags here so minification never fails
# (or silently skips) a module.

set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v wasm-opt >/dev/null 2>&1; then
    echo "error: wasm-opt not found — install it (apt: sudo apt-get install binaryen; or: cargo install wasm-opt)" >&2
    exit 1
fi

# Size budget (bytes) for the minified default build. Without optimization the default
# build is ~367 KB; wasm-opt -Oz brings it to ~250 KB (the size sdbondi's review
# expected), and this gate leaves headroom for future growth.
SIZE_BUDGET=320000

declare -a COMBOS
COMBOS=(
    "default:"
    "irv-only:--no-default-features"
    "stv:--no-default-features --features stv"
    "seq-irv:--no-default-features --features sequential-irv"
)

build() {
    local label="$1" args="$2"
    echo "== building $label ($args) =="
    # shellcheck disable=SC2086
    cargo build --target wasm32-unknown-unknown --release -p ranked_voting $args
    # Each combo must land in a distinct file — the next `cargo build` (with different
    # features) overwrites target/wasm32-unknown-unknown/release/ranked_voting.wasm.
    cp target/wasm32-unknown-unknown/release/ranked_voting.wasm \
        "target/wasm32-unknown-unknown/release/ranked_voting.${label}.raw.wasm"
}

printf '%-8s %12s %12s %14s\n' "build" "raw bytes" "min bytes" "saved"
printf '%-8s %12s %12s %14s\n' "-----" "---------" "---------" "-----"

default_min=0
for combo in "${COMBOS[@]}"; do
    IFS=':' read -r label args <<<"$combo"
    build "$label" "$args"
    local_wasm="target/wasm32-unknown-unknown/release/ranked_voting.${label}.raw.wasm"
    min_wasm="target/wasm32-unknown-unknown/release/ranked_voting.${label}.min.wasm"
    raw_size=$(wc -c < "$local_wasm")
    wasm-opt -Oz --enable-bulk-memory "$local_wasm" -o "$min_wasm"
    min_size=$(wc -c < "$min_wasm")
    saved=$((raw_size - min_size))
    printf '%-8s %12d %12d %14d\n' "$label" "$raw_size" "$min_size" "$saved"
    if [[ "$label" == "default" ]]; then
        default_min=$min_size
    fi
done

echo
if [[ $default_min -gt $SIZE_BUDGET ]]; then
    echo "error: minified default build is ${default_min} bytes, over the ${SIZE_BUDGET}-byte budget" >&2
    exit 1
fi
echo "minified default build: ${default_min} bytes (budget ${SIZE_BUDGET}) — OK"