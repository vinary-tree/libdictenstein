"""Read back browsable generated JVM, .NET, and Swift API references."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
JVM_SOURCE = ROOT / "bindings/jvm/src/main/java/io/vinarytree/libdictenstein"
DOTNET_SOURCE = ROOT / "bindings/dotnet/src/VinaryTree.Libdictenstein"
SWIFT_SOURCE = ROOT / "bindings/swift/libdictenstein/Sources/Libdictenstein"
JAVA_TYPE = re.compile(
    r"^public\s+(?:(?:final|abstract|sealed)\s+)?(?:class|interface|record|enum)\s+(\w+)",
    re.MULTILINE,
)
CSHARP_TYPE = re.compile(
    r"^public\s+(?:(?:abstract|sealed|static|readonly|partial)\s+)*"
    r"(?:record\s+(?:class\s+|struct\s+)?|class\s+|struct\s+|enum\s+|interface\s+)(\w+)",
    re.MULTILINE,
)
SWIFT_TYPE = re.compile(
    r"^(?:public|open)\s+(?:(?:final|indirect)\s+)?"
    r"(?:class|struct|enum|protocol)\s+([A-Za-z]\w*)",
    re.MULTILINE,
)


def fail(message: str) -> None:
    raise SystemExit(f"managed-api-docs: {message}")


def source_types(source: Path, pattern: re.Pattern[str], extension: str) -> set[str]:
    names: set[str] = set()
    for path in sorted(source.glob(f"*.{extension}")):
        names.update(pattern.findall(path.read_text(encoding="utf-8")))
    if not names:
        fail(f"no public {extension} types found under {source}")
    return names


def browsable(site: Path) -> None:
    index = site / "index.html"
    if not index.is_file() or "<html" not in index.read_text(encoding="utf-8").lower():
        fail(f"generated site lacks a browsable index: {site}")


def check_jvm(site: Path) -> None:
    browsable(site)
    names = source_types(JVM_SOURCE, JAVA_TYPE, "java")
    package = site / "io/vinarytree/libdictenstein"
    missing = sorted(name for name in names if not (package / f"{name}.html").is_file())
    if missing:
        fail(f"Javadoc omits public JVM types: {missing}")
    print(f"managed-api-docs: Javadoc readback covers {len(names)} public JVM types")


def check_dotnet(site: Path) -> None:
    browsable(site)
    if "libdictenstein .NET API" not in (site / "index.html").read_text(
        encoding="utf-8"
    ):
        fail("DocFX index omits the package's .NET guide")
    names = source_types(DOTNET_SOURCE, CSHARP_TYPE, "cs")
    api = site / "api"
    missing = sorted(
        name
        for name in names
        if not (api / f"VinaryTree.Libdictenstein.{name}.html").is_file()
    )
    if missing:
        fail(f"DocFX omits public .NET types: {missing}")
    print(f"managed-api-docs: DocFX readback covers {len(names)} public .NET types")


def swift_assets(site: Path) -> set[Path]:
    browsable(site)
    body = (site / "index.html").read_text(encoding="utf-8")
    match = re.search(r'var baseUrl = "(/[^"]*/?)"', body)
    if match is None:
        fail("DocC index lacks a static-hosting base path")
    base = match.group(1)
    required: set[Path] = set()
    for value in re.findall(r'(?:src|href)="([^"]+)"', body):
        if not value.startswith(base):
            continue
        relative = Path(value.removeprefix(base).split("?", 1)[0])
        if relative.is_absolute() or ".." in relative.parts:
            fail(f"unsafe DocC asset reference: {value}")
        required.add(relative)
    if not any(path.suffix == ".js" for path in required) or not any(
        path.suffix == ".css" for path in required
    ):
        fail("DocC index lacks JavaScript or CSS asset references")
    return required


def swift_render_template() -> Path:
    target = json.loads(
        subprocess.check_output(["swiftc", "-print-target-info"], text=True)
    )
    resource = Path(target["paths"]["runtimeResourcePath"])
    compiler = Path(shutil.which("swiftc") or "swiftc").resolve()
    for candidate in (
        resource.parents[1] / "share/docc/render",
        compiler.parents[1] / "share/docc/render",
    ):
        if (candidate / "index.html").is_file():
            return candidate
    fail("active Swift toolchain has no DocC render template")


def complete_swift_assets(site: Path) -> None:
    required = swift_assets(site)
    if all((site / path).is_file() for path in required):
        return
    template = swift_render_template()
    if not any(
        path.suffix == ".css"
        and (site / path).is_file()
        and (template / path).is_file()
        and (site / path).read_bytes() == (template / path).read_bytes()
        for path in required
    ):
        fail("Swift toolchain DocC renderer does not match generated CSS")
    for source in sorted(template.rglob("*")):
        if source.is_symlink():
            fail(f"DocC renderer contains a symlink: {source}")
        if not source.is_file() or source.name in {"index.html", "index-template.html"}:
            continue
        target = site / source.relative_to(template)
        if not target.exists():
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)


def check_swift(site: Path, *, complete_assets: bool) -> None:
    if complete_assets:
        complete_swift_assets(site)
    required = swift_assets(site)
    missing_assets = sorted(
        str(path) for path in required if not (site / path).is_file()
    )
    if missing_assets:
        fail(f"DocC lacks browser assets: {missing_assets}")
    data = site / "data/documentation"
    module = data / "libdictenstein.json"
    if (
        not module.is_file()
        or "dictionary" not in module.read_text(encoding="utf-8").lower()
    ):
        fail("DocC lacks the Libdictenstein dictionary overview")
    documented: set[str] = set()
    for path in sorted(data.rglob("*.json")):
        page = json.loads(path.read_text(encoding="utf-8"))
        identifier = page.get("identifier", {})
        url = identifier.get("url") if isinstance(identifier, dict) else None
        if isinstance(url, str) and url:
            documented.add(url.rsplit("/", 1)[-1].casefold())
    names = source_types(SWIFT_SOURCE, SWIFT_TYPE, "swift")
    missing = sorted(name for name in names if name.casefold() not in documented)
    if missing:
        fail(f"DocC omits public Swift types: {missing}")
    print(f"managed-api-docs: DocC readback covers {len(names)} public Swift types")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("surface", choices=("jvm", "dotnet", "swift"))
    parser.add_argument("site", type=Path)
    parser.add_argument("--complete-toolchain-assets", action="store_true")
    args = parser.parse_args()
    if args.surface == "jvm":
        check_jvm(args.site)
    elif args.surface == "dotnet":
        check_dotnet(args.site)
    else:
        check_swift(args.site, complete_assets=args.complete_toolchain_assets)


if __name__ == "__main__":
    main()
