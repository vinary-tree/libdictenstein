"""Regression tests for the source-derived VWENC inventory extractor."""

import json
import hashlib
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
EXTRACTOR = ROOT / "scripts" / "extract-variable-width-formal-inventory.py"


class FormalInventoryTest(unittest.TestCase):
    def extract(self) -> list[dict]:
        completed = subprocess.run(
            [sys.executable, str(EXTRACTOR), "--root", str(ROOT)],
            check=True,
            capture_output=True,
            text=True,
        )
        return json.loads(completed.stdout)

    def test_inventory_is_complete_unique_and_control_bound(self) -> None:
        rows = self.extract()
        self.assertEqual(len(rows), 246)
        self.assertEqual(len({row["id"] for row in rows}), 246)
        self.assertEqual(len({row["numeric_id"] for row in rows}), 246)
        self.assertEqual(sum(bool(row["negative_controls"]) for row in rows), 16)
        self.assertTrue(
            all(row["kind"] in {"Theorem", "Lemma", "Corollary", "TLA_assertion"} for row in rows)
        )
        self.assertTrue(
            all(row["semantic_area"] in {"codec", "interning", "family_refinement"} for row in rows)
        )
        self.assertTrue(all(re.fullmatch(r"[0-9a-f]{64}", row["source_sha256"]) for row in rows))
        self.assertTrue(all(Path(row["source_path"]).is_file() for row in rows))
        self.assertTrue(all(row["source_line"] > 0 for row in rows))
        self.assertTrue(all(row["id"] in row["declaration"] for row in rows))
        for source_path in {row["source_path"] for row in rows}:
            expected = hashlib.sha256((ROOT / source_path).read_bytes()).hexdigest()
            self.assertTrue(all(row["source_sha256"] == expected for row in rows if row["source_path"] == source_path))
        for row in rows:
            source_name = Path(row["source_path"]).name
            expected_area = (
                "codec" if "Codec" in source_name else
                "interning" if "Interning" in source_name else
                "family_refinement"
            )
            self.assertEqual(row["semantic_area"], expected_area)
            for control in row["negative_controls"]:
                self.assertTrue((ROOT / control).is_file(), control)

    def test_inventory_order_and_serialization_are_deterministic(self) -> None:
        first = self.extract()
        second = self.extract()
        self.assertEqual(first, second)
        self.assertEqual(
            [row["numeric_id"] for row in first],
            sorted(row["numeric_id"] for row in first),
        )

    def test_output_file_matches_stdout(self) -> None:
        stdout = subprocess.run(
            [sys.executable, str(EXTRACTOR), "--root", str(ROOT)],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "inventory.json"
            subprocess.run(
                [
                    sys.executable,
                    str(EXTRACTOR),
                    "--root",
                    str(ROOT),
                    "--output",
                    str(output),
                ],
                check=True,
            )
            self.assertEqual(output.read_text(encoding="utf-8"), stdout)


if __name__ == "__main__":
    unittest.main()
