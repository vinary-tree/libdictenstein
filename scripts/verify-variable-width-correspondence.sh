#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

spec_path="formal-verification/rocq/Spec/VariableWidthInterningSpec.v"
manifest_path="formal-verification/variable-width-interning-correspondence.tsv"
expected_row_count=46

mapfile -t formal_points < <(
  awk '
    /^Inductive InterningFormalPoint : Type :=/ {
      in_points = 1
      next
    }
    in_points && /^Inductive ImplementationObligation : Type :=/ {
      in_points = 0
    }
    in_points && /^\| Point[A-Za-z0-9_]+/ {
      point = $0
      sub(/^\| /, "", point)
      sub(/[.;].*$/, "", point)
      print point
    }
  ' "$spec_path"
)

mapfile -t correspondence_rows < <(
  awk '
    /^Definition declared_correspondence_row$/ {
      in_rows = 1
      next
    }
    in_rows && /^Definition complete_interning_formal_points/ {
      in_rows = 0
    }
    in_rows && /^  \| Point[A-Za-z0-9_]+ =>$/ {
      point = $2
      path = ""
      symbol = ""
      next
    }
    in_rows && point != "" && index($0, "(\"") > 0 &&
        index($0, "\")%string") > 0 {
      start = index($0, "(\"") + 2
      finish = index($0, "\")%string")
      value = substr($0, start, finish - start)
      if (path == "") {
        path = value
      } else {
        symbol = value
      }
      next
    }
    in_rows && point != "" &&
        /^[[:space:]]+(Refines|CommonSubstrateOnly|Conflicts|Prospective) Obligation[A-Za-z0-9_]+$/ {
      relationship = $1
      obligation = $2
      print point "|" path "|" symbol "|" relationship "|" obligation
      point = ""
      path = ""
      symbol = ""
    }
  ' "$spec_path"
)

if [[ ! -f "$manifest_path" ]]; then
  echo "ERROR: frozen correspondence manifest is absent: $manifest_path" >&2
  exit 1
fi
mapfile -t manifest_rows < "$manifest_path"

if [[ "${#formal_points[@]}" -ne "$expected_row_count" ]]; then
  echo "ERROR: discovered ${#formal_points[@]} formal points; expected $expected_row_count" >&2
  exit 1
fi
if [[ "${#correspondence_rows[@]}" -ne "$expected_row_count" ]]; then
  echo "ERROR: extracted ${#correspondence_rows[@]} Rocq correspondence rows; expected $expected_row_count" >&2
  exit 1
fi
if [[ "${#manifest_rows[@]}" -ne "$expected_row_count" ]]; then
  echo "ERROR: frozen manifest contains ${#manifest_rows[@]} rows; expected $expected_row_count" >&2
  exit 1
fi

if ! diff -u "$manifest_path" <(printf '%s\n' "${correspondence_rows[@]}"); then
  echo 'ERROR: the Rocq correspondence relation differs from the independent frozen five-field manifest' >&2
  exit 1
fi

declare -A formal_point_set=()
declare -A manifest_point_set=()

for point in "${formal_points[@]}"; do
  if [[ -n "${formal_point_set[$point]:-}" ]]; then
    echo "ERROR: duplicate InterningFormalPoint constructor: $point" >&2
    exit 1
  fi
  formal_point_set[$point]=1
done

require_regex() {
  local point="$1"
  local source_path="$2"
  local rust_symbol="$3"
  local description="$4"
  local pattern="$5"
  if ! rg --multiline -q --regexp "$pattern" "$source_path"; then
    echo "ERROR: $point maps $rust_symbol to $source_path, but its exact $description was not found" >&2
    exit 1
  fi
}

require_vocab_impl() {
  local point="$1"
  local source_path="$2"
  local rust_symbol="$3"
  require_regex "$point" "$source_path" "$rust_symbol" \
    'PersistentVocabARTrie implementation owner' \
    '^impl<S: BlockStorage> super::dict_impl::PersistentVocabARTrie<S> \{$'
}

require_vocab_struct() {
  local point="$1"
  local source_path="$2"
  local rust_symbol="$3"
  require_regex "$point" "$source_path" "$rust_symbol" \
    'PersistentVocabARTrie type owner' \
    '^pub struct PersistentVocabARTrie<S: BlockStorage = MmapDiskManager> \{$'
}

validate_exact_rust_symbol() {
  local point="$1"
  local source_path="$2"
  local rust_symbol="$3"

  case "$source_path|$rust_symbol" in
    'src/persistent_artrie/vocab/mutation_api.rs|PersistentVocabARTrie::insert')
      require_vocab_impl "$point" "$source_path" "$rust_symbol"
      require_regex "$point" "$source_path" "$rust_symbol" 'method declaration' \
        '^[[:space:]]*pub fn insert\(&self, term: &str\) -> Result<u64> \{$'
      ;;
    'src/persistent_artrie/vocab/mutation_api.rs|PersistentVocabARTrie::insert_overlay')
      require_vocab_impl "$point" "$source_path" "$rust_symbol"
      require_regex "$point" "$source_path" "$rust_symbol" 'method declaration' \
        '^[[:space:]]*fn insert_overlay\(&self, term: &str\) -> Result<u64> \{$'
      case "$point" in
        PointClaimAllocation)
          require_regex "$point" "$source_path" "$rust_symbol" \
            'durable insert-once publication call' \
            '^[[:space:]]*>>::insert_cas_with_value_durable_default\(self, term\.as_bytes\(\), index\)\?;$'
          ;;
        PointOrphanAllocation)
          require_regex "$point" "$source_path" "$rust_symbol" \
            'monotone sparse-ID allocation claim' \
            '^[[:space:]]*let index = self\.next_index\.fetch_add\(1, Ordering::AcqRel\);$'
          require_regex "$point" "$source_path" "$rust_symbol" \
            'lost-race burned-ID return path' \
            '^[[:space:]]*Ok\(self\.get_index_lockfree\(term\)\.unwrap_or\(index\)\)$'
          ;;
        *)
          echo "ERROR: $point maps insert_overlay without a claim-or-orphan use-specific validator" >&2
          exit 1
          ;;
      esac
      ;;
    'src/persistent_artrie/vocab/mutation_api.rs|PersistentVocabARTrie::snapshot')
      require_vocab_impl "$point" "$source_path" "$rust_symbol"
      require_regex "$point" "$source_path" "$rust_symbol" 'method declaration' \
        '^[[:space:]]*pub fn snapshot\(&self\) -> Self \{$'
      ;;
    'src/persistent_artrie/vocab/query_api.rs|PersistentVocabARTrie::get_term')
      require_vocab_impl "$point" "$source_path" "$rust_symbol"
      require_regex "$point" "$source_path" "$rust_symbol" 'method declaration' \
        '^[[:space:]]*pub fn get_term\(&self, index: u64\) -> Option<String> \{$'
      ;;
    'src/persistent_artrie/vocab/query_api.rs|PersistentVocabARTrie::next_index')
      require_vocab_impl "$point" "$source_path" "$rust_symbol"
      require_regex "$point" "$source_path" "$rust_symbol" 'method declaration' \
        '^[[:space:]]*pub fn next_index\(&self\) -> u64 \{$'
      ;;
    'src/persistent_artrie/vocab/dict_impl.rs|PersistentVocabARTrie::reverse_term_map')
      require_vocab_struct "$point" "$source_path" "$rust_symbol"
      require_regex "$point" "$source_path" "$rust_symbol" 'field declaration' \
        '^[[:space:]]*pub\(super\) reverse_term_map: Option<DashMap<u64, String>>,$'
      ;;
    'src/persistent_artrie/vocab/dict_impl.rs|PersistentVocabARTrie::next_index')
      require_vocab_struct "$point" "$source_path" "$rust_symbol"
      require_regex "$point" "$source_path" "$rust_symbol" 'field declaration' \
        '^[[:space:]]*pub\(super\) next_index: AtomicU64,$'
      ;;
    'src/persistent_artrie/vocab/dict_impl.rs|PersistentVocabARTrie::commit_seq')
      require_vocab_struct "$point" "$source_path" "$rust_symbol"
      require_regex "$point" "$source_path" "$rust_symbol" 'field declaration' \
        '^[[:space:]]*pub\(crate\) commit_seq: AtomicU64,$'
      ;;
    'src/persistent_artrie/vocab/dict_impl.rs|PersistentVocabARTrie::committed_watermark')
      require_vocab_struct "$point" "$source_path" "$rust_symbol"
      require_regex "$point" "$source_path" "$rust_symbol" 'field declaration' \
        '^[[:space:]]*pub\(crate\) committed_watermark:\n[[:space:]]*crate::persistent_artrie::core::committed_watermark::CommittedWatermark,$'
      ;;
    'src/persistent_artrie/vocab/dict_impl.rs|PersistentVocabARTrie::checkpoint_lock')
      require_vocab_struct "$point" "$source_path" "$rust_symbol"
      require_regex "$point" "$source_path" "$rust_symbol" 'field declaration' \
        '^[[:space:]]*pub\(crate\) checkpoint_lock: Arc<parking_lot::Mutex<\(\)>>,$'
      ;;
    'src/persistent_artrie/vocab/types.rs|VocabTrieFileHeader')
      require_regex "$point" "$source_path" "$rust_symbol" 'type declaration' \
        '^pub struct VocabTrieFileHeader \{$'
      ;;
    'src/persistent_artrie/vocab/persistence_api.rs|PersistentVocabARTrie::checkpoint_overlay')
      require_vocab_impl "$point" "$source_path" "$rust_symbol"
      require_regex "$point" "$source_path" "$rust_symbol" 'method declaration' \
        '^[[:space:]]*fn checkpoint_overlay\(&self\) -> Result<\(\)> \{$'
      ;;
    'src/persistent_artrie/u64.rs|write_snapshot_file')
      require_regex "$point" "$source_path" "$rust_symbol" 'generic free-function declaration' \
        '^fn write_snapshot_file<V: DictionaryValue, const PREFIX: usize>\($'
      ;;
    'src/persistent_artrie/core/overlay/durable_write.rs|DurableOverlayWrite::insert_cas_with_value_durable_default')
      require_regex "$point" "$source_path" "$rust_symbol" 'DurableOverlayWrite trait owner' \
        '^pub\(crate\) trait DurableOverlayWrite<K: KeyEncoding, V: DictionaryValue, S>:$'
      require_regex "$point" "$source_path" "$rust_symbol" 'default-method declaration' \
        '^[[:space:]]*fn insert_cas_with_value_durable_default\(&self, key_bytes: &\[u8\], value: V\) -> Result<bool> \{$'
      ;;
    *)
      echo "ERROR: no exact declaration validator exists for implemented mapping $point: $source_path | $rust_symbol" >&2
      exit 1
      ;;
  esac
}

for row in "${manifest_rows[@]}"; do
  IFS='|' read -r point source_path rust_symbol relationship obligation extra <<<"$row"

  if [[ -n "${extra:-}" || -z "$point" || -z "$source_path" ||
        -z "$rust_symbol" || -z "$relationship" || -z "$obligation" ]]; then
    echo "ERROR: manifest row is not exactly five nonempty fields: $row" >&2
    exit 1
  fi
  if [[ -z "${formal_point_set[$point]:-}" ]]; then
    echo "ERROR: frozen manifest names undeclared formal point: $point" >&2
    exit 1
  fi
  if [[ -n "${manifest_point_set[$point]:-}" ]]; then
    echo "ERROR: duplicate frozen correspondence row for: $point" >&2
    exit 1
  fi
  manifest_point_set[$point]=1

  case "$relationship" in
    Prospective)
      if [[ -e "$source_path" ]]; then
        echo "ERROR: $point remains Prospective although $source_path now exists" >&2
        echo 'Update the Rocq relation, frozen manifest, and exact declaration validator together.' >&2
        exit 1
      fi
      ;;
    Refines|CommonSubstrateOnly|Conflicts)
      if [[ ! -f "$source_path" ]]; then
        echo "ERROR: $point declares $relationship but source file is absent: $source_path" >&2
        exit 1
      fi
      validate_exact_rust_symbol "$point" "$source_path" "$rust_symbol"
      ;;
    *)
      echo "ERROR: $point has unknown correspondence relationship: $relationship" >&2
      exit 1
      ;;
  esac
done

for point in "${formal_points[@]}"; do
  if [[ -z "${manifest_point_set[$point]:-}" ]]; then
    echo "ERROR: formal point lacks an independent frozen correspondence row: $point" >&2
    exit 1
  fi
done

printf 'Validated %d exact variable-width correspondence rows against the independent manifest and Rust declarations.\n' \
  "${#manifest_rows[@]}"
