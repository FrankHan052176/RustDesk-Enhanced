//! Windows controlled-side input injection.
//!
//! This is the only module in the crate that synthesizes system input. It
//! consumes the platform-neutral actions produced by [`crate::input`] and emits
//! Win32 `SendInput` records, which is how the original RustDesk controlled side
//! injects pointer and keyboard input on Windows.
//!
//! Boundaries:
//! - Injection is armed only by an explicit local host option. An authenticated
//!   peer that was never granted input cannot reach any of this.
//! - The original legacy semantics are preserved: `chr` is a virtual key code,
//!   `seq` is typed as Unicode text, and `control_key` maps onto the documented
//!   `VK_*` names decided in [`crate::input::windows_virtual_key`].
//! - `CtrlAltDel` and `LockScreen` are Secure Attention Sequences that Windows
//!   refuses through `SendInput`; they are refused, never remapped.
//! - A partial `SendInput` send leaves a stuck modifier or button, so it is a
//!   hard error rather than a retry.

use crate::input::{KeyAction, MouseAction, MouseButton, normalize_absolute, windows_virtual_key};
use std::mem::{MaybeUninit, size_of};
use winapi::{
    shared::minwindef::{DWORD, UINT},
    um::winuser::{
        GetSystemMetrics, INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
        KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
        MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL,
        MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
        SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SendInput, XBUTTON1, XBUTTON2,
    },
};

/// The controlled-side native input backend is compiled in.
pub const NATIVE_INPUT_IMPLEMENTED: bool = true;

/// Largest number of `INPUT` records submitted in one `SendInput` call, so a
/// hostile text payload cannot build an unbounded batch.
const MAX_RECORDS_PER_SEND: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectError {
    /// Injection is not armed by local policy.
    NotArmed,
    /// The action cannot be injected by this backend.
    Unsupported,
    /// The OS refused part or all of the input.
    Rejected,
    /// The payload exceeds what one injection call may carry.
    TooLarge,
}

/// Virtual desktop bounds in physical pixels: origin x, origin y, width, height.
fn virtual_desktop() -> (i32, i32, i32, i32) {
    // Safety: all four metrics are pure reads with no preconditions.
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

fn send(records: &[INPUT]) -> Result<(), InjectError> {
    if records.is_empty() {
        return Ok(());
    }
    if records.len() > MAX_RECORDS_PER_SEND {
        return Err(InjectError::TooLarge);
    }
    // Safety: `records` is a live slice of initialized INPUT values and the
    // element size matches the struct `SendInput` expects.
    let sent = unsafe {
        SendInput(
            records.len() as UINT,
            records.as_ptr() as *mut INPUT,
            size_of::<INPUT>() as i32,
        )
    };
    if sent as usize == records.len() {
        Ok(())
    } else {
        Err(InjectError::Rejected)
    }
}

fn mouse_record(flags: DWORD, dx: i32, dy: i32, data: DWORD) -> INPUT {
    // Safety: zeroing an `INPUT` makes the union arm valid for any
    // interpretation, so the write below initializes the arm that `type_`
    // selects. `winapi` only exposes shared references through `u.mi()`, so the
    // fields are written through raw pointers to a zeroed slot instead.
    let mut slot: MaybeUninit<INPUT> = MaybeUninit::zeroed();
    let base = slot.as_mut_ptr();
    unsafe {
        std::ptr::addr_of_mut!((*base).u)
            .cast::<MOUSEINPUT>()
            .write(MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            });
        std::ptr::addr_of_mut!((*base).type_).write(INPUT_MOUSE);
        slot.assume_init()
    }
}

fn key_record(vk: u16, scan: u16, flags: DWORD) -> INPUT {
    // Safety: as above; the keyboard arm is the active one here.
    let mut slot: MaybeUninit<INPUT> = MaybeUninit::zeroed();
    let base = slot.as_mut_ptr();
    unsafe {
        std::ptr::addr_of_mut!((*base).u)
            .cast::<KEYBDINPUT>()
            .write(KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            });
        std::ptr::addr_of_mut!((*base).type_).write(INPUT_KEYBOARD);
        slot.assume_init()
    }
}

fn button_flags(button: MouseButton, down: bool) -> (DWORD, DWORD) {
    match button {
        MouseButton::Left => (
            if down {
                MOUSEEVENTF_LEFTDOWN
            } else {
                MOUSEEVENTF_LEFTUP
            },
            0,
        ),
        MouseButton::Right => (
            if down {
                MOUSEEVENTF_RIGHTDOWN
            } else {
                MOUSEEVENTF_RIGHTUP
            },
            0,
        ),
        MouseButton::Middle => (
            if down {
                MOUSEEVENTF_MIDDLEDOWN
            } else {
                MOUSEEVENTF_MIDDLEUP
            },
            0,
        ),
        MouseButton::Back => (
            if down {
                MOUSEEVENTF_XDOWN
            } else {
                MOUSEEVENTF_XUP
            },
            // `winapi` declares `XBUTTON1`/`XBUTTON2` as `WORD` while
            // `MOUSEINPUT::mouseData` is a `DWORD`; widen at the one place that
            // crosses the two, rather than at every use.
            DWORD::from(XBUTTON1),
        ),
        MouseButton::Forward => (
            if down {
                MOUSEEVENTF_XDOWN
            } else {
                MOUSEEVENTF_XUP
            },
            DWORD::from(XBUTTON2),
        ),
    }
}

/// Inject one decoded pointer action.
///
/// `armed` is the caller's explicit local policy decision. Injection is refused
/// when it is false, so a host that never enabled input cannot reach `SendInput`
/// even if a peer sends a pointer event.
pub fn inject_mouse(armed: bool, action: MouseAction) -> Result<(), InjectError> {
    if !armed {
        return Err(InjectError::NotArmed);
    }
    let record = match action {
        MouseAction::MoveAbsolute { x, y } => {
            let (origin_x, origin_y, width, height) = virtual_desktop();
            // A session without an interactive desktop reports no virtual screen.
            // Failing is correct: clamping every point to the origin would look
            // like a working cursor that never moves.
            if width <= 0 || height <= 0 {
                return Err(InjectError::Unsupported);
            }
            let (nx, ny) = normalize_absolute(x, y, origin_x, origin_y, width, height);
            mouse_record(
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                nx,
                ny,
                0,
            )
        }
        MouseAction::MoveRelative { dx, dy } => mouse_record(MOUSEEVENTF_MOVE, dx, dy, 0),
        MouseAction::ButtonDown(button) => {
            let (flags, data) = button_flags(button, true);
            mouse_record(flags, 0, 0, data)
        }
        MouseAction::ButtonUp(button) => {
            let (flags, data) = button_flags(button, false);
            mouse_record(flags, 0, 0, data)
        }
        MouseAction::Wheel { dx, dy } => {
            // One record carries one axis, so a two-axis step is two records.
            if dx != 0 && dy != 0 {
                return send(&[
                    mouse_record(MOUSEEVENTF_HWHEEL, 0, 0, dx as u32),
                    mouse_record(MOUSEEVENTF_WHEEL, 0, 0, dy as u32),
                ]);
            }
            if dx != 0 {
                mouse_record(MOUSEEVENTF_HWHEEL, 0, 0, dx as u32)
            } else {
                mouse_record(MOUSEEVENTF_WHEEL, 0, 0, dy as u32)
            }
        }
    };
    send(&[record])
}

/// Inject one decoded key action. `armed` is the same explicit local policy as
/// [`inject_mouse`].
pub fn inject_key(armed: bool, action: KeyAction) -> Result<(), InjectError> {
    if !armed {
        return Err(InjectError::NotArmed);
    }
    match action {
        KeyAction::Text(text) => inject_text(&text),
        KeyAction::Character { character, down } => inject_character(character, down),
        KeyAction::Control { key, down } => {
            let vk = windows_virtual_key(key).ok_or(InjectError::Unsupported)?;
            let flags = if down { 0 } else { KEYEVENTF_KEYUP };
            send(&[key_record(vk, 0, flags)])
        }
    }
}

/// Type a whole string through `KEYEVENTF_UNICODE`.
///
/// This is the IME/CJK path: it delivers Unicode code units to the focused window
/// instead of relying on the peer's keyboard layout, which is what the original
/// `KeyEvent.seq` semantics promise. Encoding to UTF-16 also produces the
/// surrogate pair a non-BMP character needs.
pub fn inject_text(text: &str) -> Result<(), InjectError> {
    if text.is_empty() {
        return Ok(());
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() > crate::input::MAX_TEXT_BYTES {
        return Err(InjectError::TooLarge);
    }
    let mut records = Vec::with_capacity(units.len().saturating_mul(2));
    for unit in units {
        // A zero virtual key with KEYEVENTF_UNICODE is the documented "type this
        // UTF-16 unit" form.
        records.push(key_record(0, unit, KEYEVENTF_UNICODE));
        records.push(key_record(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
    }
    for chunk in records.chunks(MAX_RECORDS_PER_SEND) {
        send(chunk)?;
    }
    Ok(())
}

/// Inject a single printable character.
///
/// A character outside the BMP needs a surrogate pair, so it goes through the
/// text path; its key-up half is already covered there.
fn inject_character(character: char, down: bool) -> Result<(), InjectError> {
    if (character as u32) > 0xFFFF {
        return if down {
            inject_text(&character.to_string())
        } else {
            Ok(())
        };
    }
    let unit = character as u16;
    let flags = if down {
        KEYEVENTF_UNICODE
    } else {
        KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
    };
    send(&[key_record(0, unit, flags)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use hbb_common::message_proto::ControlKey;

    #[test]
    fn injection_is_refused_while_disarmed() {
        assert_eq!(
            inject_mouse(false, MouseAction::MoveRelative { dx: 1, dy: 1 }),
            Err(InjectError::NotArmed)
        );
        assert_eq!(
            inject_key(
                false,
                KeyAction::Control {
                    key: ControlKey::Return,
                    down: true
                }
            ),
            Err(InjectError::NotArmed)
        );
        // An armed call reaches the OS instead of being refused locally. The
        // result itself is not asserted because a headless test session has no
        // interactive desktop to aim at.
        assert_ne!(
            inject_mouse(true, MouseAction::MoveAbsolute { x: 0, y: 0 }),
            Err(InjectError::NotArmed)
        );
    }

    #[test]
    fn secure_attention_and_mobile_keys_are_refused_by_the_injector() {
        for key in [
            ControlKey::CtrlAltDel,
            ControlKey::LockScreen,
            ControlKey::Unknown,
            ControlKey::Power,
        ] {
            assert_eq!(
                inject_key(true, KeyAction::Control { key, down: true }),
                Err(InjectError::Unsupported),
                "{key:?} must be refused, not remapped"
            );
        }
    }

    #[test]
    fn an_oversized_text_payload_is_rejected_before_building_records() {
        let oversized = "a".repeat(crate::input::MAX_TEXT_BYTES + 1);
        assert_eq!(inject_text(&oversized), Err(InjectError::TooLarge));
        // An empty string is a no-op, not an error.
        assert_eq!(inject_text(""), Ok(()));
    }

    #[test]
    fn a_missing_virtual_desktop_is_reported_rather_than_aimed_at_a_corner() {
        // The desktop metrics are only meaningful on an interactive desktop; in a
        // session without one they read as zero and an absolute move must fail
        // instead of clamping every point to the origin.
        let (_, _, width, height) = virtual_desktop();
        if width <= 0 || height <= 0 {
            assert_eq!(
                inject_mouse(true, MouseAction::MoveAbsolute { x: 10, y: 10 }),
                Err(InjectError::Unsupported)
            );
        }
    }

    #[test]
    fn wheel_axes_map_to_their_documented_input_flags() {
        assert_ne!(MOUSEEVENTF_WHEEL, MOUSEEVENTF_HWHEEL);
        let vertical = mouse_record(MOUSEEVENTF_WHEEL, 0, 0, 1);
        let horizontal = mouse_record(MOUSEEVENTF_HWHEEL, 0, 0, 1);
        assert_eq!(vertical.type_, INPUT_MOUSE);
        assert_eq!(horizontal.type_, INPUT_MOUSE);
        // Safety: the mouse arm was just written by `mouse_record`.
        unsafe {
            assert_eq!(vertical.u.mi().dwFlags, MOUSEEVENTF_WHEEL);
            assert_eq!(horizontal.u.mi().dwFlags, MOUSEEVENTF_HWHEEL);
        }
    }
}
