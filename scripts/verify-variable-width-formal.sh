#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
rocq_root="$repo_root/formal-verification/rocq"
tla_root="$repo_root/formal-verification/tla+"
artifact_root="${VARIABLE_WIDTH_FORMAL_ARTIFACT_ROOT:-/tmp/libdictenstein-variable-width-formal}"
log_root="$artifact_root/logs"
state_root="$artifact_root/tlc-state-spaces"

command_timeout_seconds="${VARIABLE_WIDTH_FORMAL_TIMEOUT_SECONDS:-7200}"
tlc_java_options="${VARIABLE_WIDTH_TLC_JAVA_OPTIONS:--Xms64m -Xmx512m -XX:+UseParallelGC}"
coqc_bin="${COQC_BIN:-$(command -v coqc || true)}"
coqchk_bin="${COQCHK_BIN:-$(command -v coqchk || true)}"
expected_identifier_count=190
run_number=0
last_log=""

mkdir -p "$log_root" "$state_root"

if [[ -z "$coqc_bin" || -z "$coqchk_bin" ]]; then
  echo 'ERROR: coqc and coqchk must be available before running the formal gate' >&2
  exit 1
fi

cleanup_state_directory() {
  local state_directory="$1"
  case "$state_directory" in
    "$state_root"/*)
      rm -rf -- "$state_directory"
      ;;
    *)
      echo "ERROR: refusing to remove unexpected TLC state path: $state_directory" >&2
      return 1
      ;;
  esac
}

assert_no_competing_variable_width_job() {
  local competing
  competing="$(
    ps -eo pid=,ppid=,rss=,stat=,comm=,args= |
      awk '
        $0 ~ /libdictenstein-variable-width-refinement/ &&
        ($5 ~ /^(coqc|coqchk|tlc|tla2sany)$/ ||
         ($5 == "java" && $0 ~ /tlc2\.TLC|tla2sany\.SANY/)) {
          print
        }
      '
  )"
  if [[ -n "$competing" ]]; then
    echo "ERROR: another heavy variable-width verification process is active:" >&2
    printf '%s\n' "$competing" >&2
    return 1
  fi
}

run_capped_capture() {
  local label="$1"
  local memory_high="$2"
  local memory_max="$3"
  local working_directory="$4"
  shift 4

  local unit_name
  local status=0
  local -a command=("$@")

  assert_no_competing_variable_width_job
  run_number=$((run_number + 1))
  unit_name="libdictenstein-vwenc-formal-${BASHPID}-${run_number}"
  last_log="$(mktemp "$log_root/${label}.XXXXXX.log")"

  if [[ "$command_timeout_seconds" != "0" ]]; then
    command=(
      timeout --foreground --signal=TERM --kill-after=30s
      "$command_timeout_seconds"
      "${command[@]}"
    )
  fi

  if systemd-run --user --unit="$unit_name" --wait --pipe --quiet --collect \
      --working-directory="$working_directory" \
      --property="MemoryHigh=$memory_high" \
      --property="MemoryMax=$memory_max" \
      --property=MemorySwapMax=0 \
      --property=CPUQuota=100% \
      --property=TasksMax=32 \
      "${command[@]}" >"$last_log" 2>&1; then
    status=0
  else
    status=$?
  fi

  cat "$last_log"
  return "$status"
}

run_required() {
  local status=0
  if run_capped_capture "$@"; then
    status=0
  else
    status=$?
  fi
  if [[ "$status" -ne 0 ]]; then
    echo "ERROR: required verification command failed with status $status" >&2
    echo "Output retained at $last_log" >&2
    return "$status"
  fi
  rm -f -- "$last_log"
}

verify_cfg_inventory() {
  local module="$1"
  shift
  local -a configs=("$@")
  local model_inventory
  local config_inventory

  model_inventory="$(
    sed -n 's/^\(VWENC_[A-Za-z0-9_]*\) ==.*/\1/p' "$tla_root/${module}.tla" |
      LC_ALL=C sort
  )"
  config_inventory="$(
    for config in "${configs[@]}"; do
      sed -n 's/^[[:space:]]*\(VWENC_[A-Za-z0-9_]*\)[[:space:]]*$/\1/p' \
        "$tla_root/${config}.cfg"
    done | LC_ALL=C sort -u
  )"

  if [[ -z "$model_inventory" ]]; then
    echo "ERROR: $module declares no VWENC assertions" >&2
    return 1
  fi
  if [[ "$model_inventory" != "$config_inventory" ]]; then
    echo "ERROR: ${configs[*]} do not collectively check the exact VWENC assertion inventory of ${module}.tla" >&2
    diff -u \
      <(printf '%s\n' "$model_inventory") \
      <(printf '%s\n' "$config_inventory") || true
    return 1
  fi
}

verify_stable_identifier_inventory() {
  local duplicate_names
  local duplicate_numbers
  local -a identifiers=()

  mapfile -t identifiers < <(
    {
      sed -n -E \
        's/^(Theorem|Lemma|Corollary) (VWENC_[A-Za-z0-9_]+).*/\2/p' \
        "$rocq_root/Spec/VariableWidthCodecSpec.v" \
        "$rocq_root/Spec/VariableWidthInterningSpec.v"
      sed -n 's/^\(VWENC_[A-Za-z0-9_]*\) ==.*/\1/p' \
        "$tla_root/VariableWidthCodecBoundary.tla" \
        "$tla_root/VariableWidthVocabularyInterning.tla" \
        "$tla_root/VariableWidthVocabularyPublication.tla"
    } | LC_ALL=C sort
  )

  if [[ "${#identifiers[@]}" -ne "$expected_identifier_count" ]]; then
    echo "ERROR: discovered ${#identifiers[@]} stable VWENC declarations; expected $expected_identifier_count" >&2
    exit 1
  fi

  duplicate_names="$(printf '%s\n' "${identifiers[@]}" | uniq -d)"
  duplicate_numbers="$(
    printf '%s\n' "${identifiers[@]}" |
      sed 's/^VWENC_\([0-9][0-9]*\)_.*/\1/' |
      LC_ALL=C sort -n |
      uniq -d
  )"
  if [[ -n "$duplicate_names" || -n "$duplicate_numbers" ]]; then
    echo 'ERROR: stable VWENC identifiers are not globally unique' >&2
    [[ -z "$duplicate_names" ]] || printf 'Duplicate names:\n%s\n' "$duplicate_names" >&2
    [[ -z "$duplicate_numbers" ]] || printf 'Duplicate numbers:\n%s\n' "$duplicate_numbers" >&2
    exit 1
  fi
}

run_tlc_safe() {
  local label="$1"
  local module="$2"
  local config="$3"
  local state_directory
  local status=0

  state_directory="$(mktemp -d "$state_root/${label}.XXXXXX")"
  if run_capped_capture "$label" 512M 1G "$tla_root" \
      env "JAVA_TOOL_OPTIONS=$tlc_java_options" \
      tlc -workers 1 -metadir "$state_directory" \
      -config "${config}.cfg" "${module}.tla"; then
    status=0
  else
    status=$?
  fi

  if [[ "$status" -ne 0 ]]; then
    echo "ERROR: safe TLC model $label failed with status $status" >&2
    echo "Output retained at $last_log" >&2
    echo "State space retained at $state_directory" >&2
    return "$status"
  fi
  if ! grep -Fq 'Model checking completed. No error has been found.' "$last_log"; then
    echo "ERROR: safe TLC model $label did not report complete error-free exploration" >&2
    echo "Output retained at $last_log" >&2
    echo "State space retained at $state_directory" >&2
    return 1
  fi

  cleanup_state_directory "$state_directory"
  rm -f -- "$last_log"
}

run_tlc_negative_control() {
  local label="$1"
  local module="$2"
  local config="$3"
  local expected_invariant="$4"
  local state_directory
  local status=0

  state_directory="$(mktemp -d "$state_root/${label}.XXXXXX")"
  if run_capped_capture "$label" 512M 1G "$tla_root" \
      env "JAVA_TOOL_OPTIONS=$tlc_java_options" \
      tlc -workers 1 -metadir "$state_directory" \
      -config "${config}.cfg" "${module}.tla"; then
    status=0
  else
    status=$?
  fi

  if [[ "$status" -ne 12 ]]; then
    echo "ERROR: $config exited with $status; expected TLC invariant-violation status 12" >&2
    echo "Output retained at $last_log" >&2
    echo "State space retained at $state_directory" >&2
    return 1
  fi
  if ! grep -Fxq \
      -e "Error: Invariant ${expected_invariant} is violated." \
      -e "Error: Invariant ${expected_invariant} is violated by the initial state:" \
      "$last_log"; then
    echo "ERROR: $config did not violate exactly $expected_invariant" >&2
    echo "Output retained at $last_log" >&2
    echo "State space retained at $state_directory" >&2
    return 1
  fi

  cleanup_state_directory "$state_directory"
  rm -f -- "$last_log"
  echo "OK: $config rejected by $expected_invariant"
}

echo '== Variable-width formal source integrity =='
if rg -n \
    '(^|[^[:alnum:]_])(Admitted|admit|Axiom|Axioms|Parameter|Parameters|Conjecture|Abort)([^[:alnum:]_]|$)' \
    "$rocq_root/Spec/VariableWidthCodecSpec.v" \
    "$rocq_root/Spec/VariableWidthInterningSpec.v"; then
  echo 'ERROR: variable-width Rocq sources contain a proof escape' >&2
  exit 1
fi

if rg -n 'TODO|FIXME|HACK|XXX' \
    "$rocq_root/Spec/VariableWidthCodecSpec.v" \
    "$rocq_root/Spec/VariableWidthInterningSpec.v" \
    "$tla_root/VariableWidthCodecBoundary.tla" \
    "$tla_root/VariableWidthVocabularyInterning.tla" \
    "$tla_root/VariableWidthVocabularyPublication.tla"; then
  echo 'ERROR: variable-width formal sources contain an incompletion marker' >&2
  exit 1
fi

"$repo_root/scripts/verify-variable-width-correspondence.sh"
verify_stable_identifier_inventory

verify_cfg_inventory VariableWidthCodecBoundary VariableWidthCodecBoundary
verify_cfg_inventory VariableWidthVocabularyInterning \
  VariableWidthVocabularyInterning \
  VariableWidthVocabularyInterning_MultiSpan
verify_cfg_inventory VariableWidthVocabularyPublication \
  VariableWidthVocabularyPublication \
  VariableWidthVocabularyPublication_TermFiberWitness

if ! grep -Fxq 'INIT MultiSpanInit' \
    "$tla_root/VariableWidthVocabularyInterning_MultiSpan.cfg" ||
   ! grep -Eq '^[[:space:]]*VWENC_180_MULTISPAN_WITNESS_IS_CONCRETE[[:space:]]*$' \
    "$tla_root/VariableWidthVocabularyInterning_MultiSpan.cfg" ||
   ! grep -Eq '^[[:space:]]*VWENC_175_PACKED_SPANS_ARE_DISJOINT_AND_COVER_BYTES_EXACTLY[[:space:]]*$' \
    "$tla_root/VariableWidthVocabularyInterning_MultiSpan.cfg"; then
  echo 'ERROR: the multi-span TLC control is not bound to its concrete two-span witness and exact-coverage law' >&2
  exit 1
fi

if ! grep -Fxq 'INIT TermFiberWitnessInit' \
    "$tla_root/VariableWidthVocabularyPublication_TermFiberWitness.cfg" ||
   ! grep -Eq '^[[:space:]]*VWENC_179_EXACT_TERM_FIBER_SEPARATES_SAME_RAW_ID[[:space:]]*$' \
    "$tla_root/VariableWidthVocabularyPublication_TermFiberWitness.cfg" ||
   ! grep -Eq '^[[:space:]]*VWENC_193_TWO_GENERATION_TERM_FIBER_WITNESS_IS_CONCRETE[[:space:]]*$' \
    "$tla_root/VariableWidthVocabularyPublication_TermFiberWitness.cfg"; then
  echo 'ERROR: the term-fiber TLC control is not bound to its concrete two-generation witness and exact-separation law' >&2
  exit 1
fi

echo '== Rocq proofs =='
run_required rocq-codec 1G 2G "$rocq_root" \
  "$coqc_bin" -Q . ARTrie Spec/VariableWidthCodecSpec.v
run_required rocq-interning 1G 2G "$rocq_root" \
  "$coqc_bin" -Q . ARTrie Spec/VariableWidthInterningSpec.v
run_required rocq-kernel-check 1G 2G "$rocq_root" \
  "$coqchk_bin" -Q . ARTrie \
  ARTrie.Spec.VariableWidthCodecSpec \
  ARTrie.Spec.VariableWidthInterningSpec

echo '== TLA+ syntax and semantic analysis =='
for module in \
  VariableWidthCodecBoundary \
  VariableWidthVocabularyInterning \
  VariableWidthVocabularyPublication
do
  run_required "sany-${module}" 512M 1G "$tla_root" \
    env "JAVA_TOOL_OPTIONS=$tlc_java_options" tla2sany "${module}.tla"
done

echo '== Complete safe-state exploration =='
run_tlc_safe codec-safe \
  VariableWidthCodecBoundary VariableWidthCodecBoundary
run_tlc_safe interning-safe \
  VariableWidthVocabularyInterning VariableWidthVocabularyInterning
run_tlc_safe interning-multispan \
  VariableWidthVocabularyInterning VariableWidthVocabularyInterning_MultiSpan
run_tlc_safe publication-safe \
  VariableWidthVocabularyPublication VariableWidthVocabularyPublication
run_tlc_safe publication-term-fiber \
  VariableWidthVocabularyPublication \
  VariableWidthVocabularyPublication_TermFiberWitness

echo '== Deliberately unsafe controls =='
while IFS='|' read -r label module config invariant; do
  run_tlc_negative_control "$label" "$module" "$config" "$invariant"
done <<'NEGATIVE_CONTROLS'
codec-overlong|VariableWidthCodecBoundary|VariableWidthCodecBoundary_OverlongUnsafe|VWENC_26_OVERLONG_ULEB_IS_REJECTED
codec-unterminated|VariableWidthCodecBoundary|VariableWidthCodecBoundary_UnterminatedUnsafe|VWENC_27_UNTERMINATED_ULEB_IS_REJECTED
codec-utf8-continuation|VariableWidthCodecBoundary|VariableWidthCodecBoundary_Utf8ContinuationUnsafe|VWENC_28_UTF8_CONTINUATION_IS_REJECTED
codec-physical-exposure|VariableWidthCodecBoundary|VariableWidthCodecBoundary_PhysicalExposureUnsafe|VWENC_24_CODEC_BYTES_NEVER_BECOME_LOGICAL_TRANSITIONS
interning-fingerprint-only|VariableWidthVocabularyInterning|VariableWidthVocabularyInterning_FingerprintOnlyUnsafe|VWENC_142_FINGERPRINT_COLLISIONS_NEVER_ALIAS_DISTINCT_ATOMS
interning-id-reuse|VariableWidthVocabularyInterning|VariableWidthVocabularyInterning_IdReuseUnsafe|VWENC_143_RETIRED_ID_IS_NEVER_CLAIMED_OR_LIVE_AGAIN
publication-sequence-before-vocabulary|VariableWidthVocabularyPublication|VariableWidthVocabularyPublication_SequenceBeforeVocabularyUnsafe|VWENC_149_DURABLE_SEQUENCE_REFERENCES_DURABLE_BOUND_VOCABULARY
publication-frontier-overclaim|VariableWidthVocabularyPublication|VariableWidthVocabularyPublication_FrontierOverclaimUnsafe|VWENC_147_PUBLISHED_FRONTIER_DOES_NOT_EXCEED_DURABLE_FRONTIER
publication-cross-generation-resume|VariableWidthVocabularyPublication|VariableWidthVocabularyPublication_CrossGenerationResumeUnsafe|VWENC_154_CAPTURED_CONTINUATION_RESUMES_IMMUTABLE_PAIR
publication-missing-as-empty|VariableWidthVocabularyPublication|VariableWidthVocabularyPublication_MissingVocabularyAsEmptyUnsafe|VWENC_178_RECOVERY_NEVER_SYNTHESIZES_EMPTY_SUCCESS
NEGATIVE_CONTROLS

echo 'Variable-width formal gate passed.'
