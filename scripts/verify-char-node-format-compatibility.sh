#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
scratch="$repo_root/target/char-format-compat"
baseline_commit="6a1b267a60fe9c445a0c8c7c8136e6dd40aedbf5"
interop_commit="6694ad4fcb5ce498f69b77cb14ce1ea7a2f20033"
llattice_commit="2ec21ca70ae3cbb2d8afdd295c9ed09517003324"
interop_repo="${VINARY_TREE_INTEROP_REPO:-$repo_root/../vinary-tree-interop-rc2-stack-safety-clean}"
llattice_repo="${LLATTICE_REPO:-$repo_root/../llattice}"
fixture_dir="$repo_root/tests/fixtures/char-node-format"
manifest="$fixture_dir/manifest.toml"

case "$scratch" in
  "$repo_root"/target/char-format-compat) ;;
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
mkdir -p "$scratch/baseline" "$scratch/llattice" "$scratch/liblevenshtein-rust/vinary-tree-interop"

require_commit() {
  local repository="$1"
  local expected="$2"
  local actual
  # CI checkouts are intentionally shallow.  Fetch the immutable fixture
  # revision on demand so the compatibility proof does not depend on an
  # incidental checkout depth while still refusing any substituted object.
  if ! actual="$(git -C "$repository" rev-parse --verify "${expected}^{commit}" 2>/dev/null)"; then
    git -C "$repository" fetch --no-tags --depth=1 origin "$expected"
    actual="$(git -C "$repository" rev-parse --verify "${expected}^{commit}")"
  fi
  if [ "$actual" != "$expected" ]; then
    echo "commit mismatch for $repository: expected $expected, found $actual" >&2
    exit 1
  fi
}

require_commit "$repo_root" "$baseline_commit"
require_commit "$interop_repo" "$interop_commit"
require_commit "$llattice_repo" "$llattice_commit"

git -C "$repo_root" archive "$baseline_commit" | tar -x -C "$scratch/baseline"
git -C "$interop_repo" archive "$interop_commit" |
  tar -x -C "$scratch/liblevenshtein-rust/vinary-tree-interop"
git -C "$llattice_repo" archive "$llattice_commit" | tar -x -C "$scratch/llattice"
patch --directory="$scratch/baseline" --strip=1 --input="$fixture_dir/baseline-dependency-binding.patch"
mkdir -p "$scratch/baseline/examples" "$scratch/baseline/tests/support"
cp "$repo_root/examples/char_node_format_probe.rs" "$scratch/baseline/examples/"
cp "$repo_root/tests/support/char_node_format_cases.rs" "$scratch/baseline/tests/support/"

baseline_source_sha="$(
  git -C "$repo_root" show "$baseline_commit:src/persistent_artrie/char/serialization_char.rs" |
    sha256sum | cut -d' ' -f1
)"
current_source_sha="$(sha256sum "$repo_root/src/persistent_artrie/char/serialization_char.rs" | cut -d' ' -f1)"
baseline_manifest_sha="$(
  sed -n '/^\[baseline\]/,/^\[/s/^serialization_source_sha256 = "\([^"]*\)"/\1/p' "$manifest"
)"
if [ "$baseline_manifest_sha" != "$baseline_source_sha" ]; then
  echo "baseline serialization source digest disagrees with manifest" >&2
  exit 1
fi
current_manifest_sha="$(
  sed -n '/^\[current\]/,/^\[/s/^serialization_source_sha256 = "\([^"]*\)"/\1/p' "$manifest"
)"
if [ "$current_manifest_sha" != "$current_source_sha" ]; then
  echo "current serialization source digest disagrees with manifest" >&2
  exit 1
fi

export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-12}"
export CARGO_TARGET_DIR="$scratch/current-target"
cargo run --quiet --locked --offline --manifest-path "$repo_root/Cargo.toml" --no-default-features --features persistent-artrie --example char_node_format_probe -- emit current "$current_source_sha" > "$scratch/current-writer.txt"
cmp "$scratch/current-writer.txt" "$fixture_dir/current-writer.txt"
cargo run --quiet --locked --offline --manifest-path "$repo_root/Cargo.toml" --no-default-features --features persistent-artrie --example char_node_format_probe -- verify "$fixture_dir/baseline-v2-6a1b267.txt" 3
cargo run --quiet --locked --offline --manifest-path "$repo_root/Cargo.toml" --no-default-features --features persistent-artrie --example char_node_format_probe -- verify "$fixture_dir/current-writer.txt" 3

export CARGO_TARGET_DIR="$scratch/baseline-target"
cargo run --quiet --offline --manifest-path "$scratch/baseline/Cargo.toml" --no-default-features --features persistent-artrie --example char_node_format_probe -- emit baseline "$baseline_commit" > "$scratch/baseline-v2-6a1b267.txt"
cmp "$scratch/baseline-v2-6a1b267.txt" "$fixture_dir/baseline-v2-6a1b267.txt"
cargo run --quiet --offline --manifest-path "$scratch/baseline/Cargo.toml" --no-default-features --features persistent-artrie --example char_node_format_probe -- verify "$fixture_dir/baseline-v2-6a1b267.txt" 2
cargo run --quiet --offline --manifest-path "$scratch/baseline/Cargo.toml" --no-default-features --features persistent-artrie --example char_node_format_probe -- verify "$fixture_dir/current-writer.txt" 2

printf '%s  %s\n' "$(sed -n '/^\[baseline\]/,/^\[/s/^corpus_sha256 = "\([^"]*\)"/\1/p' "$manifest")" "$fixture_dir/baseline-v2-6a1b267.txt" |
  sha256sum --check --status
printf '%s  %s\n' "$(sed -n '/^\[current\]/,/^\[/s/^corpus_sha256 = "\([^"]*\)"/\1/p' "$manifest")" "$fixture_dir/current-writer.txt" |
  sha256sum --check --status

echo "character-node V2/V3 cross-version corpus verification passed"
