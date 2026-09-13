# Protocol and ArkTS compatibility milestones

## Cross-platform invariant

- Windows, macOS, desktop Linux, Android and iOS retain the original RustDesk
  Flutter frontend and root runtime.
- The enhanced runtime is integrated additively and may not replace an
  implemented upstream FFI operation with a neutral result or no-op.
- HarmonyOS continues to use the ArkTS/HAR boundary described below.

## M0 — truthful package contract

- Keep package name `rustdesk-ohrs` and soname `librustdesk_native_har.so`.
- Freeze every API actually imported by the existing ArkTS frontend.
- Verify declaration, ArkTS wrapper and native implementation parity.
- Remove declarations that exist only to make compilation pass, or provide an
  explicit structured compatibility implementation.

## M1 — existing ArkTS viewer shell

- Implement runtime initialization, session creation/start/close and event
  polling behind the existing ArkTS bridge.
- Bind and rebind XComponent Surfaces with generation-safe lifecycle handling.
- Preserve existing event names and payload schemas through a versioned DTO
  adapter.

## M2 — authentication parity

- Password and local click approval.
- Legal empty-salt challenge handling.
- 2FA continuation, trusted-device identity and bounded login failures.
- Do not conflate challenge data with temporary-password storage.
- Remove hard-coded version claims that advertise unsupported gates.

## M3 — normal control input

- Mouse, keyboard, modifiers, text and IME semantics.
- Preserve numeric ArkTS bridge commands as an ABI only; RustDesk wire packing
  stays in the runtime.
- Keep host permissions fail-closed and require explicit local policy.

## M4 — viewer interoperability

- Direct and verified ID routes against an unmodified RustDesk host.
- TCP hole punch and forced relay with truthful fallback behavior.
- H.264/H.265 baseline plus explicit codec negotiation/fallback policy.
- Display switching, refresh, reconnect and network-transition recovery.

## M5 — collaboration channels

- Clipboard text/image/files, chat, remote cursor and audio.
- File transfer with bounded jobs, cancellation and conflict handling.

## M6 — discoverable HarmonyOS host

- hbbs registration, heartbeat, NAT/rendezvous and hbbr relay handling.
- Password, click approval and 2FA under local policy.
- Per-session permission ceilings; never default-enable unset OHOS options.
- Multi-session means independent per-tab/per-peer sessions, not shared mutable
  peer state.

## First acceptance target

The first product milestone is the existing ArkTS frontend controlling an
unmodified RustDesk host through the compatibility facade. HarmonyOS host ID
registration follows after viewer interoperability is stable.
