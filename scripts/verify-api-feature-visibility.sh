#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixtures="$repo_root/tests/fixtures/api-feature-visibility"
scratch="$repo_root/target/api-feature-visibility"

case "$scratch" in
  "$repo_root"/target/api-feature-visibility) ;;
  *)
    echo "refusing unsafe scratch path: $scratch" >&2
    exit 1
    ;;
esac

cleanup() {
  rm -rf -- "$scratch"
}
trap cleanup EXIT
cleanup
mkdir -p "$scratch"
cp -R "$fixtures/feature-on" "$scratch/"
cp -R "$fixtures/feature-off" "$scratch/"
sed -i "s|__LIBDICTENSTEIN_PATH__|$repo_root|g" \
  "$scratch/feature-on/Cargo.toml" \
  "$scratch/feature-off/Cargo.toml"

export CARGO_TARGET_DIR="$scratch/cargo-target"
cargo check --quiet --offline --manifest-path "$scratch/feature-on/Cargo.toml"

set +e
cargo check --offline --manifest-path "$scratch/feature-off/Cargo.toml" \
  >"$scratch/feature-off.log" 2>&1
status=$?
set -e
if [ "$status" -eq 0 ]; then
  echo "persistent serialization instrumentation leaked without its feature" >&2
  exit 1
fi
if ! grep -Eq 'unresolved imports?.*persistent_serialization_stats|no `PersistentSerializationStats` in the root' "$scratch/feature-off.log"; then
  cat "$scratch/feature-off.log" >&2
  echo "feature-off consumer failed for an unexpected reason" >&2
  exit 1
fi

echo "performance-instrumentation API feature visibility passed"
