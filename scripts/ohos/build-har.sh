#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
har_root="$repo_root/native/ohos_har"
output_root="$repo_root/dist/ohos-har"

: "${CARGO_HOME:?Set the verified Cargo cache}"
: "${CARGO_TARGET_DIR:?Set an external Cargo target directory}"

source "$repo_root/scripts/ohos/env.sh"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_RUSTFLAGS="$CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_RUSTFLAGS -L native=$sysroot/usr/lib/aarch64-linux-ohos"
export RUSTDESK_ENHANCED_BUILD_MARKER="${RUSTDESK_ENHANCED_BUILD_MARKER:-$(git -C "$repo_root" rev-parse HEAD 2>/dev/null || printf 'local-uncommitted')}"

cd "$har_root"
cargo build --target aarch64-unknown-linux-ohos --release --locked --offline

native_lib="$CARGO_TARGET_DIR/aarch64-unknown-linux-ohos/release/librustdesk_native_har.so"
cxx_runtime="$OHOS_NDK_HOME/native/llvm/lib/aarch64-linux-ohos/libc++_shared.so"
test -s "$native_lib"
test -s "$cxx_runtime"
test -s "$har_root/types/index.d.ts"

rm -rf "$har_root/dist"
mkdir -p "$har_root/dist/arm64-v8a" "$output_root/arm64-v8a"
cp "$native_lib" "$har_root/dist/arm64-v8a/librustdesk_native_har.so"
cp "$cxx_runtime" "$har_root/dist/arm64-v8a/libc++_shared.so"
cp "$har_root/types/index.d.ts" "$har_root/dist/index.d.ts"

"${OHRS:-ohrs}" artifact
test -s "$har_root/package.har"

cp "$native_lib" "$output_root/arm64-v8a/librustdesk_native_har.so"
cp "$cxx_runtime" "$output_root/arm64-v8a/libc++_shared.so"
cp "$har_root/package.har" "$output_root/package.har"

(
  cd "$output_root"
  shasum -a 256 arm64-v8a/librustdesk_native_har.so arm64-v8a/libc++_shared.so package.har > SHA256SUMS
)
cat "$output_root/SHA256SUMS"
