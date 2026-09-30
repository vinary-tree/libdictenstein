"""Shared exact C-header/model signature parity for generated language ABIs."""

from __future__ import annotations

import re


class ModelError(ValueError):
    """The authoritative model or its canonical C header is inconsistent."""


RETURN_TYPES = {
    "void": None,
    "uint32_t": "uint32_t",
    "const char*": "const char*",
    "LdictStatus": "LdictStatus",
}
VALID_DIRECTIONS = {"input", "output", "input-output"}


def canonical_c_type(value: str) -> str:
    value = " ".join(value.split())
    return re.sub(r"\s*\*\s*", "*", value)


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
                parameters.append((match.group(2), canonical_c_type(match.group(1))))
        name = declaration.group("name")
        if name in parsed:
            raise ModelError(f"duplicate C header declaration {name}")
        parsed[name] = (canonical_c_type(declaration.group("return")), parameters)
    return parsed


def modeled_signatures(model: dict) -> dict[str, tuple[str, list[tuple[str, str]]]]:
    signatures: dict[str, tuple[str, list[tuple[str, str]]]] = {}
    prefix = model.get("cPrefix", "")
    for function in model.get("cFunctions", []):
        name = function.get("name")
        if not isinstance(name, str) or not name.startswith(prefix):
            raise ModelError(f"invalid modeled C function name {name!r}")
        if name in signatures:
            raise ModelError(f"duplicate modeled C function {name}")
        return_type = function.get("returnType")
        if return_type not in RETURN_TYPES:
            raise ModelError(f"{name}: unsupported returnType {return_type!r}")
        parameters = function.get("parameters")
        if not isinstance(parameters, list):
            raise ModelError(f"{name}: parameters must be an array")
        seen: set[str] = set()
        modeled: list[tuple[str, str]] = []
        for parameter in parameters:
            parameter_name = parameter.get("name")
            c_type = parameter.get("cType")
            direction = parameter.get("direction")
            ownership = parameter.get("ownership")
            if not isinstance(parameter_name, str) or not parameter_name:
                raise ModelError(f"{name}: parameter has no valid name")
            if parameter_name in seen:
                raise ModelError(f"{name}: duplicate parameter {parameter_name}")
            seen.add(parameter_name)
            if not isinstance(c_type, str) or not c_type:
                raise ModelError(f"{name}.{parameter_name}: cType is required")
            if direction not in VALID_DIRECTIONS:
                raise ModelError(
                    f"{name}.{parameter_name}: invalid direction {direction!r}"
                )
            if not isinstance(ownership, str) or not ownership:
                raise ModelError(f"{name}.{parameter_name}: ownership is required")
            modeled.append((parameter_name, canonical_c_type(c_type)))
        signatures[name] = (canonical_c_type(return_type), modeled)
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
