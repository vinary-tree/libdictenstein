"""Regression tests for the joined formal/test/control ledger."""

import json
import subprocess
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
EXTRACTOR = ROOT / "scripts" / "extract-variable-width-conformance-ledger.py"


class VariableWidthConformanceLedgerTest(unittest.TestCase):
    def extract(self) -> list[dict]:
        result = subprocess.run(
            [sys.executable, str(EXTRACTOR), "--root", str(ROOT)],
            check=True,
            capture_output=True,
            text=True,
        )
        return json.loads(result.stdout)

    def test_every_formal_declaration_has_one_joined_row(self) -> None:
        rows = self.extract()
        self.assertEqual(len(rows), 246)
        self.assertEqual(len({row["id"] for row in rows}), len(rows))
        self.assertTrue(all(row["formal_source"] and row["declaration"] for row in rows))
        self.assertTrue(
            all(
                row["formal_artifact"] == row["formal_source"].rsplit(":", 1)[0]
                for row in rows
            )
        )
        self.assertTrue(
            all(
                row["proof_kind"]
                == ("Rocq_proposition" if row["language"] == "rocq" else "TLA_assertion")
                for row in rows
            )
        )
        self.assertTrue(all(row["owner_repository"] == "libdictenstein" for row in rows))
        self.assertTrue(
            all(
                row["owner_layer"] == row["semantic_area"] and row["applicability"]
                for row in rows
            )
        )
        self.assertTrue(
            all(
                row["coverage"]
                in {"positive_and_negative", "positive_only", "negative_only", "uncovered"}
                for row in rows
            )
        )

    def test_joined_ledger_is_deterministic(self) -> None:
        self.assertEqual(self.extract(), self.extract())


if __name__ == "__main__":
    unittest.main()
