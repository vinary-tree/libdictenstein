#!/usr/bin/env bash
# Reproduce the byte-DAWG child-Arc reclamation proof and its failing control.
# All checker processes are serial, memory-capped, and use disk-backed scratch.
set -euo pipefail

repo_root="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
model_dir="$repo_root/formal-verification/tla+"
rocq_dir="$repo_root/formal-verification/rocq"
stage="${1:-$repo_root/.scratch/byte-dawg-drop-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
mkdir -p "$stage/logs" "$stage/tmp" "$stage/safe-meta" \
  "$stage/shared-meta" "$stage/unsafe-meta"
stage="$(realpath "$stage")"

for tool in systemd-run tla2sany tlc coqc coqchk coqtop rg sha256sum; do
  command -v "$tool" >/dev/null || {
    printf 'Missing required verifier: %s\n' "$tool" >&2
    exit 2
  }
done

sha256sum \
  "$model_dir/LockFreeDawgDropRace.tla" \
  "$model_dir/LockFreeDawgDropRace_Safe.cfg" \
  "$model_dir/LockFreeDawgDropRace_SafeShared.cfg" \
  "$model_dir/LockFreeDawgDropRace_TryUnwrapUnsafe.cfg" \
  "$rocq_dir/Spec/OverlayArcOwnershipSpec.v" \
  "$rocq_dir/Spec/LockFreeDawgDropSpec.v" \
  "$repo_root/scripts/verify-rsdict-byte-dawg-drop-formal.sh" \
  > "$stage/sources.sha256"
sha256sum "$(command -v tla2sany)" "$(command -v tlc)" \
  "$(command -v coqc)" "$(command -v coqchk)" \
  "$(command -v coqtop)" "$(command -v systemd-run)" \
  > "$stage/tools.sha256"

run_checked() {
  local label="$1"; shift
  systemd-run --user --scope \
    -p MemoryHigh=768M -p MemoryMax=1024M -p MemorySwapMax=0 \
    -p CPUQuota=100% -p TasksMax=64 \
    "$@" > "$stage/logs/$label.log" 2>&1
}

cd "$model_dir"
run_checked sany env JAVA_TOOL_OPTIONS=-Xmx512m \
  _JAVA_OPTIONS="-Djava.io.tmpdir=$stage/tmp" \
  tla2sany LockFreeDawgDropRace.tla
# tla2sany can exit zero despite semantic errors, so the log is authoritative.
if rg -q 'Semantic errors:|\*\*\* Errors:|Error:|Exception' "$stage/logs/sany.log"; then
  printf 'SANY reported a syntax or semantic error: %s\n' "$stage/logs/sany.log" >&2
  exit 1
fi
rg -q 'Semantic processing of module LockFreeDawgDropRace' "$stage/logs/sany.log"

for case in safe shared; do
  if [[ "$case" == safe ]]; then
    cfg=LockFreeDawgDropRace_Safe.cfg
  else
    cfg=LockFreeDawgDropRace_SafeShared.cfg
  fi
  run_checked "$case" env JAVA_TOOL_OPTIONS=-Xmx512m \
    _JAVA_OPTIONS="-Djava.io.tmpdir=$stage/tmp" \
    tlc -workers 1 -seed 1 -fp 0 -metadir "$stage/$case-meta" \
    -config "$cfg" LockFreeDawgDropRace.tla
  rg -q 'Model checking completed. No error has been found.' \
    "$stage/logs/$case.log"
  rg -q '0 states left on queue' "$stage/logs/$case.log"
done

if run_checked unsafe env JAVA_TOOL_OPTIONS=-Xmx512m \
  _JAVA_OPTIONS="-Djava.io.tmpdir=$stage/tmp" \
  tlc -workers 1 -seed 1 -fp 0 -metadir "$stage/unsafe-meta" \
  -config LockFreeDawgDropRace_TryUnwrapUnsafe.cfg LockFreeDawgDropRace.tla; then
  printf 'Unsafe control unexpectedly passed\n' >&2
  exit 1
else
  unsafe_exit=$?
fi
[[ "$unsafe_exit" -eq 12 ]] || {
  printf 'Unsafe control exited %s instead of TLC invariant-failure code 12\n' \
    "$unsafe_exit" >&2
  exit 1
}
rg -q '^Error: Invariant RecursiveDropBound is violated\.$' \
  "$stage/logs/unsafe.log"
[[ "$(rg -c '^Error: Invariant ' "$stage/logs/unsafe.log")" -eq 1 ]]
rg -q 'frames = \[destructor \|-> <<0, 1, 2>>' "$stage/logs/unsafe.log"

cd "$rocq_dir"
run_checked rocq-arc coqc -noglob -Q . ARTrie \
  Spec/OverlayArcOwnershipSpec.v
run_checked rocq-dawg coqc -noglob -Q . ARTrie \
  Spec/LockFreeDawgDropSpec.v
run_checked rocq-kernel coqchk -silent -Q . ARTrie \
  ARTrie.Spec.LockFreeDawgDropSpec
run_checked rocq-assumptions coqtop -quiet -Q . ARTrie <<'EOF'
Require Import ARTrie.Spec.LockFreeDawgDropSpec.
Print Assumptions RSDICT_DAWG_000_admitted_edges_decrease_rank.
Print Assumptions RSDICT_DAWG_001_all_edge_tokens_counted.
Print Assumptions RSDICT_DAWG_002_retained_handle_adds_one_token.
Print Assumptions RSDICT_DAWG_003_into_inner_never_invokes_payload_drop.
Print Assumptions RSDICT_DAWG_004_retained_owner_forces_shared_release.
Print Assumptions RSDICT_DAWG_005_one_safe_step_preserves_native_bound.
Print Assumptions RSDICT_DAWG_006_unbounded_safe_trace_native_bound.
Print Assumptions RSDICT_DAWG_007_safe_edge_source_correspondence_boundary.
Print Assumptions RSDICT_DAWG_008_admitted_paths_are_acyclic.
Print Assumptions RSDICT_DAWG_009_no_admitted_payload_cycle.
Print Assumptions RSDICT_DAWG_010_published_and_captured_roots_count_separately.
Print Assumptions RSDICT_DAWG_011_each_progress_step_strictly_decreases_potential.
Print Assumptions RSDICT_DAWG_012_progress_trace_has_finite_step_budget.
Print Assumptions RSDICT_DAWG_013_lifecycle_step_preserves_strong_and_destroy_state.
Print Assumptions RSDICT_DAWG_014_unbounded_lifecycle_has_no_premature_or_double_destroy.
Quit.
EOF
[[ "$(rg -c 'Closed under the global context' \
  "$stage/logs/rocq-assumptions.log")" -eq 15 ]]
if rg -q 'Error|Axioms:' "$stage/logs/rocq-assumptions.log"; then
  printf 'Rocq assumption audit failed: %s\n' \
    "$stage/logs/rocq-assumptions.log" >&2
  exit 1
fi

sha256sum "$stage"/logs/*.log > "$stage/logs.sha256"
printf 'PASS byte-DAWG formal proof: %s\n' "$stage"
