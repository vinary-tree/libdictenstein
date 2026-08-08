#!/usr/bin/env python3
"""Dependency-free architectural gate for the libdictenstein language bindings.

Cross-checks the machine-readable binding model (bindings/api.json) against the
three sources it mirrors and the thirteen language facades that consume it:

  A. Symbol parity      - api.json cFunctions == `pub extern "C" fn ldict_*` in
                          src/ffi.rs == declarations in include/libdictenstein.h,
                          including persistent-artrie feature attribution.
  B. Constant parity    - LDICT_ABI_VERSION / LDICT_API_REVISION, LdictStatus
                          numeric values, LDICT_KIND_* and LDICT_CAP_* constants,
                          and unit domains, across api.json / src/ffi.rs /
                          include/libdictenstein.h (+ the interop header mirror).
  C. Facade coverage    - every ldict_* symbol referenced by a facade's FFI
                          import layer must exist in the model (referenced-symbol
                          check: facades may legitimately cover fewer than all
                          symbols today; completeness enforcement is a later
                          wave). Reports n-referenced / n-modeled per facade.
  D. Identity guard     - no f1r3fly / universal-automata strings in any
                          publishable facade or packaging file.
  E. Interop mirror     - include/vinary_tree_interop.h must be byte-identical
                          to the canonical header in the liblevenshtein-rust
                          sibling checkout (LDICT_INTEROP_HEADER_CANONICAL
                          overrides the default path; missing sibling => skip
                          with a warning, which keeps the gate usable in
                          isolated checkouts). The in-repo OCaml copies of both
                          headers are always checked against the in-repo
                          originals.
  F. JavaScript guards  - export-map subpaths, facade files, and exact-pin
                          @vinary-tree/* dependency versions (no ^/~ ranges).

Plus package-coordinate/version coherence for every facade metadata file and
sibling-pin coherence (all liblevenshtein-rust references agree with the model;
whether the pinned tag exists is a release concern tracked as ledger finding
LDICT-B1 in docs/bindings/FINDINGS_LEDGER.md).

Exit status 0 iff no check failed. `--json` emits a machine-readable report.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parents[1]
MODEL_PATH = ROOT / "bindings" / "api.json"

SYMBOL = re.compile(r"(?<![A-Za-z0-9_])ldict_[a-z0-9_]+")
PERSISTENT_FEATURE = "persistent-artrie"


class Report:
    """Collects results from every check instead of failing fast."""

    def __init__(self) -> None:
        self.failures: list[str] = []
        self.warnings: list[str] = []
        self.passes: list[str] = []
        self.facades: dict[str, dict[str, object]] = {}

    def fail(self, check: str, message: str) -> None:
        self.failures.append(f"[{check}] {message}")

    def warn(self, check: str, message: str) -> None:
        self.warnings.append(f"[{check}] {message}")

    def ok(self, check: str, message: str) -> None:
        self.passes.append(f"[{check}] {message}")


def read_text(report: Report, check: str, path: Path) -> str | None:
    if not path.is_file():
        report.fail(check, f"required file is missing: {path.relative_to(ROOT)}")
        return None
    return path.read_text(encoding="utf-8")


def rust_variant_to_screaming(name: str) -> str:
    """InvalidUtf8 -> INVALID_UTF8, IoError -> IO_ERROR, Ok -> OK."""
    return "_".join(
        fragment.upper() for fragment in re.findall(r"[A-Z][a-z0-9]*", name)
    )


# ---------------------------------------------------------------------------
# Source-of-truth parsers (src/ffi.rs and include/libdictenstein.h).
# ---------------------------------------------------------------------------


def parse_ffi_exports(source: str) -> dict[str, str | None]:
    """Exported extern "C" symbols -> gating feature (or None)."""
    pattern = re.compile(
        r'(?P<cfg>#\[cfg\(feature = "persistent-artrie"\)\]\s*)?'
        r"#\[no_mangle\]\s*"
        r'pub\s+(?:unsafe\s+)?extern\s+"C"\s+fn\s+(?P<name>ldict_[a-z0-9_]+)\s*\(',
        re.DOTALL,
    )
    return {
        match.group("name"): (PERSISTENT_FEATURE if match.group("cfg") else None)
        for match in pattern.finditer(source)
    }


def parse_header_declarations(source: str) -> set[str]:
    return set(
        re.findall(r"LDICT_API\s+[^();]*?\b(ldict_[a-z0-9_]+)\s*\(", source)
    )


def parse_ffi_status(source: str) -> dict[str, int]:
    block = re.search(r"pub enum LdictStatus \{(.*?)\n\}", source, re.DOTALL)
    if not block:
        return {}
    return {
        rust_variant_to_screaming(name): int(value)
        for name, value in re.findall(r"(\w+) = (\d+),", block.group(1))
    }


def parse_header_status(source: str) -> dict[str, int]:
    block = re.search(r"typedef enum LdictStatus \{(.*?)\} LdictStatus;", source, re.DOTALL)
    if not block:
        return {}
    return {
        name: int(value)
        for name, value in re.findall(r"LDICT_STATUS_([A-Z0-9_]+) = (\d+)", block.group(1))
    }


def parse_ffi_u32_const(source: str, name: str) -> int | None:
    match = re.search(rf"pub const {name}: u32 = (\d+);", source)
    return int(match.group(1)) if match else None


def parse_header_define(source: str, name: str) -> int | None:
    match = re.search(rf"#define {name} (\d+)u", source)
    return int(match.group(1)) if match else None


def parse_ffi_kinds(source: str) -> dict[str, int]:
    return {
        name: int(value)
        for name, value in re.findall(
            r"pub const LDICT_KIND_([A-Z0-9_]+): u32 = (\d+);", source
        )
    }


def parse_header_kinds(source: str) -> dict[str, int]:
    return {
        name: int(value)
        for name, value in re.findall(r"#define LDICT_KIND_([A-Z0-9_]+) (\d+)u", source)
    }


def parse_ffi_capabilities(source: str) -> dict[str, int]:
    return {
        name: int(bit)
        for name, bit in re.findall(
            r"pub const LDICT_CAP_([A-Z0-9_]+): u64 = 1 << (\d+);", source
        )
    }


def parse_header_capabilities(source: str) -> dict[str, int]:
    return {
        name: int(bit)
        for name, bit in re.findall(
            r"#define LDICT_CAP_([A-Z0-9_]+) \(UINT64_C\(1\) << (\d+)\)", source
        )
    }


def parse_ffi_unit_domains(source: str) -> dict[str, int]:
    variant_to_model = {"Byte": "BYTE", "UnicodeScalar": "UNICODE_SCALAR", "U64": "U64"}
    return {
        variant_to_model[variant]: int(value)
        for value, variant in re.findall(
            r"(\d+) => Ok\(BindingUnitDomain::(Byte|UnicodeScalar|U64)\)", source
        )
    }


def parse_interop_unit_domains(source: str) -> dict[str, int]:
    return {
        name: int(value)
        for name, value in re.findall(r"VT_UNIT_DOMAIN_([A-Z0-9_]+) = (\d+)", source)
    }


# ---------------------------------------------------------------------------
# Facade import-layer parsers. Each returns the set of ldict_* C ABI symbols
# the facade's FFI layer references (locally-defined helpers excluded).
# ---------------------------------------------------------------------------


def c_source_symbols(source: str) -> tuple[set[str], set[str]]:
    """(referenced, locally_defined) for a C/C++ source or header facade.

    A local definition is an unindented `ReturnType name(` line: shim helpers
    such as the Haskell cbits `ldict_hs_*` family. Every other boundary-clean
    ldict_* token is a reference into the shared ABI.
    """
    defined = set(
        re.findall(r"^[A-Za-z_][^;{}()\n]*?\b(ldict_[a-z0-9_]+)\s*\(", source, re.M)
    )
    referenced = set(SYMBOL.findall(source)) - defined
    return referenced, defined


def python_ctypes_symbols(report: Report, source: str) -> set[str]:
    referenced = set(re.findall(r"_lib\.(ldict_[a-z0-9_]+)", source))
    loops = re.findall(
        r'for name in \(([^)]+)\):\s*\n\s*function = getattr\(_lib, f"(ldict_[a-z0-9_]+)\{name\}"\)',
        source,
    )
    for items, prefix in loops:
        for item in re.findall(r'"([a-z0-9_]+)"', items):
            referenced.add(f"{prefix}{item}")
    total_dynamic = len(re.findall(r'getattr\(_lib, f"ldict_[a-z0-9_]+\{', source))
    if total_dynamic != len(loops):
        report.fail(
            "facades",
            "python: dynamically constructed ctypes symbol names the parser "
            f"cannot expand ({total_dynamic} getattr f-strings, {len(loops)} "
            "recognized `for name in (...)` loops); update _native.py or the parser",
        )
    return referenced


def java_downcall_symbols(source: str) -> set[str]:
    return set(re.findall(r'"(ldict_[a-z0-9_]+)"', source))


def dotnet_entrypoint_symbols(report: Report, source: str) -> set[str]:
    referenced = set(
        re.findall(r'EntryPoint = "(ldict_[a-z0-9_]+)"', source)
    )
    stray = set(SYMBOL.findall(source)) - referenced
    if stray:
        report.fail(
            "facades",
            f"dotnet: ldict_* tokens outside LibraryImport EntryPoint strings: {sorted(stray)}",
        )
    return referenced


def cgo_symbols(source: str) -> set[str]:
    """cgo facade: C.ldict_* calls in Go code plus calls inside the preamble.

    The boundary regex covers both; preamble helpers such as
    go_ldict_resource_words never match because of the identifier lookbehind.
    """
    return set(SYMBOL.findall(source))


def ruby_fiddle_symbols(source: str) -> set[str]:
    return set(SYMBOL.findall(source))


def haskell_symbols(
    report: Report, hs_source: str, cbits_sources: list[str]
) -> set[str]:
    imports = {
        name.lstrip("&")
        for name in re.findall(
            r'foreign import ccall\s+(?:safe\s+|unsafe\s+)?"(&?ldict_[a-z0-9_]+)"',
            hs_source,
        )
    }
    stray = set(SYMBOL.findall(hs_source)) - imports
    if stray:
        report.fail(
            "facades",
            f"haskell: ldict_* tokens outside foreign import declarations: {sorted(stray)}",
        )
    shim_defined: set[str] = set()
    shim_referenced: set[str] = set()
    for source in cbits_sources:
        referenced, defined = c_source_symbols(source)
        shim_defined |= defined
        shim_referenced |= referenced
    unresolved_imports = imports - shim_defined
    missing_shims = {
        name for name in unresolved_imports if name.startswith("ldict_hs_")
    }
    if missing_shims:
        report.fail(
            "facades",
            f"haskell: foreign imports target undefined cbits shims: {sorted(missing_shims)}",
        )
    return (unresolved_imports - missing_shims) | shim_referenced


def fortran_bindc_symbols(source: str) -> set[str]:
    return set(
        re.findall(r'bind\(c,\s*name="(ldict_[a-z0-9_]+)"\)', source, re.IGNORECASE)
    )


def swift_symbols(report: Report, model: dict) -> set[str]:
    facade = model["facades"]["swift"]
    modulemap = read_text(report, "facades", ROOT / facade["modulemap"])
    shim_header = read_text(report, "facades", ROOT / facade["shimHeader"])
    if modulemap is not None and 'header "CLibdictenstein.h"' not in modulemap:
        report.fail("facades", "swift: module.modulemap does not import CLibdictenstein.h")
    if shim_header is not None and "libdictenstein.h" not in shim_header:
        report.fail(
            "facades",
            "swift: CLibdictenstein.h does not include the project C header libdictenstein.h",
        )
    referenced: set[str] = set()
    for layer in facade["importLayers"]:
        source = read_text(report, "facades", ROOT / layer)
        if source is not None:
            referenced |= set(SYMBOL.findall(source))
    return referenced


def clojure_check(report: Report, model: dict) -> dict[str, object]:
    facade = model["facades"]["clojure"]
    layer = ROOT / facade["importLayers"][0]
    source = read_text(report, "facades", layer)
    if source is None:
        return {"mode": "mediated", "missingDefs": ["<file missing>"]}
    direct = set(SYMBOL.findall(source))
    if direct:
        report.fail(
            "facades",
            f"clojure: facade must stay mediated through the JVM package, found raw symbols {sorted(direct)}",
        )
    public_defs = set(re.findall(r"\(defn\s+([^\s\)\^]+)", source))
    missing = [name for name in facade["requiredDefs"] if name not in public_defs]
    if missing:
        report.fail("facades", f"clojure: missing required public defs: {missing}")
    return {
        "mode": "mediated",
        "requiredDefs": len(facade["requiredDefs"]),
        "presentDefs": len(facade["requiredDefs"]) - len(missing),
    }


def javascript_check(report: Report, model: dict) -> dict[str, object]:
    package_path = ROOT / "bindings" / "javascript" / "package.json"
    source = read_text(report, "javascript", package_path)
    if source is None:
        return {"mode": "mediated"}
    package = json.loads(source)
    wasm = model["wasm"]
    if package["name"] != model["packages"]["npm"]:
        report.fail("javascript", f"npm package name {package['name']!r} != model")
    if package["version"] != model["packageVersion"]:
        report.fail(
            "javascript",
            f"npm version {package['version']} != packageVersion {model['packageVersion']}",
        )
    for export in wasm["exports"]:
        if export not in package.get("exports", {}):
            report.fail("javascript", f"package.json exports lacks {export!r}")
    for facade_file in (
        "facades/native.mjs",
        "facades/native.cjs",
        "facades/typescript.mjs",
        "facades/typescript.cjs",
        "facades/clojurescript.mjs",
        "facades/clojurescript.cjs",
        "facades/wasm.mjs",
        "facades/wasi.mjs",
        "facades/wasi.cjs",
        "index.d.ts",
    ):
        if not (ROOT / "bindings" / "javascript" / facade_file).is_file():
            report.fail("javascript", f"missing facade file bindings/javascript/{facade_file}")
    exact = re.compile(r"^\d+\.\d+\.\d+$")
    dependencies = package.get("dependencies", {})
    for name, version in dependencies.items():
        if name.startswith("@vinary-tree/") and not exact.match(version):
            report.fail(
                "javascript",
                f"@vinary-tree/* dependencies must be exact pins, found {name}: {version!r}",
            )
    interop_pin = dependencies.get(wasm["interopPackage"])
    if interop_pin != model["interop"]["version"]:
        report.fail(
            "javascript",
            f"{wasm['interopPackage']} pin {interop_pin!r} != interop version {model['interop']['version']!r}",
        )
    umbrella_pin = dependencies.get(wasm["umbrellaPackage"])
    if umbrella_pin != wasm["umbrellaVersion"]:
        report.fail(
            "javascript",
            f"{wasm['umbrellaPackage']} pin {umbrella_pin!r} != wasm.umbrellaVersion {wasm['umbrellaVersion']!r}",
        )
    deps_cljs = read_text(report, "javascript", ROOT / "bindings" / "javascript" / "deps.cljs")
    if deps_cljs is not None:
        pin = re.search(r'"@vinary-tree/libdictenstein"\s+"([^"]+)"', deps_cljs)
        if not pin or pin.group(1) != model["packageVersion"]:
            report.fail(
                "javascript",
                "deps.cljs must pin @vinary-tree/libdictenstein to "
                f"{model['packageVersion']} (found {pin.group(1) if pin else 'nothing'})",
            )
    return {"mode": "mediated", "exports": len(wasm["exports"])}


# ---------------------------------------------------------------------------
# Checks.
# ---------------------------------------------------------------------------


def check_symbol_parity(report: Report, model: dict) -> None:
    modeled = {entry["name"]: entry.get("feature") for entry in model["cFunctions"]}
    ffi_source = read_text(report, "symbols", ROOT / "src" / "ffi.rs")
    header_source = read_text(report, "symbols", ROOT / model["cHeader"])
    if ffi_source is None or header_source is None:
        return
    exported = parse_ffi_exports(ffi_source)
    declared = parse_header_declarations(header_source)

    for label, missing in (
        ("modeled in api.json but not exported by src/ffi.rs", set(modeled) - set(exported)),
        ("exported by src/ffi.rs but missing from api.json", set(exported) - set(modeled)),
        ("modeled in api.json but not declared in the C header", set(modeled) - declared),
        ("declared in the C header but missing from api.json", declared - set(modeled)),
        ("exported by src/ffi.rs but not declared in the C header", set(exported) - declared),
        ("declared in the C header but not exported by src/ffi.rs", declared - set(exported)),
    ):
        if missing:
            report.fail("symbols", f"{label}: {sorted(missing)}")
    for name, feature in sorted(modeled.items()):
        if name in exported and exported[name] != feature:
            report.fail(
                "symbols",
                f"{name}: api.json feature={feature!r} but src/ffi.rs cfg gate={exported[name]!r}",
            )
    if set(modeled) == set(exported) == declared:
        report.ok(
            "symbols",
            f"{len(modeled)} ldict_* symbols agree across api.json, src/ffi.rs, and {model['cHeader']}",
        )


def compare_maps(
    report: Report, check: str, subject: str, expected: dict, actual: dict, source: str
) -> bool:
    clean = True
    for key in sorted(set(expected) | set(actual)):
        if key not in actual:
            report.fail(check, f"{subject}: {key} missing from {source}")
            clean = False
        elif key not in expected:
            report.fail(check, f"{subject}: {source} defines unmodeled {key} = {actual[key]}")
            clean = False
        elif expected[key] != actual[key]:
            report.fail(
                check,
                f"{subject}: api.json {key}={expected[key]} but {source} has {actual[key]}",
            )
            clean = False
    return clean


def check_constant_parity(report: Report, model: dict) -> None:
    ffi_source = read_text(report, "constants", ROOT / "src" / "ffi.rs")
    header_source = read_text(report, "constants", ROOT / model["cHeader"])
    interop_source = read_text(report, "constants", ROOT / model["interop"]["headerMirror"])
    if ffi_source is None or header_source is None or interop_source is None:
        return
    clean = True
    for name, model_value, ffi_value, header_value in (
        (
            "LDICT_ABI_VERSION",
            model["abiVersion"],
            parse_ffi_u32_const(ffi_source, "LDICT_ABI_VERSION"),
            parse_header_define(header_source, "LDICT_ABI_VERSION"),
        ),
        (
            "LDICT_API_REVISION",
            model["apiRevision"],
            parse_ffi_u32_const(ffi_source, "LDICT_API_REVISION"),
            parse_header_define(header_source, "LDICT_API_REVISION"),
        ),
    ):
        for source, value in (("src/ffi.rs", ffi_value), (model["cHeader"], header_value)):
            if value != model_value:
                report.fail(
                    "constants", f"{name}: api.json={model_value} but {source} has {value}"
                )
                clean = False
    status = model["enums"]["status"]["values"]
    clean &= compare_maps(report, "constants", "LdictStatus", status, parse_ffi_status(ffi_source), "src/ffi.rs")
    clean &= compare_maps(report, "constants", "LdictStatus", status, parse_header_status(header_source), model["cHeader"])
    kinds = model["kinds"]["values"]
    clean &= compare_maps(report, "constants", "LDICT_KIND_*", kinds, parse_ffi_kinds(ffi_source), "src/ffi.rs")
    clean &= compare_maps(report, "constants", "LDICT_KIND_*", kinds, parse_header_kinds(header_source), model["cHeader"])
    capabilities = model["capabilities"]["bits"]
    clean &= compare_maps(report, "constants", "LDICT_CAP_* bit", capabilities, parse_ffi_capabilities(ffi_source), "src/ffi.rs")
    clean &= compare_maps(report, "constants", "LDICT_CAP_* bit", capabilities, parse_header_capabilities(header_source), model["cHeader"])
    domains = model["unitDomains"]
    clean &= compare_maps(report, "constants", "unit domain", domains, parse_ffi_unit_domains(ffi_source), "src/ffi.rs domain()")
    clean &= compare_maps(report, "constants", "unit domain", domains, parse_interop_unit_domains(interop_source), model["interop"]["headerMirror"])
    if clean:
        report.ok(
            "constants",
            "ABI version/revision, LdictStatus, kind, capability, and unit-domain "
            "constants agree across api.json, src/ffi.rs, and the headers",
        )


def check_facades(report: Report, model: dict) -> None:
    modeled = {entry["name"] for entry in model["cFunctions"]}
    total = len(modeled)

    def record(language: str, referenced: set[str]) -> None:
        unknown = sorted(referenced - modeled)
        if unknown:
            report.fail(
                "facades",
                f"{language}: references symbols that do not exist in the model: {unknown}",
            )
        report.facades[language] = {
            "mode": "symbols",
            "referenced": len(referenced & modeled),
            "modeled": total,
            "unknown": unknown,
        }

    facades = model["facades"]

    for language in sorted(facades):
        facade = facades[language]
        parser = facade["parser"]
        if parser == "clojure-defs":
            report.facades[language] = clojure_check(report, model)
            continue
        if parser == "javascript-exports":
            report.facades[language] = javascript_check(report, model)
            continue
        if parser == "swift":
            record(language, swift_symbols(report, model))
            continue
        if parser == "haskell-foreign":
            hs_source = read_text(report, "facades", ROOT / facade["importLayers"][0])
            cbits = [
                text
                for layer in facade["shimLayers"]
                if (text := read_text(report, "facades", ROOT / layer)) is not None
            ]
            if hs_source is None:
                continue
            record(language, haskell_symbols(report, hs_source, cbits))
            continue

        referenced: set[str] = set()
        for layer in facade["importLayers"]:
            source = read_text(report, "facades", ROOT / layer)
            if source is None:
                continue
            if parser == "python-ctypes":
                referenced |= python_ctypes_symbols(report, source)
            elif parser == "java-downcalls":
                referenced |= java_downcall_symbols(source)
            elif parser == "dotnet-entrypoints":
                referenced |= dotnet_entrypoint_symbols(report, source)
            elif parser == "cgo":
                referenced |= cgo_symbols(source)
            elif parser == "ruby-fiddle":
                referenced |= ruby_fiddle_symbols(source)
            elif parser == "fortran-bindc":
                referenced |= fortran_bindc_symbols(source)
            elif parser == "c-source":
                refs, _defined = c_source_symbols(source)
                referenced |= refs
            else:
                report.fail("facades", f"{language}: unknown parser {parser!r} in api.json")
        record(language, referenced)

    if len(facades) != 13:
        report.fail("facades", f"expected 13 modeled facades, api.json lists {len(facades)}")
    missing_dirs = [
        language
        for language, facade in facades.items()
        if not (ROOT / facade["root"]).is_dir()
    ]
    if missing_dirs:
        report.fail("facades", f"facade roots missing on disk: {missing_dirs}")
    on_disk = {path.name for path in (ROOT / "bindings").iterdir() if path.is_dir()}
    unmodeled_dirs = sorted(on_disk - set(facades) - {"api.json"})
    if unmodeled_dirs:
        report.fail(
            "facades",
            f"bindings/ contains facade directories absent from api.json: {unmodeled_dirs}",
        )


IDENTITY_SUFFIXES = {
    ".c", ".cabal", ".cjs", ".clj", ".cljs", ".cmake", ".cs", ".csproj", ".edn",
    ".f90", ".gemspec", ".go", ".gradle", ".h", ".hpp", ".hs", ".hsc", ".java",
    ".json", ".kts", ".lua", ".m", ".md", ".mjs", ".ml", ".mli", ".mod",
    ".modulemap", ".opam", ".pc", ".properties", ".props", ".publish", ".py",
    ".rb", ".rockspec", ".rs", ".swift", ".template", ".toml", ".ts", ".tsv",
    ".txt", ".work", ".yml",
}
IDENTITY_SKIP_PARTS = {
    ".build", "_build", ".cpcache", ".gradle", ".pytest_cache", ".ruff_cache",
    ".swiftpm", "__pycache__", "bin", "build", "dist-newstyle", "node_modules",
    "obj", "target",
}


def check_identity(report: Report) -> None:
    roots = [
        ROOT / "bindings",
        ROOT / "include",
        ROOT / "cmake",
        ROOT / "pkgconfig",
        ROOT / "Package.swift",
        ROOT / "go.work",
        ROOT / "src" / "bindings.rs",
        ROOT / "src" / "ffi.rs",
    ]
    files: list[Path] = []
    for root in roots:
        if root.is_file():
            files.append(root)
        elif root.is_dir():
            files.extend(path for path in root.rglob("*") if path.is_file())
    offenders: list[str] = []
    for path in files:
        if path.suffix not in IDENTITY_SUFFIXES and path.name not in {"dune", "dune-project", "dune.publish"}:
            continue
        if any(part in IDENTITY_SKIP_PARTS for part in path.parts):
            continue
        source = path.read_text(encoding="utf-8", errors="ignore").lower()
        for forbidden in ("f1r3fly", "universal-automata", "universal_automata"):
            if forbidden in source:
                offenders.append(f"{path.relative_to(ROOT)}: {forbidden!r}")
    if offenders:
        report.fail("identity", "unrelated identities in publishable files: " + "; ".join(offenders))
    else:
        report.ok("identity", "no f1r3fly / universal-automata identities in publishable files")


def check_interop_header(report: Report, model: dict) -> None:
    mirror = ROOT / model["interop"]["headerMirror"]
    if not mirror.is_file():
        report.fail("interop", f"header mirror missing: {model['interop']['headerMirror']}")
        return
    canonical = Path(
        os.environ.get(
            "LDICT_INTEROP_HEADER_CANONICAL",
            ROOT.parent
            / "liblevenshtein-rust"
            / "vinary-tree-interop"
            / "include"
            / "vinary_tree_interop.h",
        )
    )
    if canonical.is_file():
        if mirror.read_bytes() != canonical.read_bytes():
            report.fail(
                "interop",
                f"{model['interop']['headerMirror']} is not byte-identical to the canonical header {canonical}",
            )
        else:
            report.ok("interop", f"header mirror is byte-identical to {canonical}")
    else:
        report.warn(
            "interop",
            f"canonical interop header not found at {canonical}; byte-equality skipped "
            "(set LDICT_INTEROP_HEADER_CANONICAL or clone the liblevenshtein-rust sibling)",
        )
    # The OCaml source package carries copies of both headers for opam builds;
    # they must never drift from the in-repo originals.
    for copy, original in (
        (ROOT / "bindings" / "ocaml" / "include" / "libdictenstein.h", ROOT / model["cHeader"]),
        (ROOT / "bindings" / "ocaml" / "include" / "vinary_tree_interop.h", mirror),
    ):
        if not copy.is_file():
            report.fail("interop", f"OCaml header copy missing: {copy.relative_to(ROOT)}")
        elif copy.read_bytes() != original.read_bytes():
            report.fail(
                "interop",
                f"{copy.relative_to(ROOT)} drifted from {original.relative_to(ROOT)}",
            )


def check_packages(report: Report, model: dict) -> None:
    packages = model["packages"]
    version = model["packageVersion"]
    interop_version = model["interop"]["version"]
    clean = True

    def expect(condition: bool, message: str) -> None:
        nonlocal clean
        if not condition:
            report.fail("packages", message)
            clean = False

    cargo_source = read_text(report, "packages", ROOT / "Cargo.toml")
    if cargo_source is not None:
        cargo = tomllib.loads(cargo_source)
        expect(cargo["package"]["name"] == model["crate"], "Cargo.toml package name != model crate")
        expect(
            cargo["package"]["version"] == version,
            f"Cargo.toml version {cargo['package']['version']} != packageVersion {version}",
        )
        features = cargo.get("features", {})
        expect("ffi" in features, "Cargo.toml lacks the ffi feature")
        expect(
            sorted(features.get("ffi", [])) == sorted(model["featureGates"]["ffi"]),
            f"Cargo.toml ffi feature {features.get('ffi')} != model featureGates.ffi",
        )
        interop_dep = cargo.get("dependencies", {}).get("vinary-tree-interop", {})
        expect(
            interop_dep.get("version") == interop_version,
            f"Cargo.toml vinary-tree-interop version {interop_dep.get('version')!r} != interop {interop_version!r}",
        )

    python_source = read_text(report, "packages", ROOT / "bindings" / "python" / "pyproject.toml")
    if python_source is not None:
        python = tomllib.loads(python_source)
        expect(python["project"]["name"] == packages["pypi"], "pyproject.toml name != packages.pypi")
        expect(
            python["project"]["version"] == version,
            f"pyproject.toml version {python['project']['version']} != {version}",
        )
        expect(
            f"vinary-tree-interop=={interop_version}" in python["project"]["dependencies"],
            f"pyproject.toml must pin vinary-tree-interop=={interop_version}",
        )

    gradle = read_text(report, "packages", ROOT / "bindings" / "jvm" / "build.gradle.kts")
    if gradle is not None:
        group, artifact = packages["maven"].split(":")
        expect(f'group = "{group}"' in gradle, f"build.gradle.kts group != {group}")
        expect(
            f'artifactId = "{artifact}"' in gradle,
            f"build.gradle.kts publication artifactId != {artifact}",
        )
        expect(f'version = "{version}"' in gradle, f"build.gradle.kts version != {version}")
        expect(
            f'api("{model["interop"]["maven"]}:{interop_version}")' in gradle,
            f"build.gradle.kts must pin {model['interop']['maven']}:{interop_version}",
        )

    project_clj = read_text(report, "packages", ROOT / "bindings" / "clojure" / "project.clj")
    if project_clj is not None:
        expect(
            f'{packages["clojars"]} "{version}"' in project_clj.replace("(defproject ", ""),
            f"project.clj coordinate != {packages['clojars']} {version}",
        )
        expect(
            f'[io.vinarytree/vinary-tree-interop "{interop_version}"]' in project_clj,
            f"project.clj must pin io.vinarytree/vinary-tree-interop {interop_version}",
        )
    deps_edn = read_text(report, "packages", ROOT / "bindings" / "clojure" / "deps.edn")
    if deps_edn is not None:
        expect(
            f'io.vinarytree/libdictenstein {{:mvn/version "{version}"}}' in deps_edn,
            f"clojure deps.edn must pin io.vinarytree/libdictenstein {version}",
        )

    csproj = read_text(
        report,
        "packages",
        ROOT / "bindings" / "dotnet" / "src" / "VinaryTree.Libdictenstein" / "VinaryTree.Libdictenstein.csproj",
    )
    if csproj is not None:
        expect(
            f"<PackageId>{packages['nuget']}</PackageId>" in csproj,
            f"csproj PackageId != {packages['nuget']}",
        )
        expect(f"<Version>{version}</Version>" in csproj, f"csproj Version != {version}")

    go_mod = read_text(report, "packages", ROOT / "bindings" / "go" / "go.mod")
    if go_mod is not None:
        expect(
            f"module {packages['goModule']}" in go_mod,
            f"go.mod module != {packages['goModule']}",
        )
        expect(
            f"github.com/vinary-tree/liblevenshtein-rust/vinary-tree-interop/bindings/go v{interop_version}"
            in go_mod,
            f"go.mod must require the interop module at v{interop_version}",
        )

    gemspec = read_text(
        report, "packages", ROOT / "bindings" / "ruby" / "vinary-tree-libdictenstein.gemspec"
    )
    if gemspec is not None:
        expect(
            f'spec.name = "{packages["rubygems"]}"' in gemspec,
            f"gemspec name != {packages['rubygems']}",
        )
    ruby_version = read_text(
        report, "packages", ROOT / "bindings" / "ruby" / "lib" / "vinary_tree" / "libdictenstein" / "version.rb"
    )
    if ruby_version is not None:
        expect(
            f'VERSION = "{version}"' in ruby_version,
            f"ruby version.rb != {version}",
        )

    cabal = read_text(
        report, "packages", ROOT / "bindings" / "haskell" / "vinary-tree-libdictenstein.cabal"
    )
    if cabal is not None:
        expect(f"name: {packages['hackage']}" in cabal, f"cabal name != {packages['hackage']}")
        expect(f"version: {version}" in cabal, f"cabal version != {version}")

    dune_project = read_text(report, "packages", ROOT / "bindings" / "ocaml" / "dune-project")
    if dune_project is not None:
        expect(
            f"(name {packages['opam']})" in dune_project,
            f"dune-project package name != {packages['opam']}",
        )
    expect(
        (ROOT / "bindings" / "ocaml" / f"{packages['opam']}.opam").is_file(),
        f"generated opam file {packages['opam']}.opam is missing",
    )

    for fpm_name in ("fpm.toml", "fpm.publish.toml"):
        fpm_source = read_text(report, "packages", ROOT / "bindings" / "fortran" / fpm_name)
        if fpm_source is not None:
            fpm = tomllib.loads(fpm_source)
            expect(fpm["name"] == packages["fpm"], f"{fpm_name} name != {packages['fpm']}")
            expect(fpm["version"] == version, f"{fpm_name} version != {version}")
    fpm_publish_source = read_text(
        report, "packages", ROOT / "bindings" / "fortran" / "fpm.publish.toml"
    )
    if fpm_publish_source is not None:
        fpm_publish = tomllib.loads(fpm_publish_source)
        interop_dep = fpm_publish.get("dependencies", {}).get("vinary-tree-interop", {})
        expect(
            interop_dep.get("v") == interop_version
            and interop_dep.get("namespace") == model["organization"]["fpmNamespace"],
            f"fpm.publish.toml must depend on {model['organization']['fpmNamespace']}/vinary-tree-interop v{interop_version}",
        )

    rockspecs = sorted((ROOT / "bindings" / "lua").glob("*.rockspec"))
    expect(bool(rockspecs), "lua rockspec is missing")
    for rockspec_path in rockspecs:
        rockspec = read_text(report, "packages", rockspec_path)
        if rockspec is None:
            continue
        expect(
            f'package = "{packages["luarocks"]}"' in rockspec,
            f"{rockspec_path.name} package != {packages['luarocks']}",
        )
        rock_version = re.search(r'version = "([^"]+)"', rockspec)
        expect(
            rock_version is not None and rock_version.group(1).split("-")[0] == version,
            f"{rockspec_path.name} version base != {version}",
        )
        expect(
            rock_version is not None
            and rockspec_path.name == f"{packages['luarocks']}-{rock_version.group(1)}.rockspec",
            f"{rockspec_path.name} filename disagrees with its package/version fields",
        )

    package_swift = read_text(report, "packages", ROOT / "Package.swift")
    if package_swift is not None:
        expect(
            f'name: "{packages["swift"]}"' in package_swift,
            f"Package.swift name != {packages['swift']}",
        )
    expect(
        (ROOT / "bindings" / "swift" / "libdictenstein" / "Package.swift").is_file(),
        "nested Swift development package is missing",
    )

    pkgconfig = read_text(report, "packages", ROOT / "pkgconfig" / "libdictenstein.pc")
    if pkgconfig is not None:
        expect(f"Version: {version}" in pkgconfig, f"pkgconfig Version != {version}")
    cmake_version = read_text(
        report, "packages", ROOT / "cmake" / "libdictensteinConfigVersion.cmake"
    )
    if cmake_version is not None:
        expect(
            f'set(PACKAGE_VERSION "{version}")' in cmake_version,
            f"cmake PACKAGE_VERSION != {version}",
        )

    if clean:
        report.ok(
            "packages",
            "registry coordinates and versions agree with the model across all facade metadata",
        )


def check_sibling_pins(report: Report, model: dict) -> None:
    llev = model["siblingPins"]["liblevenshtein"]
    llattice = model["siblingPins"]["llattice"]
    clean = True

    def expect(condition: bool, message: str) -> None:
        nonlocal clean
        if not condition:
            report.fail("sibling-pins", message)
            clean = False

    release = read_text(
        report, "sibling-pins", ROOT / ".github" / "workflows" / "release-bindings.yml"
    )
    if release is not None:
        llev_branches = set(
            re.findall(r"--branch (v[0-9.]+) https://github\.com/vinary-tree/liblevenshtein-rust", release)
        )
        expect(
            llev_branches == {f"v{llev}"},
            f"release-bindings.yml liblevenshtein-rust clone pins {sorted(llev_branches)} != v{llev}",
        )
        llattice_branches = set(
            re.findall(r"--branch (v[0-9.]+) https://github\.com/vinary-tree/llattice", release)
        )
        expect(
            llattice_branches == {f"v{llattice}"},
            f"release-bindings.yml llattice clone pins {sorted(llattice_branches)} != v{llattice}",
        )
    go_mod = read_text(report, "sibling-pins", ROOT / "bindings" / "go" / "go.mod")
    if go_mod is not None:
        expect(
            f"github.com/vinary-tree/liblevenshtein-rust/bindings/go v{llev}" in go_mod,
            f"go.mod liblevenshtein module pin != v{llev}",
        )
    package_swift = read_text(report, "sibling-pins", ROOT / "Package.swift")
    if package_swift is not None:
        expect(
            f'from: "{llev}"' in package_swift,
            f"Package.swift liblevenshtein-rust pin != {llev}",
        )
    gradle = read_text(report, "sibling-pins", ROOT / "bindings" / "jvm" / "build.gradle.kts")
    if gradle is not None:
        expect(
            f'testImplementation("io.vinarytree:liblevenshtein:{llev}")' in gradle,
            f"build.gradle.kts test pin io.vinarytree:liblevenshtein != {llev}",
        )
    project_clj = read_text(report, "sibling-pins", ROOT / "bindings" / "clojure" / "project.clj")
    if project_clj is not None:
        expect(
            f'[io.vinarytree/liblevenshtein "{llev}"]' in project_clj,
            f"project.clj test pin io.vinarytree/liblevenshtein != {llev}",
        )
    if model["wasm"]["umbrellaVersion"] != llev:
        report.fail(
            "sibling-pins",
            f"wasm.umbrellaVersion {model['wasm']['umbrellaVersion']} != siblingPins.liblevenshtein {llev}",
        )
        clean = False
    if clean:
        report.ok(
            "sibling-pins",
            f"every liblevenshtein-rust reference pins {llev} and every llattice reference pins {llattice} "
            "(tag existence is ledger finding LDICT-B1)",
        )


def main() -> int:
    argument_parser = argparse.ArgumentParser(description=__doc__)
    argument_parser.add_argument(
        "--json", action="store_true", help="emit a machine-readable JSON report"
    )
    arguments = argument_parser.parse_args()

    report = Report()
    if not MODEL_PATH.is_file():
        report.fail("model", "bindings/api.json is missing")
        model = None
    else:
        model = json.loads(MODEL_PATH.read_text(encoding="utf-8"))

    if model is not None:
        required_keys = (
            "name", "crate", "organization", "interop", "packages", "packageVersion",
            "siblingPins", "abiVersion", "apiRevision", "cPrefix", "cHeader",
            "featureGates", "snapshot", "marshalling", "unitDomains", "valueDomain",
            "structs", "enums", "kinds", "capabilities", "backends", "producers",
            "cFunctions", "facades", "wasm",
        )
        missing_keys = [key for key in required_keys if key not in model]
        if missing_keys:
            report.fail("model", f"api.json lacks required keys: {missing_keys}")
        else:
            check_symbol_parity(report, model)
            check_constant_parity(report, model)
            check_facades(report, model)
            check_identity(report)
            check_interop_header(report, model)
            check_packages(report, model)
            check_sibling_pins(report, model)

    if arguments.json:
        print(
            json.dumps(
                {
                    "status": "fail" if report.failures else "pass",
                    "failures": report.failures,
                    "warnings": report.warnings,
                    "passes": report.passes,
                    "facades": report.facades,
                },
                indent=2,
                sort_keys=True,
            )
        )
    else:
        for line in report.passes:
            print(f"PASS {line}")
        for line in report.warnings:
            print(f"WARN {line}")
        if report.facades:
            print("facade symbol coverage (referenced / modeled):")
            for language in sorted(report.facades):
                stats = report.facades[language]
                if stats.get("mode") == "symbols":
                    print(f"  {language:<11} {stats['referenced']:>2} / {stats['modeled']}")
                else:
                    print(f"  {language:<11} mediated facade (no direct symbol imports)")
        for line in report.failures:
            print(f"FAIL {line}")
        verdict = "FAILED" if report.failures else "OK"
        print(
            f"binding contract: {verdict} "
            f"({len(report.failures)} failure(s), {len(report.warnings)} warning(s))"
        )
    return 1 if report.failures else 0


if __name__ == "__main__":
    sys.exit(main())
