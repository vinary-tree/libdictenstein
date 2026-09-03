"""Regression tests for executable VWENC test registration inventory."""

import json
import subprocess
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
EXTRACTOR = ROOT / "scripts" / "extract-variable-width-test-inventory.py"


class VariableWidthTestInventoryTest(unittest.TestCase):
    def extract(self) -> list[dict]:
        completed = subprocess.run(
            [sys.executable, str(EXTRACTOR), "--root", str(ROOT)],
            check=True,
            capture_output=True,
            text=True,
        )
        return json.loads(completed.stdout)

    def test_registrations_are_unique_located_and_formal(self) -> None:
        rows = self.extract()
        self.assertGreaterEqual(len(rows), 13)
        self.assertEqual(len({row["registration"] for row in rows}), len(rows))
        for row in rows:
            source = ROOT / row["source_path"]
            self.assertTrue(source.is_file())
            self.assertGreater(row["source_line"], 0)
            self.assertIn(row["source_line"], range(1, len(source.read_text().splitlines()) + 1))
            self.assertTrue(row["numeric_ids"])

    def test_registration_inventory_is_deterministic(self) -> None:
        self.assertEqual(self.extract(), self.extract())


if __name__ == "__main__":
    unittest.main()
