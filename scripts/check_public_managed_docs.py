"""Compare every served versioned .NET API file with an immutable DocFX archive."""

from __future__ import annotations

import argparse
import hashlib
import json
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

try:
    from scripts import managed_docs_archive as docs
except ModuleNotFoundError as error:
    if error.name != "scripts":
        raise
    import managed_docs_archive as docs


def expected_files(site: Path) -> dict[str, bytes]:
    version, source_ref = docs.identity()
    site = docs.under_target(site)
    manifest_path = site / "documentation-manifest.json"
    manifest_bytes = manifest_path.read_bytes()
    manifest = json.loads(manifest_bytes)
    if (
        manifest.get("schemaVersion"),
        manifest.get("component"),
        manifest.get("version"),
        manifest.get("sourceRef"),
    ) != (1, "libdictenstein", version, source_ref):
        raise ValueError("public readback source has wrong release identity")
    digests = manifest.get("sha256")
    if not isinstance(digests, dict) or not digests:
        raise ValueError("public readback source has no file inventory")
    files = {"documentation-manifest.json": manifest_bytes}
    for name, digest in digests.items():
        path = Path(name)
        if path.is_absolute() or not path.parts or ".." in path.parts:
            raise ValueError(f"unsafe documentation manifest path: {name}")
        content = (site / path).read_bytes()
        if hashlib.sha256(content).hexdigest() != digest:
            raise ValueError(f"local archive digest mismatch: {name}")
        files[name] = content
    if "index.html" not in files or not any(name.startswith("api/") for name in files):
        raise ValueError("public readback source lacks index or API pages")
    return files


def fetch(url: str, maximum: int = 16 * 1024 * 1024) -> bytes:
    request = urllib.request.Request(
        url, headers={"User-Agent": "vinary-tree-doc-readback/1"}
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        body = response.read(maximum + 1)
    if len(body) > maximum:
        raise ValueError(f"public documentation file exceeds 16 MiB: {url}")
    return body


def compare(site: Path, *, attempts: int = 36, interval: int = 10) -> None:
    version, _ = docs.identity()
    files = expected_files(site)
    base = f"https://vinary-tree.github.io/libdictenstein/{version}/dotnet/"
    for attempt in range(1, attempts + 1):
        try:
            for name, expected in sorted(files.items()):
                actual = fetch(base + urllib.parse.quote(name, safe="/"))
                if actual != expected:
                    raise ValueError(f"public Pages bytes differ: {name}")
        except (urllib.error.URLError, ValueError) as error:
            if attempt == attempts:
                raise ValueError(
                    f"public .NET API readback failed after {attempts} attempts: {error}"
                ) from error
            time.sleep(interval)
        else:
            print(f"public managed docs: {len(files)} exact files verified at {base}")
            return


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("site", type=Path, help="verified local DocFX archive readback")
    args = parser.parse_args()
    compare(args.site)


if __name__ == "__main__":
    main()
