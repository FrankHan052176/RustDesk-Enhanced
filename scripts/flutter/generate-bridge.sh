#!/usr/bin/env bash
# Regenerate the Flutter Rust bridge for the reuse-the-Flutter-UI path.
#
# The kernel is the FRB scan target, and its crate name decides the generated
# Dart class name: package `rustdesk` yields `Rustdesk`/`RustdeskImpl`, which is
# exactly what the existing Flutter frontend constructs. Do not add a
# `--class-name` override, and do not rename the package.
#
# Requirements, all discovered the hard way against frb 1.80.1:
#   - frb 1.80.1 depends on uuid ^3.0.6, so the Dart side cannot use uuid 4.
#   - a resolvable pubspec.lock must exist or codegen aborts inside Dart tooling.
#   - libclang is needed for the C header; pass LLVM_PATH when it is not on PATH.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
kernel="$repo_root/crates/rd-engine"
dart_root="$kernel/flutter_bridge"

: "${FRB_CODEGEN:=flutter_rust_bridge_codegen}"
: "${LLVM_PATH:=}"

if ! command -v "$FRB_CODEGEN" >/dev/null 2>&1; then
  echo "flutter_rust_bridge_codegen 1.80.1 is required but not on PATH" >&2
  exit 2
fi

if [[ ! -f "$dart_root/pubspec.lock" ]]; then
  echo "missing $dart_root/pubspec.lock; run: (cd $dart_root && dart pub get)" >&2
  exit 2
fi

args=(
  --rust-input "$kernel/src/flutter_ffi.rs"
  --rust-crate-dir "$kernel"
  --dart-root "$dart_root"
  --dart-output "$dart_root/generated_bridge.dart"
  --c-output "$dart_root/bridge_generated.h"
  # The kernel's lib.rs is maintained by hand, and codegen's auto-injection puts
  # the module line above the crate doc comment, which then fails to re-parse.
  --skip-add-mod-to-lib
  # Offline generation is the norm here; dependency checks would need the network.
  --skip-deps-check
)
if [[ -n "$LLVM_PATH" ]]; then
  args+=(--llvm-path "$LLVM_PATH")
fi

"$FRB_CODEGEN" "${args[@]}"

python3 "$repo_root/scripts/flutter/patch-generated-rust.py" \
  "$kernel/src/bridge_generated.rs" \
  "$kernel/src/bridge_generated.io.rs"

echo "Generated:"
echo "  $kernel/src/bridge_generated.rs"
echo "  $kernel/src/bridge_generated.io.rs"
echo "  $dart_root/generated_bridge.dart"
echo "  $dart_root/bridge_generated.h"
