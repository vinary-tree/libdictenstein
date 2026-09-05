#!/usr/bin/env bash
# Verify the machine-readable VWENC ledger against the TLA+ declarations.
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ledger="$root/formal-verification/variable-width-invariant-ledger.tsv"
models="$root/formal-verification/tla+"

[[ -r "$ledger" ]] || { echo "missing ledger: $ledger" >&2; exit 1; }

# The first three lines are comments; the fourth is the schema header.
header=$(sed -n '4p' "$ledger")
[[ "$header" == $'id\tsource\tdeclaration\tcategory\tlaw\towner\tprofiles\tsurface\tpositive_test\tdifferential_oracle\tnegative_control\tstatus' ]] \
  || { echo "ledger header does not match the required schema" >&2; exit 1; }

model_ids=$(rg --no-heading --no-filename '^VWENC_[A-Z0-9_]+' "$models" \
  | sed 's/[[:space:]].*//' | sort -u)
ledger_ids=$(cut -f1 "$ledger" | rg '^VWENC_' | sort -u)

[[ -n "$model_ids" ]] || { echo "no formal VWENC declarations found" >&2; exit 1; }
[[ -n "$ledger_ids" ]] || { echo "no ledger rows found" >&2; exit 1; }

if [[ "$model_ids" != "$ledger_ids" ]]; then
  echo "formal declarations and ledger IDs differ" >&2
  comm -23 <(printf '%s\n' "$model_ids") <(printf '%s\n' "$ledger_ids") >&2 || true
  comm -13 <(printf '%s\n' "$model_ids") <(printf '%s\n' "$ledger_ids") >&2 || true
  exit 1
fi

awk -F '\t' '
  NR <= 4 { next }
  NF != 12 { printf "row %d has %d fields (expected 12)\n", NR, NF; bad=1; next }
  {
    seen[$1]++
    for (i = 1; i <= NF; i++) {
      if ($i == "") {
        printf "row %d (%s) has an empty field\n", NR, $1; bad=1
      }
    }
    if ($9 == "-" || $10 == "-" || $11 == "-") {
      printf "row %d (%s) lacks registered positive/differential/negative evidence\n", NR, $1; bad=1
    }
    split($2, parts, ":");
    file=parts[1]; line=parts[2];
    command="sed -n " line "p " root "/" file;
    command | getline declaration;
    close(command);
    expected=$1 " ==";
    if (declaration !~ ("^" expected)) {
      printf "row %d (%s) does not anchor its declaration at %s\n", NR, $1, $2; bad=1
    }
  }
  END {
    for (id in seen) if (seen[id] != 1) {
      printf "duplicate ledger ID: %s\n", id; bad=1
    }
    if (NR <= 4) { print "ledger has no rows"; bad=1 }
    exit bad
  }
' root="$root" "$ledger"

echo "variable-width invariant ledger verified: $(printf '%s\n' "$ledger_ids" | wc -l) declarations"
