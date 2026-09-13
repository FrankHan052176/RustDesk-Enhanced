# Architecture

## Repository boundary

This repository is the full RustDesk application based on `rustdesk4ohos`.
The existing `src/` runtime and `flutter/` frontend remain authoritative for
Windows, macOS, desktop Linux, Android and iOS. They are not replaced by a
partial compatibility implementation.

The enhanced components are additive:

- `crates/rd-engine` owns the enhanced protocol runtime, media backends and the
  standalone Windows controlled-host executable;
- `native/ohos_har` owns the HarmonyOS N-API bridge and `rustdesk-ohrs.har`;
- `RustDesk-ArkTS` owns ArkUI, lifecycle and permissions orchestration, Surface
  binding, presentation state and HarmonyOS input mapping.

`RustDesk-ArkTS` must not implement RustDesk wire messages or session state
machines. The enhanced runtime must not replace the original Flutter bridge
with neutral or incomplete implementations on platforms already served by the
upstream runtime.

## Platform split

- HarmonyOS uses ArkTS -> HAR -> `rd-engine`.
- Existing Flutter platforms use Flutter -> the root `librustdesk` crate.
- The standalone Windows enhanced host is built explicitly from
  `crates/rd-engine`; it does not alter the default RustDesk desktop build.

## Native boundary

```text
ArkTS frontend
  -> rustdesk-ohrs package/index.ets
  -> N-API compatibility facade
  -> typed rd-engine services
  -> hbb_common protobuf, crypto and configuration
```

The compatibility facade translates arguments, events and errors. Protocol,
authentication, transport, codec negotiation and session ownership remain in
`rd-engine`.

## Security defaults

- Verified ID routing remains fail-closed.
- Direct unverified routes must be labelled compatibility-only.
- HarmonyOS host permissions default to denied. Unset `enable-*` options are
  never interpreted as enabled.
- Host capture requires an explicit local action and system recording consent.
- Secrets, credentials and signing material are never emitted in provenance or
  diagnostics.
