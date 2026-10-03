"""Build and validate the deterministic Lua API archive before an immutable release."""

from __future__ import annotations

import gzip
import hashlib
import io
import json
import tarfile
from pathlib import Path

try:
    from scripts import lua_api_reference
except ModuleNotFoundError as error:
    if error.name != "scripts":
        raise
    import lua_api_reference


ROOT = Path(__file__).resolve().parents[1]
RELEASE = ROOT / "release/version.json"
STAGE = ROOT / "target/lua-docs-stage"
ARTIFACTS = ROOT / "target/lua-docs-artifacts"


def release_identity() -> tuple[str, str]:
    model = json.loads(RELEASE.read_text(encoding="utf-8"))
    return model["canonical"], model["publication"]["sourceTag"]


def archive_name(version: str) -> str:
    return f"libdictenstein-lua-documentation-{version}.tar.gz"


def build() -> Path:
    version, source_ref = release_identity()
    specification = lua_api_reference.load_and_validate()
    source = lua_api_reference.render(specification, version, source_ref).encode()
    page = STAGE / version / "lua/index.html"
    page.parent.mkdir(parents=True, exist_ok=True)
    page.write_bytes(source)
    manifest = {
        "schemaVersion": 1,
        "component": "libdictenstein",
        "version": version,
        "sourceRef": source_ref,
        "sha256": {"lua/index.html": hashlib.sha256(source).hexdigest()},
    }
    manifest_path = STAGE / version / "lua/documentation-manifest.json"
    manifest_path.write_text(
        json.dumps(manifest, sort_keys=True, indent=2) + "\n", encoding="utf-8"
    )
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.PAX_FORMAT) as tar:
        for path in (page, manifest_path):
            relative = path.relative_to(STAGE).as_posix()
            contents = path.read_bytes()
            info = tarfile.TarInfo(relative)
            info.size = len(contents)
            info.uid = info.gid = info.mtime = 0
            info.uname = info.gname = ""
            info.mode = 0o644
            tar.addfile(info, io.BytesIO(contents))
    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    artifact = ARTIFACTS / archive_name(version)
    artifact.write_bytes(gzip.compress(buffer.getvalue(), compresslevel=9, mtime=0))
    verify(artifact)
    print(f"Lua documentation archive: {artifact.relative_to(ROOT)}")
    return artifact


def verify(artifact: Path) -> None:
    version, source_ref = release_identity()
    expected = {
        f"{version}/lua/index.html",
        f"{version}/lua/documentation-manifest.json",
    }
    with tarfile.open(artifact, mode="r:gz") as tar:
        members = tar.getmembers()
        names = [member.name for member in members]
        if len(names) != len(expected) or set(names) != expected:
            raise ValueError(
                f"Lua archive has unexpected or duplicate members: {names}"
            )
        if any(
            not member.isfile() or member.issym() or member.islnk()
            for member in members
        ):
            raise ValueError("Lua archive contains a non-regular member")
        payload = {}
        for member in members:
            stream = tar.extractfile(member)
            assert stream is not None
            payload[member.name] = stream.read()
    manifest = json.loads(payload[f"{version}/lua/documentation-manifest.json"])
    if (
        manifest.get("schemaVersion"),
        manifest.get("component"),
        manifest.get("version"),
        manifest.get("sourceRef"),
    ) != (1, "libdictenstein", version, source_ref):
        raise ValueError("Lua archive manifest identity differs from release")
    if manifest.get("sha256") != {
        "lua/index.html": hashlib.sha256(
            payload[f"{version}/lua/index.html"]
        ).hexdigest()
    }:
        raise ValueError("Lua archive manifest hash differs from page bytes")


if __name__ == "__main__":
    build()
