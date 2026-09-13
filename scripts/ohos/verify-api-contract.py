#!/usr/bin/env python3
"""Compare ArkTS imports, HAR wrappers/declarations and native N-API exports."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path


IMPORT_RE = re.compile(
    r"import\s*(?:type\s*)?\{(?P<body>[^}]*)\}\s*from\s*['\"]rustdesk-ohrs['\"]"
)
WRAPPER_RE = re.compile(r"export\s+(?:async\s+)?function\s+(\w+)\s*\(")
DECLARATION_RE = re.compile(r"export\s+declare\s+function\s+(\w+)\s*\(")
NAPI_RE = re.compile(
    r"#\[napi(?:\([^\]]*\))?\]\s*pub\s+(?:async\s+)?fn\s+(\w+)\s*\(",
    re.DOTALL,
)


def lower_camel(name: str) -> str:
    parts = name.split("_")
    return parts[0] + "".join(part[:1].upper() + part[1:] for part in parts[1:])


def arkts_imports(root: Path) -> set[str]:
    imported: set[str] = set()
    for source in root.rglob("*.ets"):
        text = source.read_text(encoding="utf-8")
        for match in IMPORT_RE.finditer(text):
            for item in match.group("body").split(","):
                name = item.strip().split()[0] if item.strip() else ""
                if name:
                    imported.add(name)
    return imported


def rust_napi_exports(source_root: Path) -> set[str]:
    exports: set[str] = set()
    for source in source_root.glob("*.rs"):
        text = source.read_text(encoding="utf-8")
        exports.update(lower_camel(name) for name in NAPI_RE.findall(text))
    return exports


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--arkts-root", type=Path)
    parser.add_argument("--allow-missing-native", action="store_true")
    parser.add_argument("--json-output", type=Path)
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parents[2]
    har_root = repo_root / "native" / "ohos_har"
    wrappers = set(WRAPPER_RE.findall((har_root / "package/index.ets").read_text(encoding="utf-8")))
    declarations = set(
        DECLARATION_RE.findall((har_root / "types/index.d.ts").read_text(encoding="utf-8"))
    )
    native = rust_napi_exports(har_root / "src")
    required = arkts_imports(args.arkts_root) if args.arkts_root else set()

    report = {
        "schema_version": 1,
        "counts": {
            "arkts_required": len(required),
            "package_wrappers": len(wrappers),
            "type_declarations": len(declarations),
            "native_exports": len(native),
        },
        "missing_wrapper_for_arkts": sorted(required - wrappers),
        "missing_declaration_for_arkts": sorted(required - declarations),
        "missing_native_for_arkts": sorted(required - native),
        "missing_native_for_wrapper": sorted(wrappers - native),
        "wrapper_without_declaration": sorted(wrappers - declarations),
        "declaration_without_wrapper": sorted(declarations - wrappers),
    }

    payload = json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    print(payload, end="")
    if args.json_output:
        args.json_output.parent.mkdir(parents=True, exist_ok=True)
        args.json_output.write_text(payload, encoding="utf-8")

    structural_failure = bool(
        report["missing_wrapper_for_arkts"]
        or report["missing_declaration_for_arkts"]
        or report["wrapper_without_declaration"]
        or report["declaration_without_wrapper"]
    )
    native_failure = bool(report["missing_native_for_arkts"] or report["missing_native_for_wrapper"])
    if structural_failure or (native_failure and not args.allow_missing_native):
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
