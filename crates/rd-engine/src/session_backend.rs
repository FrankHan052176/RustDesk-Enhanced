//! One interface over the protocols a session can speak.
//!
//! The facades (`native/ohos_har` for HarmonyOS, `flutter_ffi` for the reused
//! Flutter frontends) describe a session in one vocabulary: start it, report a
//! snapshot, send pointer and keyboard events, exchange clipboard text. That
//! vocabulary is the RustDesk protocol's shape, because it came first.
//!
//! VNC speaks a different protocol but the same *operations*, so this module
//! holds either backend behind one enum. Facades keep their existing call sites;
//! what changes is that "the peer" may now be an RFB server, and the places where
//! that genuinely behaves differently are answered explicitly (see
//! [`SessionBackend::set_image_quality`] and
//! [`SessionBackend::peer_info`]) rather than left to look like a bug.
//!
//! What VNC cannot do is stated where it applies, not hidden:
//! - no image-quality negotiation: RFB has no equivalent control message;
//! - no remote-side resolution change: `SetDesktopSize` is a request to resize
//!   the server's own framebuffer, not to change a display mode;
//! - no capture-rate control: the server pushes when the screen changes.

use std::sync::Arc;

use hbb_common::message_proto::{DisplayInfo, PeerInfo};

use crate::viewer::{Viewer, ViewerError, ViewerImageQuality, ViewerKey, ViewerSnapshot};
use crate::vnc::live::VncLiveSession;

/// A running session, whichever protocol it speaks.
pub enum SessionBackend {
    /// The RustDesk protocol, with a hardware-decoded video stream.
    RustDesk(Arc<Viewer>),
    /// VNC, whose framebuffer is raw pixels.
    Vnc(Arc<VncLiveSession>),
}

impl SessionBackend {
    pub fn is_rustdesk(&self) -> bool {
        matches!(self, Self::RustDesk(_))
    }

    pub fn is_vnc(&self) -> bool {
        matches!(self, Self::Vnc(_))
    }

    /// The protocol's name, for logs and telemetry.
    pub fn protocol(&self) -> &'static str {
        match self {
            Self::RustDesk(_) => "rustdesk",
            Self::Vnc(_) => "vnc",
        }
    }

    pub fn snapshot(&self) -> ViewerSnapshot {
        match self {
            Self::RustDesk(viewer) => viewer.snapshot(),
            Self::Vnc(session) => {
                let snapshot = session.snapshot();
                ViewerSnapshot {
                    phase: snapshot.phase,
                    error: snapshot.error,
                    width: snapshot.width,
                    height: snapshot.height,
                    // RFB carries no codec name: the picture arrives as pixels,
                    // which is why this is not "H264" or any other guess.
                    codec: "raw".to_owned(),
                    // A direct connection is the only route VNC has here; there
                    // is no relay or hole punching behind it.
                    route: "direct_tcp".to_owned(),
                    received_units: snapshot.received_units,
                    // Nothing is "pushed" into a decoder; the frontend taking a
                    // frame is the equivalent event.
                    pushed_units: snapshot.taken_frames,
                    render_submissions: snapshot.taken_frames,
                    keyboard_allowed: snapshot.keyboard_allowed,
                    clipboard_allowed: snapshot.clipboard_allowed,
                    // No quality negotiation exists, so the reported value is the
                    // absence of one rather than a level that would mislead.
                    image_quality: "n/a".to_owned(),
                    requested_fps: 0,
                    closed: snapshot.closed,
                    // RFB authentication may be no authentication at all, and a
                    // desktop connection is not encrypted by the protocol. Both
                    // are reported honestly so a frontend cannot imply safety
                    // the session does not have.
                    encrypted: false,
                    peer_verified: false,
                }
            }
        }
    }

    /// Describe the peer in the shared shape.
    ///
    /// RFB reports a server name and a framebuffer size; it has no user, no
    /// display list and no feature set. The single display is therefore built
    /// from the framebuffer itself, and the string fields say what they are
    /// instead of imitating RustDesk values.
    pub fn peer_info(&self) -> Option<PeerInfo> {
        match self {
            Self::RustDesk(viewer) => viewer.peer_info(),
            Self::Vnc(session) => {
                // The generated protobuf types expose public fields, so this is
                // ordinary struct construction with `..Default::default()` for
                // everything RFB has no value for.
                let display = DisplayInfo {
                    x: 0,
                    y: 0,
                    width: i32::from(session.width()),
                    height: i32::from(session.height()),
                    name: session.server_name().to_owned(),
                    online: true,
                    // RFB has no embedded-cursor flag; a client draws the pointer
                    // it knows about locally.
                    cursor_embedded: false,
                    scale: 1.0,
                    ..Default::default()
                };
                Some(PeerInfo {
                    hostname: session.server_name().to_owned(),
                    platform: "VNC".to_owned(),
                    version: session.server_version().to_owned(),
                    displays: vec![display],
                    current_display: 0,
                    ..Default::default()
                })
            }
        }
    }

    pub fn mouse_refusal(&self) -> Option<&'static str> {
        match self {
            Self::RustDesk(viewer) => viewer.mouse_refusal(),
            // VNC has no permission handshake to refuse through: a server that
            // rejects input simply ignores it, so there is never a refusal here.
            Self::Vnc(_) => None,
        }
    }

    /// Pointer input. `kind`, `button`, `x` and `y` follow the RustDesk
    /// vocabulary because that is what the facades speak.
    pub fn send_mouse(&self, kind: u32, button: u32, x: i32, y: i32) -> Result<(), ViewerError> {
        match self {
            Self::RustDesk(viewer) => viewer.send_mouse(kind, button, x, y),
            Self::Vnc(session) => {
                let (mask, x, y) = vnc_pointer(kind, button, x, y)?;
                session.send_mouse(mask, x, y).map_err(ViewerError::Vnc)
            }
        }
    }

    pub fn send_key(
        &self,
        key: ViewerKey,
        down: bool,
        press: bool,
        alt: bool,
        ctrl: bool,
        shift: bool,
        command: bool,
    ) -> Result<(), ViewerError> {
        match self {
            Self::RustDesk(viewer) => viewer.send_key(key, down, press, alt, ctrl, shift, command),
            Self::Vnc(session) => {
                // A VNC server sees key presses as X11 keysyms with explicit
                // modifiers, so a "press" becomes a down and an up and the
                // modifier flags become real modifier key events around it.
                let modifiers = vnc_modifiers(alt, ctrl, shift, command);
                let keysym = vnc_keysym(key, shift)?;
                for modifier in &modifiers {
                    session
                        .send_key(true, *modifier)
                        .map_err(ViewerError::Vnc)?;
                }
                let result = (|| {
                    if down {
                        session.send_key(true, keysym)?;
                    }
                    if !down || press {
                        session.send_key(false, keysym)?;
                    }
                    Ok(())
                })();
                // Modifiers are released even when the key itself failed, so a
                // stuck modifier cannot survive a failed keystroke.
                for modifier in modifiers.iter().rev() {
                    let _ = session.send_key(false, *modifier);
                }
                result.map_err(ViewerError::Vnc)
            }
        }
    }

    pub fn send_text(&self, value: String) -> Result<(), ViewerError> {
        match self {
            Self::RustDesk(viewer) => viewer.send_text(value),
            Self::Vnc(session) => {
                // RFB has no "type this string" message, so each character is
                // sent as its Latin-1 keysym: X11 keysyms below 0x100 are the
                // character itself. Anything outside that range cannot be
                // expressed as a keysym and is refused rather than dropped, so a
                // caller learns the text did not arrive.
                for character in value.chars() {
                    let code = character as u32;
                    if code > 0xFF {
                        return Err(ViewerError::Vnc(crate::vnc::VncError::protocol(format!(
                            "character {character:?} has no Latin-1 keysym, which VNC needs"
                        ))));
                    }
                    session.send_key(true, code).map_err(ViewerError::Vnc)?;
                    session.send_key(false, code).map_err(ViewerError::Vnc)?;
                }
                Ok(())
            }
        }
    }

    pub fn send_clipboard_text(&self, text: String) -> Result<(), ViewerError> {
        match self {
            Self::RustDesk(viewer) => viewer.send_clipboard_text(text),
            Self::Vnc(session) => session.send_clipboard(&text).map_err(ViewerError::Vnc),
        }
    }

    pub fn take_clipboard_text(&self) -> Option<String> {
        match self {
            Self::RustDesk(viewer) => viewer.take_clipboard_text(),
            Self::Vnc(session) => session.take_clipboard(),
        }
    }

    /// Ask the peer to capture at this rate.
    ///
    /// RFB has no such control: the server pushes updates when the screen
    /// changes. The call is accepted so a frontend that always sends it keeps
    /// working, and the snapshot's `requested_fps` stays 0 for a VNC session so
    /// nothing can display a rate that is not in effect.
    pub fn set_requested_fps(&self, fps: u32) -> Result<(), ViewerError> {
        match self {
            Self::RustDesk(viewer) => viewer.set_requested_fps(fps),
            Self::Vnc(session) => session.set_requested_fps(fps).map_err(ViewerError::Vnc),
        }
    }

    /// Request an image quality.
    ///
    /// RFB has no image-quality message. A RustDesk-quality request against a
    /// VNC session is therefore refused explicitly: silently accepting it would
    /// leave the operator believing a setting had taken effect.
    pub fn set_image_quality(&self, quality: ViewerImageQuality) -> Result<(), ViewerError> {
        match self {
            Self::RustDesk(viewer) => viewer.set_image_quality(quality),
            Self::Vnc(_) => Err(ViewerError::Vnc(crate::vnc::VncError::protocol(format!(
                "VNC has no image-quality control, so {} cannot be requested",
                quality.label()
            )))),
        }
    }

    /// Select a display.
    ///
    /// RFB 3.8 has no multi-display concept, so only display 0 exists and any
    /// other index is refused rather than silently mapped onto the one
    /// framebuffer.
    pub fn switch_display(&self, display: i32, width: i32, height: i32) -> Result<(), ViewerError> {
        match self {
            Self::RustDesk(viewer) => viewer.switch_display(display, width, height),
            Self::Vnc(session) => {
                if display != 0 {
                    return Err(ViewerError::Vnc(crate::vnc::VncError::geometry(format!(
                        "a VNC server has one framebuffer, so display {display} does not exist"
                    ))));
                }
                // The reported geometry comes from the server, not the caller.
                let _ = (width, height);
                session.refresh().map_err(ViewerError::Vnc)
            }
        }
    }

    /// Ask for a different remote resolution.
    ///
    /// Not supported over VNC: `SetDesktopSize` would ask the server to resize
    /// its own framebuffer, which is a different operation from setting a
    /// display mode, and this client does not advertise the extension.
    pub fn change_resolution(
        &self,
        display: i32,
        width: i32,
        height: i32,
    ) -> Result<(), ViewerError> {
        match self {
            Self::RustDesk(viewer) => viewer.change_resolution(display, width, height),
            Self::Vnc(_) => Err(ViewerError::Vnc(crate::vnc::VncError::protocol(
                "VNC cannot change the remote resolution".to_owned(),
            ))),
        }
    }

    /// Ask for a fresh picture of the given display.
    pub fn refresh_video(&self, display: i32) -> Result<(), ViewerError> {
        match self {
            Self::RustDesk(viewer) => viewer.refresh_video(display),
            Self::Vnc(session) => {
                if display != 0 {
                    return Err(ViewerError::Vnc(crate::vnc::VncError::geometry(format!(
                        "a VNC server has one framebuffer, so display {display} does not exist"
                    ))));
                }
                session.refresh().map_err(ViewerError::Vnc)
            }
        }
    }

    /// Submit the password.
    ///
    /// A RustDesk login is a message exchange that can happen after the socket
    /// is up. VNC authenticates during the handshake, before a session exists, so
    /// there is nothing left to submit by the time this is called. Saying so is
    /// better than reporting success for a password that was never used.
    pub fn submit_password(&self, _password: String) -> Result<(), ViewerError> {
        match self {
            Self::RustDesk(viewer) => viewer.submit_password(_password),
            Self::Vnc(_) => Err(ViewerError::Vnc(crate::vnc::VncError::protocol(
                "VNC authenticates during the handshake, so a password cannot be submitted afterwards"
                    .to_owned(),
            ))),
        }
    }

    pub fn submit_second_factor(&self, _code: String) -> Result<(), ViewerError> {
        match self {
            Self::RustDesk(viewer) => viewer.submit_second_factor(_code),
            Self::Vnc(_) => Err(ViewerError::Vnc(crate::vnc::VncError::protocol(
                "VNC has no second factor beyond its own authentication".to_owned(),
            ))),
        }
    }

    pub fn continue_insecure(&self, allow: bool) -> Result<(), ViewerError> {
        match self {
            Self::RustDesk(viewer) => viewer.continue_insecure(allow),
            // RFB never verifies a remote identity, so there is no verification
            // state to continue past. Refusing here would break a connection that
            // was never gated in the first place.
            Self::Vnc(_) => Ok(()),
        }
    }

    pub fn request_close(&self) {
        match self {
            Self::RustDesk(viewer) => viewer.request_close(),
            Self::Vnc(session) => session.close(),
        }
    }

    /// The newest raw framebuffer, for backends that produce one.
    ///
    /// A RustDesk session decodes into a hardware surface and has no raw frame
    /// to hand out; only VNC does.
    pub fn take_raw_frame(&self) -> Option<(u32, u32, Vec<u8>)> {
        match self {
            Self::RustDesk(_) => None,
            Self::Vnc(session) => session.take_frame(),
        }
    }
}

/// Translate the facade's pointer vocabulary into RFB's button mask.
///
/// The facades identify a pointer event as `kind | (button << 3)`, and `button`
/// is a **button number**, the X11 numbering the app's input mapper produces:
/// 1 left, 2 right, 3 middle, 4 wheel up, 5 wheel down. RFB instead sends one
/// mask of the buttons currently held, where the bits are left, middle, right,
/// wheel up, wheel down. So the two number the buttons differently and the
/// translation is by name, not by shifting.
///
/// `kind` is the event type: 0 move, 1 press, 2 release, 3 to 5 wheel and
/// trackpad deltas. A move and a release both report an empty mask here, because
/// this client does not track which buttons are already held; a caller that
/// needs a drag sends presses and moves, and RFB servers treat the missing bits
/// as a released button, which is what a drag without an intervening press would
/// be anyway.
fn vnc_pointer(kind: u32, button: u32, x: i32, y: i32) -> Result<(u8, u16, u16), ViewerError> {
    use crate::vnc::protocol::{
        POINTER_BUTTON_LEFT, POINTER_BUTTON_MIDDLE, POINTER_BUTTON_RIGHT,
        POINTER_BUTTON_WHEEL_DOWN, POINTER_BUTTON_WHEEL_UP,
    };
    if x < 0 || y < 0 {
        return Err(ViewerError::Vnc(crate::vnc::VncError::geometry(format!(
            "pointer ({x},{y}) is negative"
        ))));
    }
    // By name, because the two numberings disagree about middle and right.
    let named = |number: u32| -> Result<u8, ViewerError> {
        match number {
            1 => Ok(POINTER_BUTTON_LEFT),
            2 => Ok(POINTER_BUTTON_RIGHT),
            3 => Ok(POINTER_BUTTON_MIDDLE),
            4 => Ok(POINTER_BUTTON_WHEEL_UP),
            5 => Ok(POINTER_BUTTON_WHEEL_DOWN),
            0 => Ok(0),
            other => Err(ViewerError::Vnc(crate::vnc::VncError::protocol(format!(
                "button {other} has no RFB equivalent"
            )))),
        }
    };
    let mask = match kind {
        0 => 0,
        1 => named(button)?,
        // A release names the button so an unknown number is still refused, but
        // sends no held buttons.
        2 => {
            named(button)?;
            0
        }
        3..=5 => named(button)?,
        other => {
            return Err(ViewerError::Vnc(crate::vnc::VncError::protocol(format!(
                "mouse event kind {other} has no RFB equivalent"
            ))));
        }
    };
    Ok((mask, x as u16, y as u16))
}

/// X11 modifier keysyms for the modifier flags the facade reports.
fn vnc_modifiers(alt: bool, ctrl: bool, shift: bool, command: bool) -> Vec<u32> {
    // 0xFFE9 Alt_L, 0xFFE3 Control_L, 0xFFE1 Shift_L, 0xFFEB Super_L.
    let mut modifiers = Vec::new();
    if ctrl {
        modifiers.push(0xFFE3);
    }
    if alt {
        modifiers.push(0xFFE9);
    }
    if shift {
        modifiers.push(0xFFE1);
    }
    if command {
        modifiers.push(0xFFEB);
    }
    modifiers
}

/// The X11 keysym for a legacy key identity.
///
/// Characters map to their Latin-1 keysym; protocol control keys map to the X11
/// key that produces the same effect, which is what an RFB server understands.
fn vnc_keysym(key: ViewerKey, shift: bool) -> Result<u32, ViewerError> {
    use hbb_common::message_proto::ControlKey;
    match key {
        ViewerKey::Character(value) => {
            // The original protocol carries a character code, which for the
            // printable range is the Latin-1 keysym. Upper case is produced by
            // sending the shifted keysym, which already encodes the shift.
            if value > 0xFF {
                return Err(ViewerError::Vnc(crate::vnc::VncError::protocol(format!(
                    "key code {value} has no Latin-1 keysym"
                ))));
            }
            Ok(value)
        }
        ViewerKey::Control(control) => {
            let keysym = match control {
                ControlKey::Alt => 0xFFE9,
                ControlKey::Backspace => 0xFF08,
                ControlKey::CapsLock => 0xFFE5,
                ControlKey::Control => 0xFFE3,
                ControlKey::Delete => 0xFFFF,
                ControlKey::DownArrow => 0xFF54,
                ControlKey::End => 0xFF57,
                ControlKey::Escape => 0xFF1B,
                ControlKey::F1 => 0xFFBE,
                ControlKey::F2 => 0xFFBF,
                ControlKey::F3 => 0xFFC0,
                ControlKey::F4 => 0xFFC1,
                ControlKey::F5 => 0xFFC2,
                ControlKey::F6 => 0xFFC3,
                ControlKey::F7 => 0xFFC4,
                ControlKey::F8 => 0xFFC5,
                ControlKey::F9 => 0xFFC6,
                ControlKey::F10 => 0xFFC7,
                ControlKey::F11 => 0xFFC8,
                ControlKey::F12 => 0xFFC9,
                ControlKey::Home => 0xFF50,
                ControlKey::LeftArrow => 0xFF51,
                ControlKey::Meta => 0xFFEB,
                ControlKey::PageDown => 0xFF56,
                ControlKey::PageUp => 0xFF55,
                ControlKey::Return => 0xFF0D,
                ControlKey::RightArrow => 0xFF53,
                ControlKey::Shift => 0xFFE1,
                ControlKey::Space => 0x20,
                ControlKey::Tab => 0xFF09,
                ControlKey::UpArrow => 0xFF52,
                // Insert has a keysym; the remaining protocol keys either have
                // no X11 equivalent or are secure-attention sequences that a
                // server must refuse anyway.
                ControlKey::Insert => 0xFF63,
                other => {
                    return Err(ViewerError::Vnc(crate::vnc::VncError::protocol(format!(
                        "control key {other:?} has no X11 keysym"
                    ))));
                }
            };
            let _ = shift;
            Ok(keysym)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_modifier_keysyms_are_the_x11_ones() {
        assert_eq!(vnc_modifiers(true, false, false, false), vec![0xFFE9]);
        assert_eq!(vnc_modifiers(false, true, false, false), vec![0xFFE3]);
        assert_eq!(vnc_modifiers(false, false, true, false), vec![0xFFE1]);
        assert_eq!(vnc_modifiers(false, false, false, true), vec![0xFFEB]);
        // Order matters: modifiers are pressed before the key and released in
        // reverse, so control comes first.
        assert_eq!(
            vnc_modifiers(true, true, true, false),
            vec![0xFFE3, 0xFFE9, 0xFFE1]
        );
    }

    #[test]
    fn buttons_are_translated_by_name_because_the_numberings_differ() {
        // Facade: 1 left, 2 right, 3 middle. RFB bits: left, middle, right.
        assert_eq!(
            vnc_pointer(1, 1, 10, 20).unwrap(),
            (1, 10, 20),
            "left press"
        );
        assert_eq!(
            vnc_pointer(1, 2, 10, 20).unwrap(),
            (4, 10, 20),
            "right press"
        );
        assert_eq!(
            vnc_pointer(1, 3, 10, 20).unwrap(),
            (2, 10, 20),
            "middle press"
        );
        // A release sends no held buttons but still refuses an unknown number.
        assert_eq!(
            vnc_pointer(2, 1, 10, 20).unwrap(),
            (0, 10, 20),
            "left release"
        );
    }

    #[test]
    fn wheel_directions_use_the_rfb_wheel_buttons() {
        assert_eq!(vnc_pointer(3, 4, 1, 2).unwrap(), (8, 1, 2), "wheel up");
        assert_eq!(vnc_pointer(3, 5, 1, 2).unwrap(), (16, 1, 2), "wheel down");
    }

    #[test]
    fn a_move_carries_no_button() {
        assert_eq!(vnc_pointer(0, 0, 5, 6).unwrap(), (0, 5, 6));
    }

    #[test]
    fn a_named_button_outside_the_known_bits_and_unknown_kinds_are_refused() {
        // 9 is not one of the facade's button numbers.
        assert!(vnc_pointer(1, 9, 0, 0).is_err());
        // The facade only defines kinds 0 through 5.
        assert!(vnc_pointer(9, 1, 0, 0).is_err());
    }

    #[test]
    fn a_negative_position_is_refused() {
        assert!(vnc_pointer(1, 1, -1, 0).is_err());
        assert!(vnc_pointer(1, 1, 0, -1).is_err());
    }

    #[test]
    fn printable_characters_become_their_latin1_keysym() {
        assert_eq!(
            vnc_keysym(ViewerKey::Character(b'a' as u32), false).unwrap(),
            0x61
        );
        assert_eq!(
            vnc_keysym(ViewerKey::Character(b'Z' as u32), true).unwrap(),
            0x5A
        );
        // Beyond Latin-1 there is no keysym this client can send.
        assert!(vnc_keysym(ViewerKey::Character('中' as u32), false).is_err());
    }

    #[test]
    fn the_control_keys_that_matter_have_x11_equivalents() {
        use hbb_common::message_proto::ControlKey;
        assert_eq!(
            vnc_keysym(ViewerKey::Control(ControlKey::Return), false).unwrap(),
            0xFF0D
        );
        assert_eq!(
            vnc_keysym(ViewerKey::Control(ControlKey::Escape), false).unwrap(),
            0xFF1B
        );
        assert_eq!(
            vnc_keysym(ViewerKey::Control(ControlKey::F12), false).unwrap(),
            0xFFC9
        );
        assert_eq!(
            vnc_keysym(ViewerKey::Control(ControlKey::UpArrow), false).unwrap(),
            0xFF52
        );
    }
}
