#!/usr/bin/env python3
"""Generate and verify libdictenstein's Julia ABI layer from bindings/api.json.

The JSON model is authoritative.  The C header is an independently maintained
public artifact used as an exact parity oracle, never as a generation source.
The generator owns three delimited regions in the Julia module and one tabular
inventory consumed by tests and reviewers.
"""

from __future__ import annotations

import argparse
import copy
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODEL_PATH = ROOT / "bindings" / "api.json"
JULIA_PATH = (
    ROOT / "bindings" / "julia" / "Libdictenstein" / "src" / "Libdictenstein.jl"
)
INVENTORY_PATH = ROOT / "bindings" / "generated" / "julia-abi-capabilities.tsv"

CONSTANTS_BEGIN = "# BEGIN GENERATED JULIA ABI CONSTANTS"
CONSTANTS_END = "# END GENERATED JULIA ABI CONSTANTS"
LAYOUTS_BEGIN = "# BEGIN GENERATED JULIA ABI LAYOUTS"
LAYOUTS_END = "# END GENERATED JULIA ABI LAYOUTS"
CALLS_BEGIN = "# BEGIN GENERATED JULIA ABI CALLS"
CALLS_END = "# END GENERATED JULIA ABI CALLS"

VALID_DIRECTIONS = {"input", "output", "input-output"}
RETURN_TYPES = {
    "void": "Cvoid",
    "uint32_t": "UInt32",
    "const char*": "Cstring",
    "LdictStatus": "Cint",
}


class ModelError(ValueError):
    """The authoritative model or a generated consumer is inconsistent."""


def canonical_c_type(value: str) -> str:
    value = " ".join(value.split())
    value = re.sub(r"\s*\*\s*", "*", value)
    return value


def parse_header_signatures(
    source: str,
) -> dict[str, tuple[str, list[tuple[str, str]]]]:
    source = re.sub(r"/\*.*?\*/", "", source, flags=re.DOTALL)
    source = re.sub(r"//[^\n]*", "", source)
    declarations = re.finditer(
        r"^[ \t]*LDICT_API[ \t]+"
        r"(?P<return>(?:const[ \t]+)?[A-Za-z_][A-Za-z0-9_]*(?:[ \t]*\*)?)[ \t]+"
        r"(?P<name>ldict_[a-z0-9_]+)\s*\((?P<parameters>.*?)\)\s*;",
        source,
        flags=re.DOTALL | re.MULTILINE,
    )
    parsed: dict[str, tuple[str, list[tuple[str, str]]]] = {}
    for declaration in declarations:
        raw_parameters = declaration.group("parameters").strip()
        parameters: list[tuple[str, str]] = []
        if raw_parameters and raw_parameters != "void":
            for raw_parameter in raw_parameters.split(","):
                normalized = " ".join(raw_parameter.split())
                match = re.fullmatch(r"(.+?)([A-Za-z_][A-Za-z0-9_]*)", normalized)
                if match is None:
                    raise ModelError(f"cannot parse C parameter {raw_parameter!r}")
                c_type = canonical_c_type(match.group(1))
                parameters.append((match.group(2), c_type))
        name = declaration.group("name")
        parsed[name] = (canonical_c_type(declaration.group("return")), parameters)
    return parsed


def modeled_signatures(model: dict) -> dict[str, tuple[str, list[tuple[str, str]]]]:
    signatures: dict[str, tuple[str, list[tuple[str, str]]]] = {}
    for function in model.get("cFunctions", []):
        name = function.get("name")
        if not isinstance(name, str) or not name.startswith(model.get("cPrefix", "")):
            raise ModelError(f"invalid modeled C function name {name!r}")
        if name in signatures:
            raise ModelError(f"duplicate modeled C function {name}")
        return_type = function.get("returnType")
        if return_type not in RETURN_TYPES:
            raise ModelError(f"{name}: unsupported returnType {return_type!r}")
        parameters = function.get("parameters")
        if not isinstance(parameters, list):
            raise ModelError(f"{name}: parameters must be an array")
        seen_parameters: set[str] = set()
        modeled_parameters: list[tuple[str, str]] = []
        for parameter in parameters:
            parameter_name = parameter.get("name")
            c_type = parameter.get("cType")
            direction = parameter.get("direction")
            ownership = parameter.get("ownership")
            if not isinstance(parameter_name, str) or not parameter_name:
                raise ModelError(f"{name}: parameter has no valid name")
            if parameter_name in seen_parameters:
                raise ModelError(f"{name}: duplicate parameter {parameter_name}")
            seen_parameters.add(parameter_name)
            if not isinstance(c_type, str) or not c_type:
                raise ModelError(f"{name}.{parameter_name}: cType is required")
            if direction not in VALID_DIRECTIONS:
                raise ModelError(
                    f"{name}.{parameter_name}: invalid direction {direction!r}"
                )
            if not isinstance(ownership, str) or not ownership:
                raise ModelError(f"{name}.{parameter_name}: ownership is required")
            modeled_parameters.append((parameter_name, canonical_c_type(c_type)))
        signatures[name] = (canonical_c_type(return_type), modeled_parameters)
    if not signatures:
        raise ModelError("cFunctions must not be empty")
    return signatures


def validate_header_parity(model: dict, header_source: str) -> None:
    expected = modeled_signatures(model)
    actual = parse_header_signatures(header_source)
    if expected.keys() != actual.keys():
        missing = sorted(expected.keys() - actual.keys())
        extra = sorted(actual.keys() - expected.keys())
        raise ModelError(
            f"C header symbol drift: missing declarations={missing}, unmodeled={extra}"
        )
    mismatches = [
        f"{name}: model={expected[name]!r}, header={actual[name]!r}"
        for name in expected
        if expected[name] != actual[name]
    ]
    if mismatches:
        raise ModelError("C header signature drift:\n" + "\n".join(mismatches))


def julia_parameter_type(parameter: dict) -> str:
    c_type = canonical_c_type(parameter["cType"])
    direction = parameter["direction"]
    name = parameter["name"]
    if c_type in {"LdictDictionary*", "const LdictDictionary*", "LdictEntryCursor*"}:
        return "Ptr{Cvoid}"
    if c_type in {"LdictDictionary**", "LdictEntryCursor**"}:
        return "Ref{Ptr{Cvoid}}"
    if c_type == "uint32_t":
        return "UInt32"
    if c_type == "uint64_t":
        return "UInt64"
    if c_type == "uint8_t":
        return "UInt8"
    if c_type == "size_t":
        return "Csize_t"
    if c_type == "const uint8_t*":
        return "Ptr{UInt8}"
    if c_type == "const uint64_t*":
        return "Ptr{UInt64}"
    if c_type == "uint8_t*":
        return "Ptr{UInt8}" if name == "out_data" else "Ref{UInt8}"
    if c_type == "uint32_t*":
        return "Ref{UInt32}"
    if c_type == "uint64_t*":
        return "Ref{UInt64}"
    if c_type == "size_t*":
        return "Ref{Csize_t}"
    if c_type == "LdictOptionalU64":
        return "OptionalU64"
    if c_type == "LdictOptionalU64*":
        return "Ref{OptionalU64}"
    if c_type == "const LdictTextEntry*":
        return "Ptr{TextEntry}"
    if c_type == "const LdictU64Entry*":
        return "Ptr{U64Entry}"
    if c_type == "VtResource*":
        return "Ref{VTI.VtResourceRaw}"
    if c_type == "const LdictEntryBatchLimits*":
        return "Ref{VTI.BatchLimits}"
    if c_type == "LdictEntryBatch*":
        return "Ref{VTI.VtDictionaryEntryBatchView}"
    if c_type == "LdictEntriesInfo*":
        return "Ref{VTI.VtDictionaryEntriesInfo}"
    if c_type in {"LdictEntryReducer", "void*"}:
        return "Ptr{Cvoid}"
    raise ModelError(
        f"no Julia mapping for {c_type!r} ({direction} parameter {name!r})"
    )


def render_constants(model: dict) -> str:
    lines = [
        CONSTANTS_BEGIN,
        "# Generated by scripts/generate-julia-abi.py from bindings/api.json.",
        f"const ABI_VERSION = UInt32({model['abiVersion']})",
        f"const API_REVISION = UInt32({model['apiRevision']})",
    ]
    for name in model["unitDomains"]:
        lines.append(f"const UNIT_{name} = VTI.UNIT_{name}")
    lines.extend(["", "@enum Status::Cint begin"])
    for name, value in model["enums"]["status"]["values"].items():
        lines.append(f"    STATUS_{name} = {value}")
    lines.extend(["end", "", "@enum DictionaryKind::UInt32 begin"])
    for name, value in model["kinds"]["values"].items():
        lines.append(f"    KIND_{name} = {value}")
    lines.append("end")
    for alias, target in model["kinds"].get("juliaAliases", {}).items():
        if target not in model["kinds"]["values"]:
            raise ModelError(
                f"Julia dictionary-kind alias {alias} has unknown target {target}"
            )
        lines.append(f"const KIND_{alias} = KIND_{target}")
    lines.extend(["", "@enum AlgebraOperation::UInt32 begin"])
    for name, value in model["enums"]["algebraOperation"]["values"].items():
        lines.append(f"    ALGEBRA_{name} = {value}")
    lines.extend(["end", "", "@enum ValueMerge::UInt32 begin"])
    for name, value in model["enums"]["valueMerge"]["values"].items():
        lines.append(f"    VALUE_MERGE_{name} = {value}")
    lines.extend(["end", ""])
    for name, bit in model["capabilities"]["bits"].items():
        lines.append(f"const CAP_{name} = UInt64(1) << {bit}")
    lines.append(CONSTANTS_END)
    return "\n".join(lines)


def render_layouts(model: dict) -> str:
    structs = model["structs"]
    expected_local = {
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
    for name, expected_fields in expected_local.items():
        actual = [(field["name"], field["type"]) for field in structs[name]["fields"]]
        if actual != expected_fields:
            raise ModelError(f"{name} layout is unsupported or drifted: {actual!r}")
    lines = [
        LAYOUTS_BEGIN,
        "# Generated by scripts/generate-julia-abi.py from bindings/api.json.",
        "struct OptionalU64",
        "    value::UInt64",
        "    has_value::UInt8",
        "    reserved::NTuple{7,UInt8}",
        "end",
        "",
        "struct TextEntry",
        "    data::Ptr{UInt8}",
        "    len::Csize_t",
        "    value::OptionalU64",
        "end",
        "",
        "struct U64Entry",
        "    data::Ptr{UInt64}",
        "    len::Csize_t",
        "    value::OptionalU64",
        "end",
        "",
        "const LdictEntry = VTI.VtDictionaryEntryRaw",
        "const LdictEntryBatchLimits = VTI.BatchLimits",
        "const LdictEntryBatch = VTI.VtDictionaryEntryBatchView",
        "const LdictEntriesInfo = VTI.VtDictionaryEntriesInfo",
        LAYOUTS_END,
    ]
    return "\n".join(lines)


def render_calls(model: dict) -> str:
    lines = [
        CALLS_BEGIN,
        "# Generated by scripts/generate-julia-abi.py from bindings/api.json.",
    ]
    for function in model["cFunctions"]:
        name = function["name"]
        wrapper = f"abi_{name}"
        parameters = function["parameters"]
        names = [parameter["name"] for parameter in parameters]
        julia_types = [julia_parameter_type(parameter) for parameter in parameters]
        return_type = RETURN_TYPES[function["returnType"]]
        arguments = ", ".join(names)
        tuple_type = (
            "()"
            if not julia_types
            else "("
            + ", ".join(julia_types)
            + ("," if len(julia_types) == 1 else "")
            + ")"
        )
        prefix = f"@inline function {wrapper}({arguments})"
        lines.append(prefix)
        call_arguments = "" if not names else ", " + ", ".join(names)
        lines.append(
            f"    ccall(native(:{name}), {return_type}, {tuple_type}{call_arguments})"
        )
        lines.append("end")
        lines.append("")
    lines.append(CALLS_END)
    return "\n".join(lines)


def replace_region(source: str, begin: str, end: str, replacement: str) -> str:
    pattern = re.compile(re.escape(begin) + r".*?" + re.escape(end), re.DOTALL)
    if len(pattern.findall(source)) != 1:
        raise ModelError(f"expected exactly one generated region {begin!r}")
    return pattern.sub(replacement, source)


def render_julia(model: dict, source: str) -> str:
    source = replace_region(
        source, CONSTANTS_BEGIN, CONSTANTS_END, render_constants(model)
    )
    source = replace_region(source, LAYOUTS_BEGIN, LAYOUTS_END, render_layouts(model))
    return replace_region(source, CALLS_BEGIN, CALLS_END, render_calls(model))


def source_outside_calls_region(source: str) -> str:
    pattern = re.compile(
        re.escape(CALLS_BEGIN) + r".*?" + re.escape(CALLS_END), re.DOTALL
    )
    if len(pattern.findall(source)) != 1:
        raise ModelError("generated Julia calls region is missing or duplicated")
    return pattern.sub("", source)


def validate_no_handwritten_calls(source: str) -> None:
    outside = source_outside_calls_region(source)
    if "ccall(" in outside:
        lines = [
            str(index)
            for index, line in enumerate(outside.splitlines(), 1)
            if "ccall(" in line
        ]
        raise ModelError(
            "handwritten ccall outside generated Julia ABI region at lines "
            + ", ".join(lines)
        )


def render_inventory(model: dict) -> str:
    header = [
        "symbol",
        "group",
        "feature",
        "return_type",
        "parameters",
        "julia_wrapper",
        "julia_return_type",
        "julia_parameter_types",
        "abi_version",
        "api_revision",
    ]
    rows = ["\t".join(header)]
    for function in model["cFunctions"]:
        parameters = ";".join(
            f"{parameter['name']}:{canonical_c_type(parameter['cType'])}:"
            f"{parameter['direction']}:{parameter['ownership']}"
            for parameter in function["parameters"]
        )
        julia_parameters = ";".join(
            julia_parameter_type(parameter) for parameter in function["parameters"]
        )
        rows.append(
            "\t".join(
                [
                    function["name"],
                    function["group"],
                    function.get("feature", ""),
                    function["returnType"],
                    parameters,
                    f"abi_{function['name']}",
                    RETURN_TYPES[function["returnType"]],
                    julia_parameters,
                    str(model["abiVersion"]),
                    str(model["apiRevision"]),
                ]
            )
        )
    return "\n".join(rows) + "\n"


def run_self_test(model: dict, header_source: str, julia_source: str) -> None:
    validate_header_parity(model, header_source)
    rendered = render_julia(model, julia_source)
    validate_no_handwritten_calls(rendered)

    duplicate = copy.deepcopy(model)
    duplicate["cFunctions"].append(copy.deepcopy(duplicate["cFunctions"][0]))
    try:
        modeled_signatures(duplicate)
    except ModelError:
        pass
    else:
        raise ModelError("negative control accepted a duplicate symbol")

    bad_direction = copy.deepcopy(model)
    bad_direction["cFunctions"][3]["parameters"][0]["direction"] = "sideways"
    try:
        modeled_signatures(bad_direction)
    except ModelError:
        pass
    else:
        raise ModelError("negative control accepted an invalid flow direction")

    drifted_header = header_source.replace(
        "uint32_t unit_domain,\n    LdictDictionary** out_dictionary);",
        "uint64_t unit_domain,\n    LdictDictionary** out_dictionary);",
        1,
    )
    try:
        validate_header_parity(model, drifted_header)
    except ModelError:
        pass
    else:
        raise ModelError("negative control accepted C header signature drift")

    injected = rendered.replace(
        "end # module Libdictenstein",
        "ccall(native(:ldict_abi_version), UInt32, ())\n\nend # module Libdictenstein",
    )
    try:
        validate_no_handwritten_calls(injected)
    except ModelError:
        pass
    else:
        raise ModelError("negative control accepted a handwritten Julia ccall")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true", help="verify generated files")
    mode.add_argument("--write", action="store_true", help="rewrite generated files")
    mode.add_argument(
        "--self-test", action="store_true", help="run generator negative controls"
    )
    arguments = parser.parse_args()

    model = json.loads(MODEL_PATH.read_text(encoding="utf-8"))
    header_source = (ROOT / model["cHeader"]).read_text(encoding="utf-8")
    julia_source = JULIA_PATH.read_text(encoding="utf-8")
    validate_header_parity(model, header_source)

    if arguments.self_test:
        run_self_test(model, header_source, julia_source)
        print("Julia ABI generator self-test passed")
        return 0

    rendered_julia = render_julia(model, julia_source)
    validate_no_handwritten_calls(rendered_julia)
    rendered_inventory = render_inventory(model)
    if arguments.write:
        JULIA_PATH.write_text(rendered_julia, encoding="utf-8")
        INVENTORY_PATH.parent.mkdir(parents=True, exist_ok=True)
        INVENTORY_PATH.write_text(rendered_inventory, encoding="utf-8")
        print(f"updated {JULIA_PATH.relative_to(ROOT)}")
        print(f"updated {INVENTORY_PATH.relative_to(ROOT)}")
        return 0

    failures = []
    if rendered_julia != julia_source:
        failures.append(str(JULIA_PATH.relative_to(ROOT)))
    if (
        not INVENTORY_PATH.is_file()
        or INVENTORY_PATH.read_text(encoding="utf-8") != rendered_inventory
    ):
        failures.append(str(INVENTORY_PATH.relative_to(ROOT)))
    if failures:
        print(
            "stale generated Julia ABI artifacts: " + ", ".join(failures),
            file=sys.stderr,
        )
        print("run scripts/generate-julia-abi.py --write", file=sys.stderr)
        return 1
    print(
        f"Julia ABI generation is current: {len(model['cFunctions'])} exact functions"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
