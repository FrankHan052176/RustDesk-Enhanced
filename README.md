# RustDesk Enhanced

`RustDesk-Enhanced` is the standalone protocol and media runtime used by the
enhanced RustDesk packages. It is intentionally independent of
`rustdesk4ohos`, which remains the repository for the Flutter HarmonyOS client.

This repository owns one source-to-artifact chain:

```text
libs/hbb_common -> crates/rd-engine -> native/ohos_har -> rustdesk-ohrs.har
```

`RustDesk-ArkTS` consumes the resulting HAR directly. There is no separate
`RustDesk-Har` source repository in this architecture.

## Current status

- `rd-engine` provides the independent viewer/host protocol runtime and the
  Windows DXGI/NVENC and HarmonyOS media backends.
- `native/ohos_har` builds the `rustdesk-ohrs` package and
  `librustdesk_native_har.so`.
- The native module currently implements the narrow `engine*` API. The package
  still carries transitional legacy wrappers, so it is not yet ABI-compatible
  with the existing `RustDesk-ArkTS` frontend. Compatibility work is tracked in
  `docs/PROTOCOL_MILESTONES.md` and must be completed before release.
- Protocol fields and enum values are authoritative only in
  `libs/hbb_common/protos/*.proto`.

## HarmonyOS HAR build

The canonical output is `dist/ohos-har/package.har`.

```bash
export CARGO_HOME=/path/to/verified/cargo-home
export CARGO_TARGET_DIR=/path/to/external/target
bash scripts/ohos/build-har.sh
```

The build is arm64-only for the initial migration. Additional ABIs must be
added as explicit, separately validated targets.

## Licensing and source lineage

The combined runtime and native package are distributed under
AGPL-3.0-only. Source lineage and the exact migration inputs are recorded in
`SOURCE_ORIGINS.md`.
