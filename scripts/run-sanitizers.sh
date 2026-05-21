#!/usr/bin/env bash
# Regenerate sanitizer-result snapshots under docs/sanitizers/.
#
# Requires:
#   - rustup toolchain install nightly
#   - rustup component add rust-src --toolchain nightly
#   - rustup component add miri    --toolchain nightly
#
# Usage:
#   scripts/run-sanitizers.sh [asan|msan|tsan|miri|all]   # default: all
#
# Snapshots are written as docs/sanitizers/<tool>-results-<YYYY-MM-DD>.log

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

OUT_DIR="docs/sanitizers"
mkdir -p "$OUT_DIR"

DATE="$(date +%Y-%m-%d)"
TARGET="${TARGET:-x86_64-unknown-linux-gnu}"

# Common cargo flags. --all-features picks up persistent-artrie + io-uring-backend.
CARGO_TEST_FLAGS=(test --target "$TARGET" --all-features --no-fail-fast)

run_asan() {
    echo "[asan] running…"
    RUSTFLAGS="-Z sanitizer=address" \
        cargo +nightly "${CARGO_TEST_FLAGS[@]}" \
        2>&1 | tee "$OUT_DIR/asan-results-$DATE.log"
}

run_msan() {
    echo "[msan] running…"
    RUSTFLAGS="-Z sanitizer=memory -Z sanitizer-memory-track-origins" \
        cargo +nightly "${CARGO_TEST_FLAGS[@]}" \
        2>&1 | tee "$OUT_DIR/msan-results-$DATE.log"
}

run_tsan() {
    echo "[tsan] running…"
    RUSTFLAGS="-Z sanitizer=thread" \
        cargo +nightly "${CARGO_TEST_FLAGS[@]}" \
        2>&1 | tee "$OUT_DIR/tsan-results-$DATE.log"
}

run_miri() {
    echo "[miri] running… (subset; full suite is too slow under miri)"
    cargo +nightly miri test --no-fail-fast \
        2>&1 | tee "$OUT_DIR/miri-results-$DATE.log"
}

WHICH="${1:-all}"
case "$WHICH" in
    asan) run_asan ;;
    msan) run_msan ;;
    tsan) run_tsan ;;
    miri) run_miri ;;
    all)  run_asan; run_msan; run_tsan; run_miri ;;
    *)
        echo "usage: $0 [asan|msan|tsan|miri|all]" >&2
        exit 2
        ;;
esac

echo "[done] snapshots written to $OUT_DIR/"
