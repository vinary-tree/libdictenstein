#!/usr/bin/env python3
"""Write or validate every libdictenstein release-train coordinate."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODEL_PATH = ROOT / "release/version.json"
GENERATED_TREE_PARTS = frozenset(
    {".git", ".venv", "_build", "build", "dist", "node_modules", "target", "venv"}
)


def derived(canonical: str) -> dict[str, str]:
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)-rc\.(\d+)", canonical)
    if match is None:
        raise ValueError(f"canonical version is not a numbered RC: {canonical}")
    major, minor, patch, candidate = match.groups()
    base = f"{major}.{minor}.{patch}"
    return {
        "cargo": canonical, "clojars": canonical, "cmake": canonical,
        "fpm": base, "goTag": f"v{canonical}", "hackage": base,
        "luaRocks": f"{base}rc{candidate}-1", "maven": canonical, "npm": canonical,
        "nuget": canonical, "opam": f"{base}~rc{candidate}",
        "pkgConfig": canonical, "pypi": f"{base}rc{candidate}",
        "rubygems": f"{base}.rc.{candidate}", "swiftTag": canonical,
    }


def text(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def replace(path: str, pattern: str, replacement: str, expected: int = 1) -> None:
    target = ROOT / path
    updated, count = re.subn(pattern, replacement, target.read_text(encoding="utf-8"), flags=re.MULTILINE)
    if count != expected:
        raise ValueError(f"{path}: expected {expected} matches for {pattern!r}, found {count}")
    target.write_text(updated, encoding="utf-8")


def rewrite_candidate_tokens(patterns: tuple[str, ...], canonical: str) -> None:
    base, candidate = canonical.split("-rc.", 1)
    escaped = re.escape(base)
    replacements = (
        (rf"{escaped}\.rc\.\d+", f"{base}.rc.{candidate}"),
        (rf"{escaped}~rc\d+", f"{base}~rc{candidate}"),
        (rf"{escaped}rc\d+-\d+", f"{base}rc{candidate}-1"),
        (rf"{escaped}rc\d+", f"{base}rc{candidate}"),
        (rf"{escaped}-rc\.\d+", canonical),
    )
    for pattern in patterns:
        for target in ROOT.glob(pattern):
            relative = target.relative_to(ROOT)
            if not target.is_file() or GENERATED_TREE_PARTS.intersection(relative.parts):
                continue
            source = target.read_text(encoding="utf-8")
            for version_pattern, replacement in replacements:
                source = re.sub(version_pattern, replacement, source)
            target.write_text(source, encoding="utf-8")


def update_json(path: str, mutate) -> None:
    target = ROOT / path
    value = json.loads(target.read_text(encoding="utf-8"))
    mutate(value)
    target.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def write_versions(model: dict[str, object], versions: dict[str, str]) -> None:
    canonical = str(model["canonical"])
    candidate = canonical.rsplit(".", 1)[-1]
    deps = model["dependencies"]
    assert isinstance(deps, dict)
    replace("Cargo.toml", r'^version = "[^"]+"$', f'version = "{canonical}"')
    replace(
        "Cargo.toml", r'^vinary-tree-interop = \{[^\n]+\}$',
        f'vinary-tree-interop = {{ path = "../vinary-tree-interop", version = "={deps["vinary-tree-interop"]}", optional = true }}',
    )
    replace(
        "Cargo.toml", r'^llattice = \{[^\n]+\}$',
        f'llattice = {{ path = "../llattice", version = "={deps["llattice"]}" }}',
    )

    def api(value: dict) -> None:
        value["packageVersion"] = canonical
        value["interop"]["version"] = deps["vinary-tree-interop"]
        value["siblingPins"]["liblevenshtein"] = deps["liblevenshtein"]
        value["siblingPins"]["llattice"] = deps["llattice"]
        value["wasm"]["umbrellaVersion"] = deps["@vinary-tree/vinary-tree"]
        value["release"] = {"canonical": canonical, "registries": versions, "distTag": model["publication"]["distTag"]}
        value["packages"]["_source"]["luarocks"] = (
            f'bindings/lua/vinary-tree-libdictenstein-{versions["luaRocks"]}.rockspec package'
        )
        value["documentation"]["facades"]["lua"]["package"] = (
            f'bindings/lua/vinary-tree-libdictenstein-{versions["luaRocks"]}.rockspec package'
        )
    update_json("bindings/api.json", api)

    def npm(value: dict) -> None:
        value["version"] = versions["npm"]
        value["dependencies"]["@vinary-tree/interop"] = deps["@vinary-tree/interop"]
        value["dependencies"]["@vinary-tree/vinary-tree"] = deps["@vinary-tree/vinary-tree"]
        value.setdefault("publishConfig", {})["tag"] = model["publication"]["distTag"]
    update_json("bindings/javascript/package.json", npm)
    replace(
        "bindings/javascript/test/facades.test.mjs",
        r'assert\.equal\(packageJson\.dependencies\["@vinary-tree/vinary-tree"\], "[^"]+"\);',
        f'assert.equal(packageJson.dependencies["@vinary-tree/vinary-tree"], "{deps["@vinary-tree/vinary-tree"]}");',
    )
    replace("bindings/python/pyproject.toml", r'^version = "[^"]+"$', f'version = "{versions["pypi"]}"')
    replace(
        "bindings/python/pyproject.toml",
        r'("vinary-tree-interop==)[^"]+(")',
        rf'\g<1>{versions["pypi"]}\2',
    )
    replace("bindings/jvm/build.gradle.kts", r'^version = "[^"]+"$', f'version = "{versions["maven"]}"')
    replace("bindings/jvm/jreleaser.yml", r'^  version: \S+$', f'  version: {versions["maven"]}')
    replace("bindings/jvm/build.gradle.kts", r'api\("io\.vinarytree:vinary-tree-interop:[^"]+"\)', f'api("io.vinarytree:vinary-tree-interop:{versions["maven"]}")')
    replace("bindings/jvm/build.gradle.kts", r'testImplementation\("io\.vinarytree:liblevenshtein:[^"]+"\)', f'testImplementation("io.vinarytree:liblevenshtein:{versions["maven"]}")')
    replace("bindings/clojure/project.clj", r'^(\(defproject io\.vinarytree/libdictenstein-clojure) "[^"]+"$', rf'\1 "{versions["clojars"]}"')
    for artifact in ("vinary-tree-interop", "libdictenstein", "liblevenshtein"):
        replace("bindings/clojure/project.clj", rf'\[io\.vinarytree/{artifact} "[^"]+"\]', f'[io.vinarytree/{artifact} "{versions["maven"]}"]')
    replace("bindings/dotnet/src/VinaryTree.Libdictenstein/VinaryTree.Libdictenstein.csproj", r'^    <Version>[^<]+</Version>$', f'    <Version>{versions["nuget"]}</Version>')
    replace(
        "bindings/dotnet/src/VinaryTree.Libdictenstein/VinaryTree.Libdictenstein.csproj",
        r'<PackageReference Include="VinaryTree\.Interop" Version="[^"]+" />',
        f'<PackageReference Include="VinaryTree.Interop" Version="{versions["nuget"]}" />',
    )
    replace("bindings/ruby/lib/vinary_tree/libdictenstein/version.rb", r'^    VERSION = "[^"]+"$', f'    VERSION = "{versions["rubygems"]}"')
    replace("bindings/fortran/fpm.toml", r'^version = "[^"]+"$', f'version = "{versions["fpm"]}"')
    replace("bindings/fortran/fpm.publish.toml", r'^version = "[^"]+"$', f'version = "{versions["fpm"]}"')
    replace("bindings/fortran/fpm.publish.toml", r'^v = "[^"]+"$', f'v = "{versions["fpm"]}"')
    replace("bindings/go/go.mod", r'^module \S+$', "module github.com/vinary-tree/libdictenstein/bindings/go/v4")
    replace(
        "bindings/go/go.mod", r'github\.com/vinary-tree/liblevenshtein-rust/bindings/go(?:/v4)? v\S+',
        f'github.com/vinary-tree/liblevenshtein-rust/bindings/go/v4 {versions["goTag"]}',
    )
    replace(
        "bindings/go/go.mod", r'github\.com/vinary-tree/(?:liblevenshtein-rust/vinary-tree-interop|vinary-tree-interop)/bindings/go(?:/v4)? v\S+',
        f'github.com/vinary-tree/vinary-tree-interop/bindings/go/v4 {versions["goTag"]}',
    )
    for path in ("bindings/go/entries.go", "bindings/go/libdictenstein.go"):
        source = text(path)
        source = source.replace("../../../liblevenshtein-rust/vinary-tree-interop/include", "../../../vinary-tree-interop/include")
        source = re.sub(
            r"github\.com/vinary-tree/(?:liblevenshtein-rust/vinary-tree-interop|vinary-tree-interop)/bindings/go(?:/v4)?",
            "github.com/vinary-tree/vinary-tree-interop/bindings/go/v4",
            source,
        )
        (ROOT / path).write_text(source, encoding="utf-8")
    for path in ("bindings/ocaml/vinary-tree-libdictenstein.opam", "bindings/ocaml/vinary-tree-libdictenstein.opam.template"):
        replace(path, r'"vinary-tree-interop" \{[^}]+\}', f'"vinary-tree-interop" {{= "{versions["opam"]}"}}')
    replace("bindings/haskell/vinary-tree-libdictenstein.cabal", r'^version: \S+$', f'version: {versions["hackage"]}')
    cabal_path = ROOT / "bindings/haskell/vinary-tree-libdictenstein.cabal"
    cabal = cabal_path.read_text(encoding="utf-8")
    if "x-release-candidate:" not in cabal:
        cabal = cabal.replace(f"version: {versions['hackage']}\n", f"version: {versions['hackage']}\nx-release-candidate: rc.1\n", 1)
    cabal = re.sub(
        r"^x-release-candidate: \S+$",
        f"x-release-candidate: rc.{candidate}",
        cabal,
        flags=re.MULTILINE,
    )
    cabal = re.sub(
        r"vinary-tree-interop >=[^\s,]+ && <[^\s,]+",
        "vinary-tree-interop >=4 && <5",
        cabal,
    )
    cabal_path.write_text(cabal, encoding="utf-8")
    for path in ("Package.swift", "bindings/swift/libdictenstein/Package.swift"):
        replace(
            path,
            r'(url: "https://github\.com/vinary-tree/vinary-tree-interop\.git",\n\s+exact: ")[^"]+("\n)',
            rf'\g<1>{versions["swiftTag"]}\2',
        )
    replace(
        "bindings/clojure/deps.edn",
        r'io\.vinarytree/vinary-tree-interop \{:mvn/version "[^"]+"\}',
        f'io.vinarytree/vinary-tree-interop {{:mvn/version "{versions["maven"]}"}}',
    )
    replace(
        "bindings/clojure/deps.edn",
        r'io\.vinarytree/libdictenstein \{:mvn/version "[^"]+"\}',
        f'io.vinarytree/libdictenstein {{:mvn/version "{versions["maven"]}"}}',
    )
    replace(
        "bindings/javascript/deps.cljs",
        r'"@vinary-tree/libdictenstein" "[^"]+"',
        f'"@vinary-tree/libdictenstein" "{versions["npm"]}"',
    )
    lua_path = f'bindings/lua/vinary-tree-libdictenstein-{versions["luaRocks"]}.rockspec'
    lua_target = ROOT / lua_path
    if not lua_target.exists():
        candidates = list((ROOT / "bindings/lua").glob("vinary-tree-libdictenstein-*.rockspec"))
        if len(candidates) != 1:
            raise ValueError(f"expected one LuaRocks source file, found {len(candidates)}")
        candidates[0].rename(lua_target)
    replace(lua_path, r'^version = "[^"]+"$', f'version = "{versions["luaRocks"]}"')
    replace(
        lua_path,
        r'^(source = \{ url = "[^"]+", tag = ")[^"]+(" \})$',
        rf'\g<1>{versions["goTag"]}\2',
    )
    replace("cmake/libdictensteinConfigVersion.cmake", r'^set\(PACKAGE_VERSION "[^"]+"\)$', f'set(PACKAGE_VERSION "{versions["cmake"]}")')
    replace("pkgconfig/libdictenstein.pc", r'^Version: \S+$', f'Version: {versions["pkgConfig"]}')
    replace("pkgconfig/libdictenstein.pc", r'^Requires: vinary-tree-interop = \S+$', f'Requires: vinary-tree-interop = {versions["pkgConfig"]}')
    rewrite_candidate_tokens(
        (
            ".github/workflows/*.yml",
            "bindings/**/*.md",
            "docs/**/*.md",
            "docs/**/*.puml",
        ),
        canonical,
    )


def validate(model: dict[str, object], versions: dict[str, str]) -> list[str]:
    failures: list[str] = []
    if model.get("registries") != versions:
        failures.append("release/version.json registry spellings are stale")
    publication = model.get("publication", {})
    if not isinstance(publication, dict) or publication.get("fpm") is not False or publication.get("hackage") is not False:
        failures.append("numeric-only fpm and Hackage RC publication must remain embargoed")
    canonical = str(model["canonical"])
    candidate = canonical.rsplit(".", 1)[-1]
    deps = model["dependencies"]
    assert isinstance(deps, dict)
    required = {
        "Cargo": ("Cargo.toml", r'^version = "([^"]+)"$', canonical),
        "Python": ("bindings/python/pyproject.toml", r'^version = "([^"]+)"$', versions["pypi"]),
        "JVM": ("bindings/jvm/build.gradle.kts", r'^version = "([^"]+)"$', versions["maven"]),
        ".NET": ("bindings/dotnet/src/VinaryTree.Libdictenstein/VinaryTree.Libdictenstein.csproj", r'<Version>([^<]+)</Version>', versions["nuget"]),
        "Ruby": ("bindings/ruby/lib/vinary_tree/libdictenstein/version.rb", r'VERSION = "([^"]+)"', versions["rubygems"]),
        "CMake": ("cmake/libdictensteinConfigVersion.cmake", r'PACKAGE_VERSION "([^"]+)"', versions["cmake"]),
        "pkg-config": ("pkgconfig/libdictenstein.pc", r'^Version: (\S+)$', versions["pkgConfig"]),
        "pkg-config interop": ("pkgconfig/libdictenstein.pc", r'^Requires: vinary-tree-interop = (\S+)$', versions["pkgConfig"]),
        "Swift root": ("Package.swift", r'exact: "([^"]+)"', versions["swiftTag"]),
        "Swift facade": ("bindings/swift/libdictenstein/Package.swift", r'exact: "([^"]+)"', versions["swiftTag"]),
        "LuaRocks": (f'bindings/lua/vinary-tree-libdictenstein-{versions["luaRocks"]}.rockspec', r'^version = "([^"]+)"$', versions["luaRocks"]),
    }
    for name, (path, pattern, wanted) in required.items():
        match = re.search(pattern, text(path), flags=re.MULTILINE)
        if (match.group(1) if match else None) != wanted:
            failures.append(f"{name} version is stale")
    api = json.loads(text("bindings/api.json"))
    if api.get("packageVersion") != canonical or api.get("interop", {}).get("version") != deps["vinary-tree-interop"]:
        failures.append("binding model release identity is stale")
    package = json.loads(text("bindings/javascript/package.json"))
    if package.get("version") != versions["npm"] or package.get("publishConfig", {}).get("tag") != publication.get("distTag"):
        failures.append("npm release identity is stale")
    go_mod = text("bindings/go/go.mod")
    if "module github.com/vinary-tree/libdictenstein/bindings/go/v4" not in go_mod:
        failures.append("Go module lacks /v4 semantic import path")
    cabal = text("bindings/haskell/vinary-tree-libdictenstein.cabal")
    if f"x-release-candidate: rc.{candidate}" not in cabal:
        failures.append("Hackage source candidate marker is missing")
    if "vinary-tree-interop >=4 && <5,\n    vinary-tree-libdictenstein" not in cabal:
        failures.append("Hackage conformance dependencies are malformed")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    model = json.loads(MODEL_PATH.read_text(encoding="utf-8"))
    versions = derived(str(model["canonical"]))
    if args.write:
        write_versions(model, versions)
    failures = validate(model, versions)
    if failures:
        for failure in failures:
            print(f"release-version error: {failure}", file=sys.stderr)
        return 1
    print(f"release versions agree with {model['canonical']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
