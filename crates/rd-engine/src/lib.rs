//! Replacement RustDesk runtime. Wire definitions and cryptography retain the
//! upstream contract; legacy Connection/VideoService/VideoHandler are not linked.

/// The protocol/peer version this runtime reports to remotes.
///
/// It is a wire-visible value: peers gate features on it, so it names the
/// original release whose semantics this runtime implements rather than this
/// crate's own `CARGO_PKG_VERSION`.
pub const REPORTED_VERSION: &str = "1.4.9";

/// Product name shown to peers and in locally generated text.
pub const APP_NAME: &str = "RustDesk";

pub mod authentication;
/// Encoder bitrate selection for a session shape. Used by the controlled host.
pub mod bitrate;
mod executor;
pub mod handshake;
#[cfg(any(
    not(target_os = "windows"),
    feature = "windows-modern-producer",
    dsh_windows_typecheck
))]
pub mod host;
pub mod input;
pub mod media_capability;
pub mod media_color;
#[cfg(not(any(target_os = "windows", dsh_windows_typecheck)))]
#[path = "platform/ohos_publisher.rs"]
pub mod publisher;
#[cfg(all(
    any(target_os = "windows", dsh_windows_typecheck),
    feature = "windows-modern-producer"
))]
#[path = "platform/windows_host_publisher.rs"]
pub mod publisher;
pub mod rdp;
pub mod rendezvous;
pub mod session;
/// One interface over the protocols a session can speak (RustDesk, VNC).
pub mod session_backend;
pub mod transport;
pub mod viewer;
/// VNC (RFB) support.
pub mod vnc;
/// Windows controlled-side input injection. It exists only where `SendInput`
/// does, and is never reachable from an unauthenticated peer.
#[cfg(all(
    any(target_os = "windows", dsh_windows_typecheck),
    feature = "windows-modern-producer"
))]
#[path = "platform/windows_input.rs"]
pub mod windows_input;
#[cfg(all(
    any(target_os = "windows", dsh_windows_typecheck),
    feature = "windows-modern-producer"
))]
#[path = "platform/windows_native.rs"]
pub mod windows_native;
#[cfg(all(
    any(target_os = "windows", dsh_windows_typecheck),
    feature = "windows-modern-producer"
))]
pub use windows_native::NATIVE_BACKEND_IMPLEMENTED;
/// Content-adaptive bitrate control for the controlled host. Platform-neutral
/// arithmetic, kept beside the publisher it drives.
#[cfg(feature = "windows-modern-producer")]
#[path = "platform/windows_bitrate.rs"]
pub mod windows_bitrate;
#[cfg(feature = "windows-modern-producer")]
#[path = "platform/windows_publisher.rs"]
pub mod windows_publisher;
