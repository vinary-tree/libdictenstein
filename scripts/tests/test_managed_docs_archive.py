"""Deterministic and safe versioned DocFX archive checks."""

from __future__ import annotations

import gzip
import io
import tarfile
import tempfile
import unittest
import urllib.parse
from pathlib import Path
from unittest.mock import patch

from scripts import check_public_managed_docs as public
from scripts import managed_docs_archive as docs


class ManagedDocsArchiveTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        docs.TARGET.mkdir(exist_ok=True)

    def test_exact_filenames_and_reproducible_bytes(self) -> None:
        with tempfile.TemporaryDirectory(dir=docs.TARGET) as directory:
            root = Path(directory)
            site = root / "site"
            site.mkdir()
            (site / "index.html").write_text("<html>libdictenstein .NET API</html>")
            (site / "type:member.html").write_text("<html>member</html>")
            (site / "api").mkdir()
            (site / "api/Dictionary.html").write_text("<html>Dictionary</html>")
            artifact = root / "docs.tar.gz"
            docs.build(site, artifact)
            first = artifact.read_bytes()
            docs.build(site, artifact)
            self.assertEqual(first, artifact.read_bytes())
            readback = docs.verify(artifact, root / "readback", run_checker=False)
            self.assertEqual(
                (readback / "type:member.html").read_text(), "<html>member</html>"
            )
            expected = public.expected_files(readback)
            version, _ = docs.identity()
            base = f"https://vinary-tree.github.io/libdictenstein/{version}/dotnet/"

            def fake_fetch(url: str) -> bytes:
                self.assertTrue(url.startswith(base))
                return expected[urllib.parse.unquote(url.removeprefix(base))]

            with patch.object(public, "fetch", side_effect=fake_fetch):
                public.compare(readback, attempts=1, interval=0)

    def test_rejects_path_traversal(self) -> None:
        version, _ = docs.identity()
        with tempfile.TemporaryDirectory(dir=docs.TARGET) as directory:
            root = Path(directory)
            artifact = root / "unsafe.tar.gz"
            payload = io.BytesIO()
            with tarfile.open(fileobj=payload, mode="w") as tar:
                info, stream = docs.archive_member(
                    f"{version}/dotnet/../escape", b"bad"
                )
                tar.addfile(info, stream)
            artifact.write_bytes(gzip.compress(payload.getvalue(), mtime=0))
            with self.assertRaisesRegex(ValueError, "unsafe DocFX archive path"):
                docs.verify(artifact, root / "readback", run_checker=False)

    def test_outputs_must_be_under_target(self) -> None:
        with self.assertRaisesRegex(ValueError, "escapes repository target"):
            docs.under_target(docs.ROOT / "README.md")


if __name__ == "__main__":
    unittest.main()
