"""Regression checks for source-derived managed API documentation gates."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "check_managed_api_docs", ROOT / "scripts/check-managed-api-docs.py"
)
assert SPEC is not None and SPEC.loader is not None
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


class ManagedApiDocsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        (ROOT / "target").mkdir(exist_ok=True)

    def test_inventory_comes_from_current_public_facades(self) -> None:
        self.assertIn(
            "DynamicDawg", CHECK.source_types(CHECK.JVM_SOURCE, CHECK.JAVA_TYPE, "java")
        )
        self.assertIn(
            "Dictionary",
            CHECK.source_types(CHECK.DOTNET_SOURCE, CHECK.CSHARP_TYPE, "cs"),
        )
        self.assertIn(
            "Dictionary",
            CHECK.source_types(CHECK.SWIFT_SOURCE, CHECK.SWIFT_TYPE, "swift"),
        )
        self.assertIn(
            "EntryStream",
            CHECK.source_types(CHECK.SWIFT_SOURCE, CHECK.SWIFT_TYPE, "swift"),
        )

    def test_javadoc_cannot_omit_a_public_type(self) -> None:
        with tempfile.TemporaryDirectory(dir=ROOT / "target") as directory:
            site = Path(directory)
            (site / "index.html").write_text("<html></html>", encoding="utf-8")
            with self.assertRaisesRegex(SystemExit, "Javadoc omits public JVM types"):
                CHECK.check_jvm(site)

    def test_docfx_cannot_omit_a_public_type(self) -> None:
        with tempfile.TemporaryDirectory(dir=ROOT / "target") as directory:
            site = Path(directory)
            (site / "index.html").write_text(
                "<html>libdictenstein .NET API</html>", encoding="utf-8"
            )
            with self.assertRaisesRegex(SystemExit, "DocFX omits public .NET types"):
                CHECK.check_dotnet(site)

    def test_swift_rejects_unsafe_asset_reference(self) -> None:
        with tempfile.TemporaryDirectory(dir=ROOT / "target") as directory:
            site = Path(directory)
            (site / "index.html").write_text(
                '<html><script>var baseUrl = "/libdictenstein/"</script>'
                '<script src="/libdictenstein/../foreign.js"></script></html>',
                encoding="utf-8",
            )
            with self.assertRaisesRegex(SystemExit, "unsafe DocC asset reference"):
                CHECK.swift_assets(site)


if __name__ == "__main__":
    unittest.main()
