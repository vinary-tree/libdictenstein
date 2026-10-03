"""Negative controls for the live Go publication dependency gate."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "sync_release_version", ROOT / "scripts/sync-release-version.py"
)
assert SPEC is not None and SPEC.loader is not None
SYNC = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SYNC)


class GoPublicationWaitGateTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.model = json.loads(
            (ROOT / "release/version.json").read_text(encoding="utf-8")
        )
        cls.versions = SYNC.derived(str(cls.model["canonical"]))
        cls.go_mod = (ROOT / "bindings/go/go.mod").read_text(encoding="utf-8")
        cls.workflow = (ROOT / ".github/workflows/release-bindings.yml").read_text(
            encoding="utf-8"
        )

    def failures(
        self,
        *,
        model: dict | None = None,
        go_mod: str | None = None,
        workflow: str | None = None,
    ) -> list[str]:
        return SYNC.go_publication_wait_gate_failures(
            self.model if model is None else model,
            self.versions,
            self.go_mod if go_mod is None else go_mod,
            self.workflow if workflow is None else workflow,
        )

    def test_live_dynamic_wait_gate_matches_both_go_dependencies(self) -> None:
        self.assertEqual(self.failures(), [])

    def test_changed_workflow_module_path_is_rejected(self) -> None:
        before = (
            "            github.com/vinary-tree/vinary-tree-interop/bindings/go/v4 \\\n"
        )
        after = before.replace("/v4", "/v5")
        self.assertIn(before, self.workflow)
        self.assertTrue(self.failures(workflow=self.workflow.replace(before, after, 1)))

    def test_changed_workflow_version_predicate_is_rejected(self) -> None:
        before = ".Version == $version"
        self.assertIn(before, self.workflow)
        self.assertTrue(
            self.failures(
                workflow=self.workflow.replace(before, ".Version != $version", 1)
            )
        )

    def test_changed_workflow_version_source_is_rejected(self) -> None:
        before = "version=$(jq -er '.registries.goTag' release/version.json)"
        self.assertIn(before, self.workflow)
        self.assertTrue(
            self.failures(
                workflow=self.workflow.replace(
                    before, before.replace("goTag", "npm"), 1
                )
            )
        )

    def test_changed_go_mod_requirement_is_rejected(self) -> None:
        before = (
            "github.com/vinary-tree/vinary-tree-interop/bindings/go/v4 "
            + self.versions["goTag"]
        )
        self.assertIn(before, self.go_mod)
        self.assertTrue(
            self.failures(go_mod=self.go_mod.replace(before, before + "-wrong", 1))
        )

    def test_changed_release_dependency_version_is_rejected(self) -> None:
        model = {
            **self.model,
            "dependencies": {
                **self.model["dependencies"],
                "vinary-tree-interop": "4.0.0-rc.5",
            },
        }
        self.assertTrue(self.failures(model=model))

    def test_json_writer_preserves_existing_format_without_semantic_change(
        self,
    ) -> None:
        scratch = ROOT / "target"
        scratch.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="release-json-", dir=scratch
        ) as directory:
            path = Path(directory) / "format.json"
            original = '{"canonical":"4.0.0-rc.6",  "goTag":"v4.0.0-rc.6"}\n'
            path.write_text(original, encoding="utf-8")
            with patch.object(SYNC, "ROOT", Path(directory)):
                SYNC.update_json(
                    "format.json", lambda value: value.update({"goTag": "v4.0.0-rc.6"})
                )
            self.assertEqual(path.read_text(encoding="utf-8"), original)


class ReleaseGuideIdentityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.model = json.loads(
            (ROOT / "release/version.json").read_text(encoding="utf-8")
        )
        cls.versions = SYNC.derived(
            str(cls.model["canonical"]),
            cls.model["publication"]["luaRocksRevision"],
        )
        cls.guide = (ROOT / "docs/releasing.md").read_text(encoding="utf-8")

    def failures(self, guide: str) -> list[str]:
        return SYNC.release_guide_identity_failures(
            self.model, self.versions, guide
        )

    def test_current_corrective_source_and_go_tags_are_distinct(self) -> None:
        self.assertEqual(self.failures(self.guide), [])

    def test_stale_source_tag_is_rejected_even_if_current_tag_appears_later(self) -> None:
        source_tag = self.model["publication"]["sourceTag"]
        changed = self.guide.replace(
            f"source tag is\n`{source_tag}`",
            "source tag is\n`v4.0.0-rc.6-release.0`",
            1,
        )
        self.assertNotEqual(changed, self.guide)
        self.assertIn("release operator guide uses a stale source tag", self.failures(changed))

    def test_stale_go_module_tag_is_rejected(self) -> None:
        changed = self.guide.replace(
            f"Go module tag is\n`bindings/go/{self.versions['goTag']}`",
            "Go module tag is\n`bindings/go/v4.0.0-rc.5`",
            1,
        )
        self.assertNotEqual(changed, self.guide)
        self.assertIn("release operator guide uses a stale Go module tag", self.failures(changed))

    def test_missing_live_identity_section_is_rejected(self) -> None:
        changed = self.guide.replace("## Identity and prerequisites", "## Prerequisites", 1)
        self.assertIn(
            "release operator guide has no unique live identity section",
            self.failures(changed),
        )


if __name__ == "__main__":
    unittest.main()
