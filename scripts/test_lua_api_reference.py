"""Offline checks for Lua facade documentation and immutable archive bytes."""

from __future__ import annotations

import hashlib
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import lua_api_reference as reference
from scripts import lua_docs_archive as archive


class LuaReferenceTests(unittest.TestCase):
    def test_complete_source_registration(self) -> None:
        model = reference.load_and_validate()
        exported = {
            entry["name"] for group in model["groups"] for entry in group["entries"]
        }
        self.assertIn("dynamic_dawg", exported)
        self.assertIn("entries_iter", exported)
        self.assertIn("symmetric_difference", exported)

    def test_missing_method_is_rejected(self) -> None:
        model = reference.load_and_validate()
        model["groups"][0]["entries"] = model["groups"][0]["entries"][1:]
        archive.ROOT.joinpath("target").mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=archive.ROOT / "target") as temporary:
            incomplete = Path(temporary) / "incomplete.json"
            incomplete.write_text(json.dumps(model), encoding="utf-8")
            with (
                mock.patch.object(reference, "SPEC", incomplete),
                self.assertRaisesRegex(ValueError, "functions differs"),
            ):
                reference.load_and_validate()

    def test_guide_resource_contract_matches_method_table(self) -> None:
        model = reference.load_and_validate()
        methods = next(
            group["entries"]
            for group in model["groups"]
            if group["sourceArray"] == "dictionary_methods"
        )
        method_names = {entry["name"] for entry in methods}
        guide = (archive.ROOT / "bindings/lua/README.md").read_text(encoding="utf-8")
        if "resource" in method_names:
            self.assertIn("`resource()` lends", guide)
        else:
            self.assertIn(
                "dictionary userdata is the `vinary-tree.dictionary.v1` resource", guide
            )
            self.assertIn("There is no `resource()` method.", guide)
            self.assertNotIn("`resource()` lends", guide)

    def test_archive_is_byte_reproducible_and_verified(self) -> None:
        archive.ROOT.joinpath("target").mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=archive.ROOT / "target") as temporary:
            root = Path(temporary)
            with (
                mock.patch.object(archive, "STAGE", root / "stage"),
                mock.patch.object(archive, "ARTIFACTS", root / "artifacts"),
            ):
                first = archive.build().read_bytes()
                second = archive.build().read_bytes()
                self.assertEqual(
                    hashlib.sha256(first).digest(), hashlib.sha256(second).digest()
                )


if __name__ == "__main__":
    unittest.main()
