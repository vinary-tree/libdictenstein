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
contract_statuses="$(mktemp)"
persistence_tags="$(mktemp)"
missing_tags="$(mktemp)"
unused_tags="$(mktemp)"
trap 'rm -f "$actual" "$expected" "$ledger_tags" "$contract_tags" "$contract_statuses" "$persistence_tags" "$missing_tags" "$unused_tags"' EXIT

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

awk -F '\t' '
  BEGIN {
    valid_coverages = " rocq tla loom miri correspondence compile-time unit trusted-boundary "
    valid_statuses = " covered miri-wired trusted-boundary "
  }
  $1 !~ /^#/ && $1 != "" {
    if (seen[$1]++) {
      print "Duplicate unsafe contract tag in UNSAFE_CONTRACTS.tsv: " $1 > "/dev/stderr"
      exit 1
    }
    if (NF < 6 || $4 == "" || $5 == "" || $6 == "") {
      print "Malformed unsafe contract row; expected contract, scope, obligation, coverage, status, evidence: " $0 > "/dev/stderr"
      exit 1
    }

    has_miri = 0
    has_trusted = 0
    n = split($4, coverages, ",")
    for (i = 1; i <= n; i++) {
      coverage = coverages[i]
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", coverage)
      if (coverage == "" || index(valid_coverages, " " coverage " ") == 0) {
        print "Invalid unsafe contract coverage [" coverage "] for tag " $1 > "/dev/stderr"
        exit 1
      }
      if (coverage == "miri") has_miri = 1
      if (coverage == "trusted-boundary") has_trusted = 1
    }

    if (index(valid_statuses, " " $5 " ") == 0) {
      print "Invalid unsafe contract status [" $5 "] for tag " $1 > "/dev/stderr"
      exit 1
    }
    if ($5 == "miri-wired" && !has_miri) {
      print "Miri-wired unsafe contract lacks miri coverage for tag " $1 > "/dev/stderr"
      exit 1
    }
    if ($5 == "trusted-boundary" && !has_trusted) {
      print "Trusted unsafe contract lacks trusted-boundary coverage for tag " $1 > "/dev/stderr"
      exit 1
    }
    if (has_trusted && $5 != "trusted-boundary") {
      print "Unsafe contract marks trusted-boundary coverage without trusted-boundary status for tag " $1 > "/dev/stderr"
      exit 1
    }

    print $1 "\t" $5 "\t" $4
  }
' "$contracts" | LC_ALL=C sort > "$contract_statuses"

awk -F '\t' '
  $1 !~ /^#/ && $1 ~ /^src\/persistent_/ {
    print $5
  }
' "$ledger" | LC_ALL=C sort -u > "$persistence_tags"

awk -F '\t' '
  NR == FNR {
    persistence[$1] = 1
    next
  }
  $1 in persistence && $2 !~ /^(covered|miri-wired)$/ {
    print "Persistence unsafe contract lacks covered or miri-wired status: " $1 " (" $2 ")" > "/dev/stderr"
    exit 1
  }
' "$persistence_tags" "$contract_statuses"

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
reviewed contract, coverage class, status, and evidence to
formal-verification/UNSAFE_CONTRACTS.tsv.
MSG
  exit 1
fi

echo "Unsafe boundary inventory, contract tags, and coverage metadata match formal-verification ledgers"
