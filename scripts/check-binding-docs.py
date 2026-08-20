#!/usr/bin/env python3
"""Validate that every declared libdictenstein facade has current documentation."""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MODEL = json.loads((ROOT / "bindings/api.json").read_text(encoding="utf-8"))
DOCS = MODEL["documentation"]
LINK_RE = re.compile(r"!?(?:\[[^\]]+\])\(([^)]+)\)")
PLACEHOLDER_RE = re.compile(r"\b(?:TODO|TBD|FIXME|STUB)\b", re.IGNORECASE)


def fail(message: str) -> None:
    print(f"binding-docs: {message}", file=sys.stderr)
    raise SystemExit(1)


def read(relative: str) -> tuple[Path, str]:
    path = ROOT / relative
    if not path.is_file():
        fail(f"missing documented file: {relative}")
    return path, path.read_text(encoding="utf-8")


def check_links(path: Path, text: str) -> None:
    for raw in LINK_RE.findall(text):
        target = raw.strip().split(maxsplit=1)[0].strip("<>")
        if not target or target.startswith(("#", "http://", "https://", "mailto:")):
            continue
        target = target.split("#", 1)[0]
        if not target:
            continue
        resolved = (path.parent / target).resolve()
        try:
            resolved.relative_to(ROOT)
        except ValueError:
            fail(f"{path.relative_to(ROOT)} links outside the repository: {raw}")
        if not resolved.exists():
            fail(f"{path.relative_to(ROOT)} has broken local link: {raw}")


def check_guide(entry: dict[str, object]) -> None:
    relative = str(entry["guide"])
    path, text = read(relative)
    if "BEGIN GENERATED BINDING OPERATIONS" not in text:
        fail(f"{relative} is not governed by the binding-guide generator")
    for heading in DOCS["requiredTopics"]:
        if f"## {heading}" not in text:
            fail(f"{relative} is missing required section {heading!r}")
    for language in entry["languages"]:
        if str(language).casefold() not in text.casefold():
            fail(f"{relative} does not identify represented language {language!r}")
    example = str(entry["example"])
    read(example)
    if example not in text:
        fail(f"{relative} does not link canonical example {example}")

    fences: list[str] = []
    in_fence = False
    for line in text.splitlines():
        if not line.startswith("```"):
            continue
        if in_fence:
            in_fence = False
            continue
        tag = line[3:].strip().split(maxsplit=1)[0] if line[3:].strip() else ""
        fences.append(tag)
        in_fence = True
    if in_fence:
        fail(f"{relative} has an unclosed fenced code block")
    if not fences or any(not tag for tag in fences):
        fail(f"{relative} must tag every fenced code block with a language")
    if not any(tag in {"sh", "bash", "shell", "console"} for tag in fences):
        fail(f"{relative} has no executable verification command")
    if PLACEHOLDER_RE.search(text):
        fail(f"{relative} contains a documentation placeholder")
    check_links(path, text)


def main() -> None:
    subprocess.run(
        [sys.executable, str(ROOT / DOCS["generator"]), "--check"],
        cwd=ROOT,
        check=True,
    )
    documented = set(DOCS["facades"])
    declared = set(MODEL["facades"]) | {"c"}
    if documented != declared:
        fail(
            "documentation facade set differs from declared facades: "
            f"missing={sorted(declared - documented)}, "
            f"extra={sorted(documented - declared)}"
        )
    for entry in DOCS["facades"].values():
        check_guide(entry)

    hub_path, hub = read(DOCS["hub"])
    architecture_path, architecture = read(DOCS["architecture"])
    check_links(hub_path, hub)
    check_links(architecture_path, architecture)
    for entry in DOCS["facades"].values():
        guide = str(entry["guide"])
        if guide not in hub and str(Path(guide).parent) not in hub:
            fail(f"binding hub does not route readers to {guide}")

    print(f"binding-docs: ok ({len(documented)} producer facades)")


if __name__ == "__main__":
    main()
