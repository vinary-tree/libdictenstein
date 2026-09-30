#!/usr/bin/env python3
"""Generate Raku NativeCall ABI regions from bindings/api.json.

The model is the generation source. The independently maintained public C
header is an exact parity oracle; any symbol, signature, constant, layout, or
alias drift fails before bytes are emitted. Only the delimited low-level ABI
regions are rewritten; the idiomatic collection facade remains handwritten.
"""

from __future__ import annotations

import argparse
import copy
import json
import re
import sys
from pathlib import Path

from binding_abi_model import ModelError, canonical_c_type, validate_header_parity

ROOT = Path(__file__).resolve().parents[1]
MODEL_PATH = ROOT / "bindings/api.json"
HEADER_PATH = ROOT / "include/libdictenstein.h"
RAKU_PATH = ROOT / "bindings/raku/lib/Libdictenstein.rakumod"
META_PATH = ROOT / "bindings/raku/META6.json"
INVENTORY_PATH = ROOT / "bindings/generated/raku-abi-capabilities.tsv"

REGIONS = {
    "CONSTANTS": (
        "# BEGIN GENERATED RAKU ABI CONSTANTS",
        "# END GENERATED RAKU ABI CONSTANTS",
    ),
    "LAYOUTS": (
        "# BEGIN GENERATED RAKU ABI LAYOUTS",
        "# END GENERATED RAKU ABI LAYOUTS",
    ),
    "CALLS": ("# BEGIN GENERATED RAKU ABI CALLS", "# END GENERATED RAKU ABI CALLS"),
}
LOCAL_LAYOUTS = {
    "LdictOptionalU64": [
        ("value", "u64"),
        ("has_value", "u8"),
        ("reserved", "[u8; 7]"),
    ],
    "LdictTextEntry": [
        ("data", "*const u8"),
        ("len", "usize"),
        ("value", "LdictOptionalU64"),
    ],
    "LdictU64Entry": [
        ("data", "*const u64"),
        ("len", "usize"),
        ("value", "LdictOptionalU64"),
    ],
}
HEADER_FIELD_TYPES = {
    "u64": "uint64_t",
    "u8": "uint8_t",
    "usize": "size_t",
    "*const u8": "const uint8_t*",
    "*const u64": "const uint64_t*",
    "LdictOptionalU64": "LdictOptionalU64",
}
RETURN_TYPES = {
    "LdictStatus": "int32",
    "uint32_t": "uint32",
    "const char*": "Str",
    "void": None,
}
SCALARS = {
    "uint32_t": "uint32",
    "uint64_t": "uint64",
    "uint8_t": "uint8",
    "size_t": "size_t",
}


def header_without_comments(header: str) -> str:
    return re.sub(r"/\*.*?\*/", "", header, flags=re.DOTALL)


def validate_constants(model: dict, header: str) -> None:
    for label, number in (
        ("LDICT_ABI_VERSION", model["abiVersion"]),
        ("LDICT_API_REVISION", model["apiRevision"]),
    ):
        if not re.search(rf"^#define {label} {number}u$", header, re.MULTILINE):
            raise ModelError(f"C header {label} differs from the model")
    actual_kinds = {
        name: int(value)
        for name, value in re.findall(
            r"^#define LDICT_KIND_([A-Z0-9_]+) (\d+)u$",
            header,
            re.MULTILINE,
        )
    }
    if actual_kinds != model["kinds"]["values"]:
        raise ModelError(f"C header dictionary kinds differ: {actual_kinds}")
    actual_capabilities = {
        name: int(bit)
        for name, bit in re.findall(
            r"^#define LDICT_CAP_([A-Z0-9_]+) \(UINT64_C\(1\) << (\d+)\)$",
            header,
            re.MULTILINE,
        )
    }
    if actual_capabilities != model["capabilities"]["bits"]:
        raise ModelError(f"C header capability bits differ: {actual_capabilities}")
    for enum in model["enums"].values():
        ctype, prefix = enum["cType"], enum["cPrefix"]
        block = re.search(
            rf"typedef enum {ctype} \{{(.*?)\}} {ctype};", header, re.DOTALL
        )
        if block is None:
            raise ModelError(f"C header enum {ctype} missing")
        actual = {
            name: int(value)
            for name, value in re.findall(
                rf"{prefix}([A-Z0-9_]+)\s*=\s*(\d+)", block.group(1)
            )
        }
        if actual != enum["values"]:
            raise ModelError(f"C header enum {ctype} differs from the model: {actual}")


def expected_field_declarations(fields: list[dict]) -> list[str]:
    declarations: list[str] = []
    for field in fields:
        name, type_name = field["name"], field["type"]
        array = re.fullmatch(r"\[u8; (\d+)\]", type_name)
        if array:
            declarations.append(f"uint8_t {name}[{array.group(1)}]")
        else:
            if type_name not in HEADER_FIELD_TYPES:
                raise ModelError(f"unsupported modeled C layout type {type_name}")
            declarations.append(f"{HEADER_FIELD_TYPES[type_name]} {name}")
    return declarations


def validate_layouts(model: dict, header: str) -> None:
    source = header_without_comments(header)
    for name, expected in LOCAL_LAYOUTS.items():
        spec = model["structs"].get(name)
        if (
            spec is None
            or [(field["name"], field["type"]) for field in spec.get("fields", [])]
            != expected
        ):
            raise ModelError(
                f"{name}: modeled local layout differs from supported NativeCall layout"
            )
        block = re.search(
            rf"typedef struct {name} \{{(.*?)\}} {name};", source, re.DOTALL
        )
        if block is None:
            raise ModelError(f"C header struct {name} missing")
        actual = [
            canonical_c_type(part).strip()
            for part in block.group(1).split(";")
            if part.strip()
        ]
        modeled = [
            canonical_c_type(part)
            for part in expected_field_declarations(spec["fields"])
        ]
        if actual != modeled:
            raise ModelError(f"C header struct {name} differs: {actual} != {modeled}")
    aliases = {
        name: spec["alias"]
        for name, spec in model["structs"].items()
        if "alias" in spec
    }
    if set(model["structs"]) != set(LOCAL_LAYOUTS) | set(aliases):
        raise ModelError("modeled struct lacks a generated local layout or C alias")
    actual_aliases = {
        name: target
        for target, name in re.findall(
            r"typedef\s+(Vt[A-Za-z0-9_]+)\s+(Ldict[A-Za-z0-9_]+);", source
        )
    }
    if aliases != actual_aliases:
        raise ModelError(
            f"C header alias drift: modeled={aliases}, header={actual_aliases}"
        )


def nativecall_parameter(parameter: dict) -> str:
    c_type = canonical_c_type(parameter["cType"])
    name = parameter["name"]
    if c_type in SCALARS:
        return SCALARS[c_type]
    if c_type in {"uint8_t*", "uint32_t*", "uint64_t*", "size_t*"}:
        if name in {"out_bytes", "out_data"}:
            return "Pointer"
        return SCALARS[c_type[:-1]] + " is rw"
    if c_type == "LdictOptionalU64":
        return "OptionalValue"
    if c_type == "LdictOptionalU64*":
        return "OptionalValue"
    if c_type == "VtResource*":
        return "RawResource"
    if c_type in {"LdictDictionary**", "LdictEntryCursor**", "LdictByteEntryCursor**"}:
        return "Pointer is rw"
    pointer_types = {
        "LdictDictionary*",
        "const LdictDictionary*",
        "LdictEntryCursor*",
        "LdictByteEntryCursor*",
        "const uint8_t*",
        "const uint64_t*",
        "const LdictTextEntry*",
        "const LdictU64Entry*",
        "const LdictEntryBatchLimits*",
        "LdictEntryBatch*",
        "LdictEntriesInfo*",
        "const LdictByteEntryBatchLimits*",
        "LdictByteEntryBatch*",
        "LdictByteEntriesInfo*",
        "LdictEntryReducer",
        "LdictByteEntryReducer",
        "void*",
    }
    if c_type in pointer_types:
        return "Pointer"
    raise ModelError(f"no Raku NativeCall mapping for {c_type} ({name})")


def render_constants(model: dict) -> str:
    begin, end = REGIONS["CONSTANTS"]
    lines = [
        begin,
        "# Generated by scripts/generate-raku-abi.py from bindings/api.json.",
        f"our constant ABI-VERSION is export = {model['abiVersion']};",
        f"our constant API-REVISION is export = {model['apiRevision']};",
        "",
    ]
    for title, values, prefix in (
        ("Status", model["enums"]["status"]["values"], "STATUS-"),
        ("DictionaryKind", model["kinds"]["values"], ""),
        ("AlgebraOperation", model["enums"]["algebraOperation"]["values"], "ALGEBRA-"),
        ("ValueMerge", model["enums"]["valueMerge"]["values"], "VALUE-MERGE-"),
    ):
        lines.append(f"our enum {title} is export (")
        for name, value in values.items():
            lines.append(f"    {prefix}{name.replace('_', '-')} => {value},")
        lines.append(");")
        lines.append("")
    for alias, target in model["kinds"].get("compatibilityAliases", {}).items():
        if target not in model["kinds"]["values"]:
            raise ModelError(f"unknown dictionary-kind alias {alias}")
        lines.append(
            f"our constant {alias.replace('_', '-')} is export = {target.replace('_', '-')};"
        )
    if model["kinds"].get("compatibilityAliases"):
        lines.append("")
    for name, bit in model["capabilities"]["bits"].items():
        lines.append(
            f"our constant CAP-{name.replace('_', '-')} is export = 1 +< {bit};"
        )
    lines.append(end)
    return "\n".join(lines)


def render_layouts(model: dict) -> str:
    validate = model["structs"]["LdictOptionalU64"]["fields"]
    count_match = re.fullmatch(r"\[u8; (\d+)\]", validate[2]["type"])
    if count_match is None:
        raise ModelError("OptionalU64 reserved byte array is invalid")
    count = int(count_match.group(1))
    begin, end = REGIONS["LAYOUTS"]
    lines = [
        begin,
        "# Generated by scripts/generate-raku-abi.py from bindings/api.json.",
        "class OptionalValue is repr('CStruct') is export {",
        "    has uint64 $.value;",
        "    has uint8 $.has-value;",
    ]
    lines += [f"    has uint8 $.reserved{i};" for i in range(count)]
    lines += [
        "}",
        "",
        "role EntryDescriptor {",
        "    has Pointer $.data;",
        "    has size_t $.len;",
        "    has uint64 $.mapped-value;",
        "    has uint8 $.has-value;",
    ]
    lines += [f"    has uint8 $.reserved{i};" for i in range(count)]
    lines += [
        "",
        "    submethod BUILD(",
        "        Pointer:D :$data!, Int:D :$len!, Int:D :$mapped-value!,",
        "        Int:D :$has-value!,",
    ]
    lines += [
        f"        Int:D :$reserved{i} = 0{',' if i < count - 1 else ''}"
        for i in range(count)
    ]
    lines += [
        "    ) {",
        "        $!data := $data;",
        "        $!len = $len;",
        "        $!mapped-value = $mapped-value;",
        "        $!has-value = $has-value;",
    ]
    lines += [f"        $!reserved{i} = $reserved{i};" for i in range(count)]
    lines += [
        "    }",
        "}",
        "",
        "class TextEntry is repr('CStruct') does EntryDescriptor is export { }",
        "class U64Entry is repr('CStruct') does EntryDescriptor is export { }",
        end,
    ]
    return "\n".join(lines)


def render_calls(model: dict) -> str:
    begin, end = REGIONS["CALLS"]
    lines = [
        begin,
        "# Generated by scripts/generate-raku-abi.py from bindings/api.json.",
    ]
    for function in model["cFunctions"]:
        name = function["name"]
        raku_name = name.replace("_", "-")
        parameters = ", ".join(nativecall_parameter(p) for p in function["parameters"])
        return_type = RETURN_TYPES[function["returnType"]]
        signature = f"sub {raku_name}({parameters}"
        if return_type:
            signature += f"{' ' if parameters else ''}--> {return_type}"
        signature += ")"
        lines.append(signature)
        lines.append(f"    is native(&native-library) is symbol('{name}') {{ * }}")
    lines.append(end)
    return "\n".join(lines)


def replace_region(source: str, name: str, replacement: str) -> str:
    begin, end = REGIONS[name]
    pattern = re.compile(re.escape(begin) + r".*?" + re.escape(end), re.DOTALL)
    if len(pattern.findall(source)) != 1:
        raise ModelError(f"expected exactly one Raku generated region {name}")
    return pattern.sub(lambda _: replacement, source)


def facade_source(source: str) -> str:
    for name in REGIONS:
        source = replace_region(source, name, "")
    return source


def validate_surface(model: dict, source: str) -> dict[str, str]:
    facade = model["facades"]["raku"]
    if facade["importLayers"] != [str(RAKU_PATH.relative_to(ROOT))]:
        raise ModelError("Raku import-layer path differs from the canonical facade")
    if (
        facade["package"] != "raku"
        or model["packages"]["raku"] != json.loads(META_PATH.read_text())["name"]
    ):
        raise ModelError("Raku package coordinate differs between model and META6.json")
    if facade["generator"] != str(Path(__file__).resolve().relative_to(ROOT)):
        raise ModelError("Raku generator path differs from the model")
    if facade["abiInventory"] != str(INVENTORY_PATH.relative_to(ROOT)):
        raise ModelError("Raku ABI inventory path differs from the model")
    outside = facade_source(source)
    if re.search(r"^\s*sub\s+ldict-[a-z0-9-]+\s*\(", outside, re.MULTILINE):
        raise ModelError(
            "handwritten ldict NativeCall declaration outside generated region"
        )
    names = {f["name"] for f in model["cFunctions"]}
    referenced = set(
        re.findall(r"(?<![A-Za-z0-9-])(ldict-[a-z0-9-]+)(?![A-Za-z0-9-])", outside)
    )
    unknown = referenced - {name.replace("_", "-") for name in names}
    if unknown:
        raise ModelError(
            f"Raku facade calls unmodeled native symbol: {sorted(unknown)}"
        )
    used = {
        name
        for name in names
        if re.search(
            r"(?<![A-Za-z0-9-])"
            + re.escape(name.replace("_", "-"))
            + r"(?![A-Za-z0-9-])",
            outside,
        )
    }
    reasons = facade["rawOnlyReasons"]
    raw_only = facade["rawOnly"]
    if set(raw_only) != names - used:
        raise ModelError(
            f"Raku raw-only inventory differs from unwrapped ABI symbols: missing={sorted(names - used - set(raw_only))}, stale={sorted(set(raw_only) - (names - used))}"
        )
    if any(not reasons.get(key) for key in raw_only.values()):
        raise ModelError("Raku raw-only symbol lacks a nonempty reason")
    if set(reasons) != set(raw_only.values()):
        raise ModelError("Raku raw-only reason keys do not match used categories")
    return {name: ("facade" if name in used else raw_only[name]) for name in names}


def render_inventory(model: dict, surface: dict[str, str]) -> str:
    facade = model["facades"]["raku"]
    lines = [
        "# Generated by scripts/generate-raku-abi.py; edit bindings/api.json instead.",
        f"# package={model['packages']['raku']} module={RAKU_PATH.relative_to(ROOT)}",
        "symbol\tgroup\tc_signature\traku_signature\tsurface\treason",
    ]
    for function in model["cFunctions"]:
        name = function["name"]
        c_sig = (
            function["returnType"]
            + "("
            + ", ".join(p["cType"] for p in function["parameters"])
            + ")"
        )
        raku_sig = (
            (RETURN_TYPES[function["returnType"]] or "void")
            + "("
            + ", ".join(nativecall_parameter(p) for p in function["parameters"])
            + ")"
        )
        reason_key = surface[name]
        state = "facade" if reason_key == "facade" else f"raw-only:{reason_key}"
        reason = "-" if reason_key == "facade" else facade["rawOnlyReasons"][reason_key]
        lines.append(
            f"{name}\t{function['group']}\t{c_sig}\t{raku_sig}\t{state}\t{reason}"
        )
    return "\n".join(lines) + "\n"


def render(model: dict, header: str, source: str) -> tuple[str, str]:
    validate_header_parity(model, header)
    validate_constants(model, header)
    validate_layouts(model, header)
    surface = validate_surface(model, source)
    result = source
    for name, content in (
        ("CONSTANTS", render_constants(model)),
        ("LAYOUTS", render_layouts(model)),
        ("CALLS", render_calls(model)),
    ):
        result = replace_region(result, name, content)
    return result, render_inventory(model, surface)


def self_test(model: dict, header: str, source: str) -> None:
    render(model, header, source)
    duplicate = copy.deepcopy(model)
    duplicate["cFunctions"].append(copy.deepcopy(duplicate["cFunctions"][0]))
    controls = [(duplicate, header, source, "duplicate symbol")]
    bad_enum = copy.deepcopy(model)
    bad_enum["enums"]["status"]["values"]["OK"] = 900
    controls.append((bad_enum, header, source, "enum drift"))
    bad_layout = copy.deepcopy(model)
    bad_layout["structs"]["LdictOptionalU64"]["fields"][0]["type"] = "u8"
    controls.append((bad_layout, header, source, "layout drift"))
    bad_omission = copy.deepcopy(model)
    bad_omission["facades"]["raku"]["rawOnly"].pop(
        next(iter(bad_omission["facades"]["raku"]["rawOnly"]))
    )
    controls.append((bad_omission, header, source, "missing omission"))
    stale_reason = copy.deepcopy(model)
    stale_reason["facades"]["raku"]["rawOnlyReasons"]["unused"] = "obsolete"
    controls.append((stale_reason, header, source, "orphan omission reason"))
    bad_kind = copy.deepcopy(model)
    bad_kind["kinds"]["values"]["DYNAMIC_DAWG"] = 900
    controls.append((bad_kind, header, source, "kind drift"))
    bad_capability = copy.deepcopy(model)
    bad_capability["capabilities"]["bits"]["LOOKUP"] = 900
    controls.append((bad_capability, header, source, "capability drift"))
    bad_alias = copy.deepcopy(model)
    bad_alias["structs"].pop("LdictByteEntry")
    controls.append((bad_alias, header, source, "byte-value alias omission"))
    controls.append(
        (
            model,
            header.replace(
                "LDICT_API uint32_t ldict_api_revision",
                "LDICT_API uint64_t ldict_api_revision",
            ),
            source,
            "header signature drift",
        )
    )
    controls.append(
        (
            model,
            header,
            source + "\nsub ldict-fake(--> int32) is native { * }\n",
            "handwritten symbol",
        )
    )
    controls.append(
        (
            model,
            header,
            source + "\nmy $ignored = ldict-fake(Pointer);\n",
            "unmodeled facade reference",
        )
    )
    for bad_model, bad_header, bad_source, label in controls:
        try:
            render(bad_model, bad_header, bad_source)
        except ModelError:
            continue
        raise ModelError(f"negative control accepted {label}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument(
        "--check", action="store_true", help="check generated bytes without writing"
    )
    mode.add_argument(
        "--write", action="store_true", help="regenerate Raku ABI regions and inventory"
    )
    mode.add_argument(
        "--self-test",
        action="store_true",
        help="exercise negative drift controls in memory",
    )
    args = parser.parse_args()
    model = json.loads(MODEL_PATH.read_text())
    header = HEADER_PATH.read_text()
    source = RAKU_PATH.read_text()
    try:
        if args.self_test:
            self_test(model, header, source)
            print("Raku ABI negative drift controls passed")
            return 0
        generated, inventory = render(model, header, source)
    except (ModelError, KeyError, TypeError, ValueError) as error:
        print(f"Raku ABI model error: {error}", file=sys.stderr)
        return 1
    if args.write:
        RAKU_PATH.write_text(generated)
        INVENTORY_PATH.write_text(inventory)
        print(f"generated Raku ABI: {len(model['cFunctions'])} exact functions")
        return 0
    stale = []
    if generated != source:
        stale.append(str(RAKU_PATH.relative_to(ROOT)))
    if not INVENTORY_PATH.exists() or INVENTORY_PATH.read_text() != inventory:
        stale.append(str(INVENTORY_PATH.relative_to(ROOT)))
    if stale:
        print("stale Raku ABI artifacts: " + ", ".join(stale), file=sys.stderr)
        return 1
    print(f"Raku ABI generation is current: {len(model['cFunctions'])} exact functions")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
