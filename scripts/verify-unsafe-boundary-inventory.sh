#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ledger="$repo_root/formal-verification/UNSAFE_INVENTORY.tsv"
contracts="$repo_root/formal-verification/UNSAFE_CONTRACTS.tsv"

if [ ! -f "$ledger" ]; then
  echo "Missing unsafe inventory ledger: $ledger" >&2
  exit 1
fi

if [ ! -f "$contracts" ]; then
  echo "Missing unsafe contract ledger: $contracts" >&2
  exit 1
fi

actual="$(mktemp)"
expected="$(mktemp)"
ledger_tags="$(mktemp)"
contract_tags="$(mktemp)"
missing_tags="$(mktemp)"
unused_tags="$(mktemp)"
trap 'rm -f "$actual" "$expected" "$ledger_tags" "$contract_tags" "$missing_tags" "$unused_tags"' EXIT

cd "$repo_root"

rg -n --no-heading \
  '(^[[:space:]]*unsafe[[:space:]]+impl\b|^[[:space:]]*(pub[[:space:]]+)?unsafe[[:space:]]+fn\b|\bunsafe[[:space:]]*\{)' \
  src -g '*.rs' \
  | awk -F: '
      function ltrim(s) { sub(/^[[:space:]]+/, "", s); return s }
      function rtrim(s) { sub(/[[:space:]]+$/, "", s); return s }
      function trim(s) { return rtrim(ltrim(s)) }
      {
        path = $1
        line = $0
        sub(/^[^:]+:[0-9]+:/, "", line)
        stripped = trim(line)
        if (stripped ~ /^\/\//) next

        if (stripped ~ /^unsafe[[:space:]]+impl/) {
          kind = "unsafe_impl"
        } else if (stripped ~ /^(pub[[:space:]]+)?unsafe[[:space:]]+fn/) {
          kind = "unsafe_fn"
        } else if (stripped ~ /unsafe[[:space:]]*\{/) {
          kind = "unsafe_block"
        } else {
          kind = "unsafe_other"
        }

        key = path "\t" kind "\t" stripped
        counts[key]++
      }
      END {
        for (key in counts) print key "\t" counts[key]
      }
    ' \
  | LC_ALL=C sort > "$actual"

awk -F '\t' '
  NF >= 4 && $1 !~ /^#/ && $1 != "" {
    print $1 "\t" $2 "\t" $3 "\t" $4
  }
' "$ledger" | LC_ALL=C sort > "$expected"

awk -F '\t' '
  $1 !~ /^#/ && $1 != "" {
    if (NF < 5 || $5 == "") {
      print "Missing contract tag for unsafe inventory row: " $0 > "/dev/stderr"
      exit 1
    }
    print $5
  }
' "$ledger" | LC_ALL=C sort -u > "$ledger_tags"

awk -F '\t' '
  $1 !~ /^#/ && $1 != "" {
    print $1
  }
' "$contracts" | LC_ALL=C sort -u > "$contract_tags"

comm -23 "$ledger_tags" "$contract_tags" > "$missing_tags"
comm -13 "$ledger_tags" "$contract_tags" > "$unused_tags"

if [ -s "$missing_tags" ]; then
  echo "Unsafe inventory references contract tags missing from UNSAFE_CONTRACTS.tsv:" >&2
  sed 's/^/  - /' "$missing_tags" >&2
  exit 1
fi

if [ -s "$unused_tags" ]; then
  echo "UNSAFE_CONTRACTS.tsv contains contract tags not used by UNSAFE_INVENTORY.tsv:" >&2
  sed 's/^/  - /' "$unused_tags" >&2
  exit 1
fi

if ! diff -u "$expected" "$actual"; then
  cat >&2 <<'MSG'

Unsafe inventory drift detected.

If this is an intentional unsafe-boundary change, audit the new or changed
contract and update formal-verification/UNSAFE_INVENTORY.tsv with the reviewed
path, kind, pattern, count, and contract tag. If the tag is new, also add its
reviewed contract to formal-verification/UNSAFE_CONTRACTS.tsv.
MSG
  exit 1
fi

echo "Unsafe boundary inventory and contract tags match formal-verification ledgers"
