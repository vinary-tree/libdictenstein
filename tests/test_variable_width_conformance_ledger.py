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
                row["assumptions"]
                and row["trust_boundary"]
                and row["stack_safety"]
                and row["performance"]
                and row["acceptance_command"]
                and row["evidence_artifact"]
                and row["plain_language_law"]
                and row["current_target_public_surface"]
                and "differential_oracle_ids" in row
                and "required_mutant_control_ids" in row
                and row["status"]
                for row in rows
            )
        )
        registered = {
            test["registration"]
            for test in self._test_inventory()
        }
        joined = {
            test
            for row in rows
            for test in row["positive_tests"]
        }
        self.assertEqual(joined, registered)
        self.assertTrue(
            all(
                row["required_mutant_control_ids"] == row["negative_controls"]
                for row in rows
            )
        )
        self.assertTrue(
            all(
                (row["proof_only_exception"] is not None)
                == (row["coverage"] == "positive_only")
                for row in rows
            )
        )
        self.assertTrue(
            all(
                row["proof_only_exception"] == f"proof-only:{row['id']}"
                for row in rows
                if row["coverage"] == "positive_only"
            )
        )
        self.assertTrue(
            all(
                row["proof_only_rationale"]
                and row["formal_artifact"] in row["proof_only_rationale"]
                and all(
                    registration in row["proof_only_rationale"]
                    for registration in row["positive_tests"]
                )
                for row in rows
                if row["coverage"] == "positive_only"
            )
        )
        self.assertTrue(
            all(
                row["proof_only_rationale"] is None
                for row in rows
                if row["coverage"] != "positive_only"
            )
        )
        expected_status = {
            "positive_and_negative": "registered-with-negative-control",
            "positive_only": "registered-positive-only",
            "negative_only": "negative-control-only",
            "uncovered": "uncovered",
        }
        self.assertTrue(
            all(row["status"] == expected_status[row["coverage"]] for row in rows)
        )
        self.assertTrue(
            all((ROOT / row["acceptance_command"]).is_file() for row in rows)
        )
        self.assertTrue(
            all((ROOT / row["evidence_artifact"]).is_file() for row in rows)
        )
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

    def _test_inventory(self) -> list[dict]:
        extractor = ROOT / "scripts" / "extract-variable-width-test-inventory.py"
        result = subprocess.run(
            [sys.executable, str(extractor), "--root", str(ROOT)],
            check=True,
            capture_output=True,
            text=True,
        )
        return json.loads(result.stdout)


if __name__ == "__main__":
    unittest.main()
