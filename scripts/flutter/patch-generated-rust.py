#!/usr/bin/env python3
"""Make flutter_rust_bridge 1.80.1 output compile under Rust edition 2024.

The kernel is edition 2024 (it uses let chains), while frb 1.80.1 emits the
edition-2021 form of the `no_mangle` attribute. Edition 2024 requires
`#[unsafe(no_mangle)]`, and that spelling is also accepted on edition 2021, so
the rewrite is meaning-preserving: the exported symbol keeps its exact name and
linkage, which is precisely what the Dart side binds to through `dlsym`.

Run after every codegen; `scripts/flutter/generate-bridge.sh` does it for you.
"""

from __future__ import annotations

import pathlib
import re
import sys

# Only the attribute spelling changes, never the symbol name.
NO_MANGLE = re.compile(r"#\[no_mangle\]")


def patch(path: pathlib.Path) -> int:
    if not path.is_file():
        return 0
    text = path.read_text(encoding="utf-8")
    patched, count = NO_MANGLE.subn("#[unsafe(no_mangle)]", text)
    if count:
        path.write_text(patched, encoding="utf-8")
    return count


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    total = 0
    for argument in sys.argv[1:]:
        path = pathlib.Path(argument)
        count = patch(path)
        total += count
        print(f"{path}: {count} attribute(s) updated")
    if total == 0:
        print("warning: no `#[no_mangle]` found; did codegen output change?", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
