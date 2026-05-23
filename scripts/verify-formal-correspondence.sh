#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

echo "== Rust correspondence tests =="
cargo test --features persistent-artrie --test persistent_artrie_formal_correspondence
cargo test --features persistent-artrie --test persistent_artrie_storage_correspondence
cargo test --features persistent-artrie --test persistent_artrie_loom_correspondence
cargo test \
  --features "persistent-artrie group-commit" \
  --test persistent_artrie_formal_correspondence \
  group_commit_writes_returned_lsns_in_wal_order

echo "== Rocq proofs =="
make -C formal-verification/rocq -j1

echo "== TLA+ syntax checks =="
if command -v tla2sany >/dev/null 2>&1; then
  (
    cd formal-verification/tla+
    for module in \
      DocumentTransactions \
      AsyncWalGroupCommit \
      VersionLifecycle \
      MmapBlockStorage \
      LockFreeARTrieLinearizability \
      ByzantineStorage \
      HotStuffConsensus
    do
      tla2sany "${module}.tla"
    done
  )
else
  echo "Skipping SANY checks: tla2sany is not on PATH"
fi

if [ "${RUN_TLC:-0}" = "1" ]; then
  echo "== TLC bounded model checks =="
  if ! command -v tlc >/dev/null 2>&1; then
    echo "RUN_TLC=1 was set, but tlc is not on PATH" >&2
    exit 1
  fi

  (
    cd formal-verification/tla+
    for module in \
      DocumentTransactions \
      AsyncWalGroupCommit \
      VersionLifecycle \
      MmapBlockStorage \
      LockFreeARTrieLinearizability \
      ByzantineStorage \
      HotStuffConsensus
    do
      tlc -workers 1 -config "${module}.cfg" "${module}.tla"
    done
  )
else
  echo "Skipping TLC model checking; set RUN_TLC=1 to enable bounded TLC runs"
fi
