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
resource_control="${VARIABLE_WIDTH_FORMAL_RESOURCE_CONTROL:-systemd}"
coqc_bin="${COQC_BIN:-$(command -v coqc || true)}"
coqchk_bin="${COQCHK_BIN:-$(command -v coqchk || true)}"
expected_identifier_count=246
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

  local status=0
  local -a command=("$@")

  assert_no_competing_variable_width_job
  run_number=$((run_number + 1))
  last_log="$(mktemp "$log_root/${label}.XXXXXX.log")"

  if [[ "$command_timeout_seconds" != "0" ]]; then
    command=(
      timeout --foreground --signal=TERM --kill-after=30s
      "$command_timeout_seconds"
      "${command[@]}"
    )
  fi

  case "$resource_control" in
    systemd)
      if systemd-run --user --wait --pipe --quiet --collect \
          --working-directory="$working_directory" \
          --property="MemoryHigh=$memory_high" \
          --property="MemoryMax=$memory_max" \
          --property=MemorySwapMax=0 \
          --property=CPUQuota=100% \
          --property=TasksMax=128 \
          "${command[@]}" >"$last_log" 2>&1; then
        status=0
      else
        status=$?
      fi
      ;;
    external)
      local memory_limit_bytes
      case "$memory_max" in
        512M) memory_limit_bytes=536870912 ;;
        1G) memory_limit_bytes=1073741824 ;;
        2G) memory_limit_bytes=2147483648 ;;
        8G) memory_limit_bytes=8589934592 ;;
        *)
          echo "ERROR: unsupported external memory limit: $memory_max" >&2
          return 1
          ;;
      esac
      if (cd "$working_directory" &&
          # OCaml/Rocq reserves a large virtual heap up front.  Capping
          # address space rejects that reservation before execution begins;
          # RSS is the relevant resident-memory safety bound here.
          prlimit --rss="$memory_limit_bytes" \
            "${command[@]}" >"$last_log" 2>&1); then
        status=0
      else
        status=$?
      fi
      ;;
    *)
      echo "ERROR: unsupported VARIABLE_WIDTH_FORMAL_RESOURCE_CONTROL=$resource_control" >&2
      return 1
      ;;
  esac

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
        "$rocq_root/Spec/VariableWidthInterningSpec.v" \
        "$rocq_root/Spec/VariableWidthFamilyRefinementSpec.v"
      sed -n 's/^\(VWENC_[A-Za-z0-9_]*\) ==.*/\1/p' \
        "$tla_root/VariableWidthCodecBoundary.tla" \
        "$tla_root/VariableWidthVocabularyInterning.tla" \
        "$tla_root/VariableWidthVocabularyPublication.tla" \
        "$tla_root/VariableWidthFamilyRefinement.tla"
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

verify_family_refinement_identifier_manifest() {
  local -a expected_rocq=(
    VWENC_194_LOGICAL_OBSERVATIONAL_EQUIVALENCE_IS_AN_EQUIVALENCE
    VWENC_195_MEMBERSHIP_AND_TERMINALITY_ARE_LOGICAL_OBSERVATIONS
    VWENC_196_MAPPED_VALUE_PRESENCE_AND_IDENTITY_ARE_OBSERVABLE
    VWENC_197_ORDERED_LOGICAL_OUTGOING_LABELS_ARE_OBSERVABLE
    VWENC_198_PREFIX_ENTRIES_ARE_LOGICAL_OBSERVATIONS
    VWENC_199_FULL_ENUMERATION_ORDER_IS_DETERMINISTIC_AND_OBSERVABLE
    VWENC_200_APPLICABLE_SUBSTRING_RESULTS_ARE_LOGICAL_OBSERVATIONS
    VWENC_201_APPLICABLE_SUFFIX_RESULTS_ARE_LOGICAL_OBSERVATIONS
    VWENC_202_PHYSICAL_LAYOUT_AND_CODEC_STAGING_STATE_ARE_NONOBSERVABLE
    VWENC_203_DICTIONARY_FAMILY_INVENTORY_IS_EXHAUSTIVE
    VWENC_204_FAMILY_PROFILE_MATRIX_IS_TOTAL_AND_FUNCTIONAL
    VWENC_205_FAMILY_SURFACE_MATRIX_IS_TOTAL_AND_FUNCTIONAL
    VWENC_206_FAMILY_PROFILE_SURFACE_MATRIX_IS_TOTAL
    VWENC_207_EVERY_INAPPLICABLE_CELL_HAS_AN_EXPLICIT_STRUCTURAL_REASON
    VWENC_208_PATHMAP_REMAINS_AN_EXTERNAL_BYTE_KEYED_ADAPTER
    VWENC_209_PATHMAP_CANONICAL_ULEB_USES_ONLY_FIXED_WIDTH_INTERNED_IDS
    VWENC_210_LEGACY_ONE_PARAMETER_FAMILY_SPELLING_DEFAULTS_TO_BYTES
    VWENC_211_MAPPED_VALUE_REMAINS_FIRST_AND_WIDTH_IS_NOT_A_PARAMETER
    VWENC_212_PROFILE_ALONE_OWNS_EDGE_UNIT_AND_WIDTH_METADATA
    VWENC_213_OPEN_IN_MEMORY_UNITS_CANNOT_MINT_PERSISTENT_IDENTITIES
    VWENC_214_FORMAT_IDENTITY_IS_INDEPENDENT_OF_RUST_TYPE_NAMES
    VWENC_215_SPECIALIZATION_REFINES_THE_GENERIC_LOGICAL_VIEW
    VWENC_216_EVERY_RETAINED_SPECIALIZED_KERNEL_PRESERVES_ALL_OBSERVATIONS
    VWENC_217_KERNEL_SELECTION_IS_BOUND_ONCE_NOT_BRANCHING_PER_EDGE
    VWENC_218_LEGACY_ALIAS_INVENTORIES_PRESERVE_CANONICAL_TARGETS
    VWENC_219_EVERY_CHAR_ALIAS_TARGETS_UNICODE_SCALAR_UNITS
    VWENC_220_EVERY_U64_ALIAS_PRESERVES_PROFILE_AND_EXPLICIT_LAYOUT
    VWENC_221_DYNAMIC_TO_FROZEN_CONVERSION_PRESERVES_LOGICAL_OBSERVATIONS
    VWENC_222_NODE_ZIPPER_AND_CURSOR_SHARE_ONE_REVISION_BOUND_VIEW
    VWENC_223_FACTORY_COLLECTION_AND_SERIALIZATION_PRESERVE_PROFILE_VIEW
    VWENC_224_SET_COMBINATORS_COMMUTE_WITH_PROFILE_REFINEMENT
    VWENC_225_VALUE_COMBINATORS_COMMUTE_WITH_PROFILE_REFINEMENT
    VWENC_226_ENCODED_ADAPTER_STAGING_BYTES_ARE_HIDDEN_FROM_CONSUMERS
    VWENC_227_PATHMAP_UTF8_GROUPING_EMITS_ONE_UNICODE_SCALAR
    VWENC_228_CANONICAL_ULEB_CODEWORD_EMITS_ONE_OPAQUE_LOGICAL_ATOM
    VWENC_229_CODEWORD_BOUNDARY_OFFSETS_ARE_EXACTLY_LOGICAL_SPLITS
    VWENC_230_RAW_UTF8_SUFFIX_CAN_START_INSIDE_ONE_SCALAR_CODEWORD
    VWENC_231_RAW_ULEB_SUFFIX_CAN_START_INSIDE_ONE_CODEWORD
    VWENC_232_LOGICAL_SUFFIXES_BEGIN_ONLY_AT_CODEWORD_BOUNDARIES
    VWENC_233_RAW_BYTE_SUFFIX_INDEXES_CLAIM_ONLY_BYTE_SEMANTICS
    VWENC_234_DIRECT_UNITS_PRESERVE_ONE_CODEWORD_PER_LOGICAL_EDGE
    VWENC_235_INTERNED_IDS_PRESERVE_ONE_FIXED_CODEWORD_PER_LOGICAL_EDGE
    VWENC_236_CONSUMER_VOCABULARY_BINDING_IS_VALIDATED_ONCE
    VWENC_237_MISMATCHED_VOCABULARY_FIBERS_ARE_REJECTED_BEFORE_TRAVERSAL
    VWENC_238_EVERY_HOT_TRANSITION_HAS_AN_EXACT_FIXED_WIDTH_ENCODING
    VWENC_239_ARBITRARY_WIDTH_BIGUINT_BYTES_STAY_OUTSIDE_HOT_TRAVERSAL
    VWENC_240_DICTIONARY_PROFILES_DO_NOT_OWN_LLATTICE_ALGEBRA
    VWENC_247_HOT_TRAVERSAL_VIEW_EXISTS_IFF_FIBER_BINDING_SUCCEEDS
    VWENC_248_MISMATCHED_FIBER_CANNOT_CONSTRUCT_A_HOT_TRAVERSAL_VIEW
    VWENC_249_BOUND_HOT_VIEWS_CONTAIN_ONLY_EXACT_FIXED_WIDTH_UNITS
  )
  local -a expected_tla=(
    VWENC_241_CODEC_BYTES_NEVER_APPEAR_AS_LOGICAL_LABELS
    VWENC_242_UTF8_SCALAR_IS_NEVER_SPLIT_ACROSS_LOGICAL_TRANSITIONS
    VWENC_243_SUFFIX_MATCHES_NEVER_BEGIN_INSIDE_A_LOGICAL_CODEWORD
    VWENC_244_SPECIALIZED_KERNEL_PRESERVES_THE_COMPLETE_OBSERVATION
    VWENC_245_FORMAT_IDENTITY_COMES_ONLY_FROM_EXPLICIT_PROFILE_METADATA
    VWENC_246_MISMATCHED_VOCABULARY_FIBER_IS_REJECTED_BEFORE_TRAVERSAL
  )
  local -a actual_rocq=()
  local -a actual_tla=()

  mapfile -t actual_rocq < <(
    sed -n -E \
      's/^(Theorem|Lemma|Corollary) (VWENC_[A-Za-z0-9_]+).*/\2/p' \
      "$rocq_root/Spec/VariableWidthFamilyRefinementSpec.v"
  )
  mapfile -t actual_tla < <(
    sed -n 's/^\(VWENC_[A-Za-z0-9_]*\) ==.*/\1/p' \
      "$tla_root/VariableWidthFamilyRefinement.tla"
  )

  if ! diff -u \
      <(printf '%s\n' "${expected_rocq[@]}" | LC_ALL=C sort) \
      <(printf '%s\n' "${actual_rocq[@]}" | LC_ALL=C sort); then
    echo 'ERROR: the exact Rocq family-refinement identifier manifest changed' >&2
    return 1
  fi
  if ! diff -u \
      <(printf '%s\n' "${expected_tla[@]}" | LC_ALL=C sort) \
      <(printf '%s\n' "${actual_tla[@]}" | LC_ALL=C sort); then
    echo 'ERROR: the exact TLA+ family-refinement identifier manifest changed' >&2
    return 1
  fi
}

verify_formal_inventory_extractor() {
  local inventory_file="$artifact_root/formal-inventory.json"
  python3 "$repo_root/scripts/extract-variable-width-formal-inventory.py" \
    --root "$repo_root" --output "$inventory_file"
  python3 - "$inventory_file" "$expected_identifier_count" <<'PY'
import json
import sys

path, expected = sys.argv[1], int(sys.argv[2])
rows = json.load(open(path, encoding="utf-8"))
if len(rows) != expected:
    raise SystemExit(f"extracted {len(rows)} declarations; expected {expected}")
if len({row["id"] for row in rows}) != len(rows):
    raise SystemExit("formal inventory contains duplicate IDs")
controls = sum(bool(row["negative_controls"]) for row in rows)
if controls != 16:
    raise SystemExit(f"extracted {controls} negative-control bindings; expected 16")
print(f"Formal inventory extractor validated {len(rows)} declarations and {controls} controls.")
PY
  rm -f -- "$inventory_file"
}

run_tlc_safe() {
  local label="$1"
  local module="$2"
  local config="$3"
  local state_directory
  local config_path
  local status=0

  state_directory="$(mktemp -d "$state_root/${label}.XXXXXX")"
  if [[ "$config" = /* ]]; then
    config_path="${config}.cfg"
  else
    config_path="$tla_root/${config}.cfg"
  fi
  if run_capped_capture "$label" 512M 1G "$tla_root" \
      env "JAVA_TOOL_OPTIONS=$tlc_java_options" \
      tlc -workers 1 -metadir "$state_directory" \
      -config "$config_path" "${module}.tla"; then
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
  local expected_mutant_constant="$5"
  local state_directory
  local config_path="$tla_root/${config}.cfg"
  local status=0
  local -a selected_invariants=()
  local -a true_mutant_constants=()
  local -a module_invariants=()

  mapfile -t selected_invariants < <(
    sed -n 's/^[[:space:]]*\(VWENC_[A-Za-z0-9_]*\)[[:space:]]*$/\1/p' \
      "$tla_root/${config}.cfg"
  )
  if [[ "${#selected_invariants[@]}" -ne 1 ||
        "${selected_invariants[0]}" != "$expected_invariant" ]]; then
    echo "ERROR: $config must select only $expected_invariant" >&2
    return 1
  fi

  mapfile -t true_mutant_constants < <(
    sed -n 's/^[[:space:]]*\([A-Za-z][A-Za-z0-9_]*\)[[:space:]]*=[[:space:]]*TRUE[[:space:]]*$/\1/p' \
      "$tla_root/${config}.cfg"
  )
  if [[ "${#true_mutant_constants[@]}" -ne 1 ||
        "${true_mutant_constants[0]}" != "$expected_mutant_constant" ]]; then
    echo "ERROR: $config must enable only $expected_mutant_constant" >&2
    return 1
  fi

  mapfile -t module_invariants < <(
    awk '
      /^[[:space:]]*INVARIANT[[:space:]]*$/ { in_invariants=1; next }
      /^[[:space:]]*PROPERTY[[:space:]]*$/ { in_invariants=0 }
      in_invariants && /^[[:space:]]*VWENC_[A-Za-z0-9_]+[[:space:]]*$/ { print $1 }
    ' "$tla_root/${module}.cfg"
  )
  local non_target
  for non_target in "${module_invariants[@]}"; do
    if [[ "$non_target" == "$expected_invariant" ]]; then
      continue
    fi
    local temporary_config
    temporary_config="$(mktemp "$artifact_root/${label}.non-target.XXXXXX.cfg")"
    {
      sed -n '1,/^[[:space:]]*INVARIANT[[:space:]]*$/p' "$config_path"
      printf '  %s\n' "$non_target"
    } >"$temporary_config"
    if ! run_tlc_safe \
        "${label}-preserves-${non_target}" "$module" \
        "${temporary_config%.cfg}"; then
      echo "ERROR: $config also violates non-target invariant $non_target" >&2
      rm -f -- "$temporary_config"
      return 1
    fi
    rm -f -- "$temporary_config"
  done

  state_directory="$(mktemp -d "$state_root/${label}.XXXXXX")"
  if run_capped_capture "$label" 512M 1G "$tla_root" \
      env "JAVA_TOOL_OPTIONS=$tlc_java_options" \
      tlc -workers 1 -metadir "$state_directory" \
      -config "$config_path" "${module}.tla"; then
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
  if grep -Fq \
      "Error: Invariant ${expected_invariant} is violated by the initial state:" \
      "$last_log"; then
    echo "ERROR: $config failed in the initial state instead of after its mutant action" >&2
    echo "Output retained at $last_log" >&2
    echo "State space retained at $state_directory" >&2
    return 1
  fi
  if ! grep -Fxq \
      "Error: Invariant ${expected_invariant} is violated." "$last_log"; then
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
    "$rocq_root/Spec/VariableWidthInterningSpec.v" \
    "$rocq_root/Spec/VariableWidthFamilyRefinementSpec.v"; then
  echo 'ERROR: variable-width Rocq sources contain a proof escape' >&2
  exit 1
fi

if rg -n 'TODO|FIXME|HACK|XXX' \
    "$rocq_root/Spec/VariableWidthCodecSpec.v" \
    "$rocq_root/Spec/VariableWidthInterningSpec.v" \
    "$rocq_root/Spec/VariableWidthFamilyRefinementSpec.v" \
    "$tla_root/VariableWidthCodecBoundary.tla" \
    "$tla_root/VariableWidthVocabularyInterning.tla" \
    "$tla_root/VariableWidthVocabularyPublication.tla" \
    "$tla_root/VariableWidthFamilyRefinement.tla"; then
  echo 'ERROR: variable-width formal sources contain an incompletion marker' >&2
  exit 1
fi

"$repo_root/scripts/verify-variable-width-correspondence.sh"
verify_stable_identifier_inventory
verify_family_refinement_identifier_manifest
verify_formal_inventory_extractor

verify_cfg_inventory VariableWidthCodecBoundary VariableWidthCodecBoundary
verify_cfg_inventory VariableWidthVocabularyInterning \
  VariableWidthVocabularyInterning \
  VariableWidthVocabularyInterning_MultiSpan
verify_cfg_inventory VariableWidthVocabularyPublication \
  VariableWidthVocabularyPublication \
  VariableWidthVocabularyPublication_TermFiberWitness
verify_cfg_inventory VariableWidthFamilyRefinement \
  VariableWidthFamilyRefinement \
  VariableWidthFamilyRefinement_PhysicalExposureUnsafe \
  VariableWidthFamilyRefinement_Utf8SplitUnsafe \
  VariableWidthFamilyRefinement_InteriorSuffixUnsafe \
  VariableWidthFamilyRefinement_SpecializedDivergenceUnsafe \
  VariableWidthFamilyRefinement_TypeNameFormatUnsafe \
  VariableWidthFamilyRefinement_FiberMismatchUnsafe

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
rocq_memory_max="${VARIABLE_WIDTH_FORMAL_ROCQ_MEMORY_MAX:-2G}"
run_required rocq-map-spec 1G "$rocq_memory_max" "$rocq_root" \
  "$coqc_bin" -Q . ARTrie Spec/MapSpec.v
run_required rocq-dictionary-law-spec 1G "$rocq_memory_max" "$rocq_root" \
  "$coqc_bin" -Q . ARTrie Spec/DictionaryLawSpec.v
run_required rocq-dawg-mutation-spec 1G "$rocq_memory_max" "$rocq_root" \
  "$coqc_bin" -Q . ARTrie Spec/DynamicDawgMutationSpec.v
run_required rocq-dawg-u64-spec 1G "$rocq_memory_max" "$rocq_root" \
  "$coqc_bin" -Q . ARTrie Spec/DynamicDawgU64Spec.v
run_required rocq-codec 1G "$rocq_memory_max" "$rocq_root" \
  "$coqc_bin" -Q . ARTrie Spec/VariableWidthCodecSpec.v
run_required rocq-interning 1G "$rocq_memory_max" "$rocq_root" \
  "$coqc_bin" -Q . ARTrie Spec/VariableWidthInterningSpec.v
run_required rocq-family-refinement 1G "$rocq_memory_max" "$rocq_root" \
  "$coqc_bin" -Q . ARTrie Spec/VariableWidthFamilyRefinementSpec.v
run_required rocq-kernel-check 1G "$rocq_memory_max" "$rocq_root" \
  "$coqchk_bin" -Q . ARTrie \
  ARTrie.Spec.VariableWidthCodecSpec \
  ARTrie.Spec.VariableWidthInterningSpec \
  ARTrie.Spec.VariableWidthFamilyRefinementSpec

echo '== TLA+ syntax and semantic analysis =='
for module in \
  VariableWidthCodecBoundary \
  VariableWidthVocabularyInterning \
  VariableWidthVocabularyPublication \
  VariableWidthFamilyRefinement
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
run_tlc_safe family-refinement-safe \
  VariableWidthFamilyRefinement VariableWidthFamilyRefinement

echo '== Deliberately unsafe controls =='
while IFS='|' read -r label module config invariant mutant_constant; do
  run_tlc_negative_control \
    "$label" "$module" "$config" "$invariant" "$mutant_constant"
done <<'NEGATIVE_CONTROLS'
codec-overlong|VariableWidthCodecBoundary|VariableWidthCodecBoundary_OverlongUnsafe|VWENC_26_OVERLONG_ULEB_IS_REJECTED|AcceptOverlongUleb
codec-unterminated|VariableWidthCodecBoundary|VariableWidthCodecBoundary_UnterminatedUnsafe|VWENC_27_UNTERMINATED_ULEB_IS_REJECTED|AcceptUnterminatedUleb
codec-utf8-continuation|VariableWidthCodecBoundary|VariableWidthCodecBoundary_Utf8ContinuationUnsafe|VWENC_28_UTF8_CONTINUATION_IS_REJECTED|AcceptUtf8Continuation
codec-physical-exposure|VariableWidthCodecBoundary|VariableWidthCodecBoundary_PhysicalExposureUnsafe|VWENC_24_CODEC_BYTES_NEVER_BECOME_LOGICAL_TRANSITIONS|ExposePhysicalCodecBytes
interning-fingerprint-only|VariableWidthVocabularyInterning|VariableWidthVocabularyInterning_FingerprintOnlyUnsafe|VWENC_142_FINGERPRINT_COLLISIONS_NEVER_ALIAS_DISTINCT_ATOMS|FingerprintOnlyEquality
interning-id-reuse|VariableWidthVocabularyInterning|VariableWidthVocabularyInterning_IdReuseUnsafe|VWENC_143_RETIRED_ID_IS_NEVER_CLAIMED_OR_LIVE_AGAIN|ReusePublishedId
publication-sequence-before-vocabulary|VariableWidthVocabularyPublication|VariableWidthVocabularyPublication_SequenceBeforeVocabularyUnsafe|VWENC_149_DURABLE_SEQUENCE_REFERENCES_DURABLE_BOUND_VOCABULARY|PublishSequenceBeforeVocabulary
publication-frontier-overclaim|VariableWidthVocabularyPublication|VariableWidthVocabularyPublication_FrontierOverclaimUnsafe|VWENC_147_PUBLISHED_FRONTIER_DOES_NOT_EXCEED_DURABLE_FRONTIER|OverclaimVocabularyFrontier
publication-cross-generation-resume|VariableWidthVocabularyPublication|VariableWidthVocabularyPublication_CrossGenerationResumeUnsafe|VWENC_154_CAPTURED_CONTINUATION_RESUMES_IMMUTABLE_PAIR|AllowCrossGenerationResume
publication-missing-as-empty|VariableWidthVocabularyPublication|VariableWidthVocabularyPublication_MissingVocabularyAsEmptyUnsafe|VWENC_178_RECOVERY_NEVER_SYNTHESIZES_EMPTY_SUCCESS|MissingVocabularyAsEmpty
family-physical-exposure|VariableWidthFamilyRefinement|VariableWidthFamilyRefinement_PhysicalExposureUnsafe|VWENC_241_CODEC_BYTES_NEVER_APPEAR_AS_LOGICAL_LABELS|ExposeCodecBytes
family-utf8-split|VariableWidthFamilyRefinement|VariableWidthFamilyRefinement_Utf8SplitUnsafe|VWENC_242_UTF8_SCALAR_IS_NEVER_SPLIT_ACROSS_LOGICAL_TRANSITIONS|SplitUtf8Scalar
family-interior-suffix|VariableWidthFamilyRefinement|VariableWidthFamilyRefinement_InteriorSuffixUnsafe|VWENC_243_SUFFIX_MATCHES_NEVER_BEGIN_INSIDE_A_LOGICAL_CODEWORD|AllowInteriorSuffixStart
family-specialized-divergence|VariableWidthFamilyRefinement|VariableWidthFamilyRefinement_SpecializedDivergenceUnsafe|VWENC_244_SPECIALIZED_KERNEL_PRESERVES_THE_COMPLETE_OBSERVATION|DivergeSpecializedKernel
family-type-name-format|VariableWidthFamilyRefinement|VariableWidthFamilyRefinement_TypeNameFormatUnsafe|VWENC_245_FORMAT_IDENTITY_COMES_ONLY_FROM_EXPLICIT_PROFILE_METADATA|InferFormatIdentityFromTypeName
family-fiber-mismatch|VariableWidthFamilyRefinement|VariableWidthFamilyRefinement_FiberMismatchUnsafe|VWENC_246_MISMATCHED_VOCABULARY_FIBER_IS_REJECTED_BEFORE_TRAVERSAL|AcceptMismatchedVocabularyFiber
NEGATIVE_CONTROLS

echo 'Variable-width formal gate passed.'
