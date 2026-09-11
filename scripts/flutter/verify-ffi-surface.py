#!/usr/bin/env python3
"""Freeze the Flutter FFI surface against the upstream reference.

The Flutter frontend is reused unchanged, so every one of the 321 bridge
functions has to keep its exact name, parameter names, parameter types and
return type. Nothing in upstream CI checks this, and the generated bindings are
gitignored, so drift would only surface as a Dart compile error downstream.
This script is that missing gate.

Usage:
  python3 scripts/flutter/verify-ffi-surface.py \
      --reference /path/to/upstream/src/flutter_ffi.rs \
      --candidate crates/rd-engine/src/flutter_ffi.rs

Exit status is non-zero on any missing, added, or changed signature.
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys


def signatures(path: pathlib.Path) -> dict[str, str]:
    """Extract `pub fn` signatures as normalized single-line strings."""
    text = path.read_text(encoding="utf-8")
    found: dict[str, str] = {}
    for match in re.finditer(r"^pub fn (\w+)\s*\(", text, re.MULTILINE):
        start = match.start()
        open_paren = text.index("(", start)
        depth = 0
        end = -1
        for index in range(open_paren, len(text)):
            character = text[index]
            if character == "(":
                depth += 1
            elif character == ")":
                depth -= 1
                if depth == 0:
                    end = index
                    break
        if end < 0:
            raise SystemExit(f"{path}: unbalanced parameter list for {match.group(1)}")
        rest = text[end + 1 :]
        body = rest.index("{")
        signature = f"pub fn {match.group(1)}{text[open_paren : end + 1]}{rest[:body]}"
        signature = re.sub(r"\s+", " ", signature).strip()
        # Parameter attributes are not part of the wire contract.
        signature = re.sub(r"#\[[^\]]*\]\s*", "", signature)
        # `ResultType<T>` and `anyhow::Result<T>` are the same type to the
        # generated binding; normalize so the reference spelling may differ.
        signature = signature.replace("ResultType<", "Result<")
        # Trailing commas are cosmetic but change the byte comparison.
        signature = re.sub(r",\s*\)", ")", signature)
        found[match.group(1)] = signature
    return found


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference", type=pathlib.Path, required=True)
    parser.add_argument("--candidate", type=pathlib.Path, required=True)
    args = parser.parse_args()

    reference = signatures(args.reference)
    candidate = signatures(args.candidate)

    missing = sorted(set(reference) - set(candidate))
    added = sorted(set(candidate) - set(reference))
    changed = sorted(
        name for name in set(reference) & set(candidate) if reference[name] != candidate[name]
    )

    for name in missing:
        print(f"MISSING {name}", file=sys.stderr)
    for name in added:
        print(f"ADDED   {name}", file=sys.stderr)
    for name in changed:
        print(f"CHANGED {name}", file=sys.stderr)
        print(f"  reference: {reference[name]}", file=sys.stderr)
        print(f"  candidate: {candidate[name]}", file=sys.stderr)

    print(
        f"reference={len(reference)} candidate={len(candidate)} "
        f"missing={len(missing)} added={len(added)} changed={len(changed)}"
    )
    return 1 if (missing or added or changed) else 0


if __name__ == "__main__":
    raise SystemExit(main())
