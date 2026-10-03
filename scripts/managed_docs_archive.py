"""Stage and read back the exact versioned DocFX site before immutable release."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[1]
TARGET = ROOT / "target"
SITE = TARGET / "managed-api-docs/dotnet"
ARTIFACTS = TARGET / "managed-api-docs-artifacts"
READBACK = TARGET / "managed-api-docs-readback"
RELEASE = ROOT / "release/version.json"


def identity() -> tuple[str, str]:
    model = json.loads(RELEASE.read_text(encoding="utf-8"))
    return model["canonical"], model["publication"]["sourceTag"]


def archive_name(version: str) -> str:
    return f"libdictenstein-dotnet-documentation-{version}.tar.gz"


def under_target(path: Path) -> Path:
    path = path.resolve()
    if path == TARGET.resolve() or not path.is_relative_to(TARGET.resolve()):
        raise ValueError(f"managed docs path escapes repository target: {path}")
    return path


def archive_member(name: str, content: bytes) -> tuple[tarfile.TarInfo, io.BytesIO]:
    info = tarfile.TarInfo(name)
    info.size = len(content)
    info.uid = info.gid = info.mtime = 0
    info.uname = info.gname = ""
    info.mode = 0o644
    return info, io.BytesIO(content)


def build(site: Path = SITE, artifact: Path | None = None) -> Path:
    version, source_ref = identity()
    site = under_target(site)
    artifact = under_target(artifact or ARTIFACTS / archive_name(version))
    if not (site / "index.html").is_file():
        raise ValueError("DocFX site lacks index.html")
    files: dict[str, bytes] = {}
    for path in sorted(site.rglob("*")):
        if path.is_symlink() or not (path.is_file() or path.is_dir()):
            raise ValueError(f"unsupported DocFX site entry: {path}")
        if path.is_file():
            files[path.relative_to(site).as_posix()] = path.read_bytes()
    manifest = {
        "schemaVersion": 1,
        "component": "libdictenstein",
        "version": version,
        "sourceRef": source_ref,
        "sha256": {
            name: hashlib.sha256(content).hexdigest() for name, content in files.items()
        },
    }
    metadata = (json.dumps(manifest, sort_keys=True, indent=2) + "\n").encode()
    artifact.parent.mkdir(parents=True, exist_ok=True)
    with (
        artifact.open("wb") as output,
        gzip.GzipFile(fileobj=output, mode="wb", mtime=0, filename="") as compressed,
        tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as tar,
    ):
        info, stream = archive_member(
            f"{version}/dotnet/documentation-manifest.json", metadata
        )
        tar.addfile(info, stream)
        for name, content in files.items():
            info, stream = archive_member(f"{version}/dotnet/{name}", content)
            tar.addfile(info, stream)
    verify(artifact, run_checker=False)
    print(f"managed-docs-archive: packed {len(files)} DocFX files: {artifact}")
    return artifact


def verify(
    artifact: Path | None = None,
    destination: Path = READBACK,
    *,
    run_checker: bool = True,
) -> Path:
    version, source_ref = identity()
    artifact = under_target(artifact or ARTIFACTS / archive_name(version))
    destination = under_target(destination)
    if not artifact.is_file():
        raise ValueError(f"DocFX archive is missing: {artifact}")
    if destination.exists():
        shutil.rmtree(destination)
    site = destination / version / "dotnet"
    site.mkdir(parents=True)
    seen: dict[str, str] = {}
    manifest: dict | None = None
    with tarfile.open(artifact, mode="r:gz") as tar:
        for item in tar:
            if not item.isfile() or item.issym() or item.islnk():
                raise ValueError(f"unsafe DocFX archive member: {item.name}")
            stream = tar.extractfile(item)
            if stream is None:
                raise ValueError(f"unreadable DocFX archive member: {item.name}")
            content = stream.read()
            path = PurePosixPath(item.name)
            if (
                path.is_absolute()
                or len(path.parts) < 3
                or path.parts[:2] != (version, "dotnet")
                or ".." in path.parts
            ):
                raise ValueError(f"unsafe DocFX archive path: {item.name}")
            relative = PurePosixPath(*path.parts[2:]).as_posix()
            if relative == "documentation-manifest.json":
                if manifest is not None:
                    raise ValueError("duplicate DocFX archive manifest")
                manifest = json.loads(content)
                continue
            if relative in seen:
                raise ValueError(f"duplicate DocFX archive file: {relative}")
            seen[relative] = hashlib.sha256(content).hexdigest()
            output = site / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_bytes(content)
    if not isinstance(manifest, dict) or (
        manifest.get("schemaVersion"),
        manifest.get("component"),
        manifest.get("version"),
        manifest.get("sourceRef"),
    ) != (1, "libdictenstein", version, source_ref):
        raise ValueError("DocFX archive manifest has wrong release identity")
    if manifest.get("sha256") != seen:
        raise ValueError("DocFX archive manifest does not match readback files")
    (site / "documentation-manifest.json").write_text(
        json.dumps(manifest, sort_keys=True, indent=2) + "\n", encoding="utf-8"
    )
    if not (site / "index.html").is_file():
        raise ValueError("DocFX archive lacks index.html")
    if run_checker:
        subprocess.run(
            [
                sys.executable,
                str(ROOT / "scripts/check-managed-api-docs.py"),
                "dotnet",
                str(site),
            ],
            cwd=ROOT,
            check=True,
        )
    print(f"managed-docs-archive: read back {len(seen)} exact DocFX files")
    return site


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("build", "verify"))
    parser.add_argument(
        "--archive", type=Path, help="archive to verify after public download"
    )
    args = parser.parse_args()
    if args.action == "build":
        if args.archive is not None:
            parser.error("--archive applies only to verify")
        build()
    else:
        verify(args.archive)


if __name__ == "__main__":
    main()
