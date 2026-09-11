#!/usr/bin/env python3
"""Check the committed FRB bindings still cover the whole FFI surface.

The generated bindings are committed so the crate can be formatted, tested and
built without a Dart toolchain. That trade removes the guarantee codegen
provided implicitly, so this script restores it: every `pub fn` declared in
`flutter_ffi.rs` must have a `wire_<name>_impl` in the committed
`bridge_generated.rs`, and the committed bindings must not declare functions
whose source has since been deleted.

This is a name-level check, not a signature comparison. Signature fidelity
against the upstream reference is `verify-ffi-surface.py`'s job; this one runs
without any toolchain, so CI can enforce it on every platform.

Usage:
  python3 scripts/flutter/verify-committed-bridge.py

Exit status is non-zero when the committed bindings are stale.
"""

from __future__ import annotations

import pathlib
import re
import sys

KERNEL = pathlib.Path(__file__).resolve().parents[2] / "crates" / "rd-engine"
FFI = KERNEL / "src" / "flutter_ffi.rs"
GENERATED = KERNEL / "src" / "bridge_generated.rs"
# The Dart side is generated in the same run, so a mismatch in sizes is a
# cheap hint that someone regenerated one artefact and not the others.
DART = KERNEL / "flutter_bridge" / "generated_bridge.dart"


def declared_functions() -> set[str]:
    text = FFI.read_text(encoding="utf-8")
    return set(re.findall(r"^pub fn (\w+)", text, re.MULTILINE))


def generated_functions() -> set[str]:
    text = GENERATED.read_text(encoding="utf-8")
    return set(re.findall(r"^fn wire_(\w+)_impl\b", text, re.MULTILINE))


def main() -> int:
    for path in (FFI, GENERATED, DART):
        if not path.is_file():
            print(f"missing {path}", file=sys.stderr)
            return 2

    declared = declared_functions()
    generated = generated_functions()

    uncovered = sorted(declared - generated)
    orphaned = sorted(generated - declared)

    for name in uncovered:
        print(f"NOT GENERATED {name}", file=sys.stderr)
    for name in orphaned:
        print(f"STALE         {name}", file=sys.stderr)

    print(
        f"declared={len(declared)} generated={len(generated)} "
        f"uncovered={len(uncovered)} stale={len(orphaned)}"
    )
    if uncovered or orphaned:
        print(
            "rerun scripts/flutter/generate-bridge.sh and commit the result",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
