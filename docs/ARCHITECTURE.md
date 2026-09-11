# Architecture

## Repository boundary

`RustDesk-Enhanced` is the source authority for the enhanced protocol runtime,
platform media backends, Windows host executable, HarmonyOS N-API bridge and
the `rustdesk-ohrs.har` artifact.

It must not depend on a sibling `RustDesk-Har` or on `rustdesk4ohos`.

`RustDesk-ArkTS` owns ArkUI, lifecycle and permissions orchestration, Surface
binding, presentation state and input mapping. It must not implement RustDesk
wire messages or session state machines.

`rustdesk4ohos` owns the Flutter HarmonyOS client and its upstream convergence.

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
