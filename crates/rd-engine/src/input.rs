//! Controlled-side input decoding and injection policy.
//!
//! The original protocol delivers input as `MouseEvent`/`KeyEvent`/
//! `PointerDeviceEvent`. Decoding those into a small, platform-neutral action
//! set keeps the wire contract testable on every host and keeps the Win32
//! injection surface tiny.
//!
//! Injection is never implied by successful authentication. A host must arm it
//! explicitly, and the peer must still hold the granted permission.

use hbb_common::message_proto::{ControlKey, KeyEvent, KeyboardMode, MouseEvent, message};

/// Original-protocol mouse event kinds (`mask & MOUSE_TYPE_MASK`).
pub const MOUSE_TYPE_MOVE: i32 = 0;
pub const MOUSE_TYPE_DOWN: i32 = 1;
pub const MOUSE_TYPE_UP: i32 = 2;
pub const MOUSE_TYPE_WHEEL: i32 = 3;
pub const MOUSE_TYPE_TRACKPAD: i32 = 4;
pub const MOUSE_TYPE_MOVE_RELATIVE: i32 = 5;
pub const MOUSE_TYPE_MASK: i32 = 0x7;

pub const MOUSE_BUTTON_LEFT: i32 = 0x01;
pub const MOUSE_BUTTON_RIGHT: i32 = 0x02;
pub const MOUSE_BUTTON_WHEEL: i32 = 0x04;
pub const MOUSE_BUTTON_BACK: i32 = 0x08;
pub const MOUSE_BUTTON_FORWARD: i32 = 0x10;

/// Largest accepted pointer coordinate magnitude. The protocol uses `sint32`;
/// a virtual desktop never exceeds a small fraction of this.
const MAX_ABSOLUTE_COORDINATE: i32 = 1 << 20;
/// Largest accepted single wheel/trackpad delta.
const MAX_WHEEL_DELTA: i32 = 1 << 14;
/// Number of mouse buttons any single event may carry.
const MAX_BUTTON_BITS: i32 = 0x1f;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Platform-neutral pointer action. Absolute actions carry protocol desktop
/// coordinates; relative actions carry signed deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    MoveAbsolute {
        x: i32,
        y: i32,
    },
    MoveRelative {
        dx: i32,
        dy: i32,
    },
    ButtonDown(MouseButton),
    ButtonUp(MouseButton),
    /// A vertical/horizontal wheel step. Sign follows the protocol: a positive
    /// `dy` scrolls the peer's content up.
    Wheel {
        dx: i32,
        dy: i32,
    },
}

/// One original-protocol key name resolved to its wire identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// A whole string, delivered through `KeyEvent.seq`. This is the only
    /// correct path for IME/CJK text, which has no key identity.
    Text(String),
    /// A printable character that the peer's own layout must resolve.
    Character { character: char, down: bool },
    /// A named control key.
    Control { key: ControlKey, down: bool },
}

/// Decode an original-protocol `MouseEvent`. `None` means the event is
/// malformed and must be dropped rather than guessed.
pub fn decode_mouse(event: &MouseEvent) -> Option<MouseAction> {
    let kind = event.mask & MOUSE_TYPE_MASK;
    let buttons = event.mask >> 3;
    if buttons < 0 || buttons > MAX_BUTTON_BITS {
        return None;
    }
    match kind {
        MOUSE_TYPE_MOVE => {
            check_absolute(event.x, event.y)?;
            Some(MouseAction::MoveAbsolute {
                x: event.x,
                y: event.y,
            })
        }
        MOUSE_TYPE_MOVE_RELATIVE => {
            check_delta(event.x)?;
            check_delta(event.y)?;
            Some(MouseAction::MoveRelative {
                dx: event.x,
                dy: event.y,
            })
        }
        MOUSE_TYPE_DOWN | MOUSE_TYPE_UP => {
            let button = decode_button(buttons)?;
            // A button transition still carries the pointer position.
            check_absolute(event.x, event.y)?;
            Some(if kind == MOUSE_TYPE_DOWN {
                MouseAction::ButtonDown(button)
            } else {
                MouseAction::ButtonUp(button)
            })
        }
        MOUSE_TYPE_WHEEL | MOUSE_TYPE_TRACKPAD => {
            // The original protocol negates the wheel X axis before injecting:
            // a positive protocol x is a left scroll on the peer.
            // `checked_neg` because `i32::MIN` has no positive counterpart.
            let dx = event.x.checked_neg()?;
            check_delta(dx)?;
            check_delta(event.y)?;
            Some(MouseAction::Wheel { dx, dy: event.y })
        }
        _ => None,
    }
}

fn decode_button(bits: i32) -> Option<MouseButton> {
    // Exactly one button per transition. A combined mask would inject a chord
    // the user never pressed.
    match bits {
        MOUSE_BUTTON_LEFT => Some(MouseButton::Left),
        MOUSE_BUTTON_RIGHT => Some(MouseButton::Right),
        MOUSE_BUTTON_WHEEL => Some(MouseButton::Middle),
        MOUSE_BUTTON_BACK => Some(MouseButton::Back),
        MOUSE_BUTTON_FORWARD => Some(MouseButton::Forward),
        _ => None,
    }
}

fn check_absolute(x: i32, y: i32) -> Option<()> {
    // `unsigned_abs` also covers `i32::MIN`, where `abs` would overflow.
    (x.unsigned_abs() <= MAX_ABSOLUTE_COORDINATE as u32
        && y.unsigned_abs() <= MAX_ABSOLUTE_COORDINATE as u32)
        .then_some(())
}

fn check_delta(value: i32) -> Option<()> {
    (value.unsigned_abs() <= MAX_WHEEL_DELTA as u32).then_some(())
}

/// Decode an original-protocol `KeyEvent`. Only the modes this host implements
/// are accepted; an unknown mode is dropped instead of being treated as legacy.
pub fn decode_key(event: &KeyEvent) -> Option<KeyAction> {
    let mode = event.mode.enum_value().ok()?;
    if mode != KeyboardMode::Legacy {
        return None;
    }
    let down = event.down || event.press;
    match event.union.as_ref()? {
        hbb_common::message_proto::key_event::Union::Seq(text) => {
            if text.is_empty() || text.len() > MAX_TEXT_BYTES {
                return None;
            }
            Some(KeyAction::Text(text.clone()))
        }
        hbb_common::message_proto::key_event::Union::ControlKey(key) => {
            // `Unknown` is not a key; injecting it would press an arbitrary
            // virtual key on the peer.
            let key = key.enum_value().ok()?;
            if key == ControlKey::Unknown {
                return None;
            }
            Some(KeyAction::Control { key, down })
        }
        hbb_common::message_proto::key_event::Union::Chr(code) => {
            let character = char::from_u32(*code)?;
            // Control characters are not text; they must arrive as a control key.
            if character.is_control() {
                return None;
            }
            Some(KeyAction::Character { character, down })
        }
        // `unicode` and `win2win_hotkey` are not produced by the layout the
        // viewer encodes, so accepting them would guess at a key identity.
        _ => None,
    }
}

/// Bound for a single injected text payload.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;

/// Decoded inbound input, ready for a platform injector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    Mouse(MouseAction),
    Key(KeyAction),
}

/// Windows virtual-key codes the original `ControlKey` values map onto.
///
/// These are plain numbers so the mapping is decided and tested on every
/// platform; only the injector needs the Windows headers. The values are the
/// documented `VK_*` constants.
pub mod vk {
    pub const BACK: u16 = 0x08;
    pub const TAB: u16 = 0x09;
    pub const CLEAR: u16 = 0x0c;
    pub const RETURN: u16 = 0x0d;
    pub const SHIFT: u16 = 0x10;
    pub const CONTROL: u16 = 0x11;
    pub const MENU: u16 = 0x12;
    pub const PAUSE: u16 = 0x13;
    pub const CAPITAL: u16 = 0x14;
    pub const KANA: u16 = 0x15;
    pub const HANGEUL: u16 = 0x15;
    pub const JUNJA: u16 = 0x17;
    pub const FINAL: u16 = 0x18;
    pub const HANJA: u16 = 0x19;
    pub const KANJI: u16 = 0x19;
    pub const ESCAPE: u16 = 0x1b;
    pub const SPACE: u16 = 0x20;
    pub const PRIOR: u16 = 0x21;
    pub const NEXT: u16 = 0x22;
    pub const END: u16 = 0x23;
    pub const HOME: u16 = 0x24;
    pub const LEFT: u16 = 0x25;
    pub const UP: u16 = 0x26;
    pub const RIGHT: u16 = 0x27;
    pub const DOWN: u16 = 0x28;
    pub const SELECT: u16 = 0x29;
    pub const PRINT: u16 = 0x2a;
    pub const EXECUTE: u16 = 0x2b;
    pub const SNAPSHOT: u16 = 0x2c;
    pub const INSERT: u16 = 0x2d;
    pub const DELETE: u16 = 0x2e;
    pub const HELP: u16 = 0x2f;
    pub const LWIN: u16 = 0x5b;
    pub const RWIN: u16 = 0x5c;
    pub const APPS: u16 = 0x5d;
    pub const SLEEP: u16 = 0x5f;
    pub const NUMPAD0: u16 = 0x60;
    pub const NUMPAD1: u16 = 0x61;
    pub const NUMPAD2: u16 = 0x62;
    pub const NUMPAD3: u16 = 0x63;
    pub const NUMPAD4: u16 = 0x64;
    pub const NUMPAD5: u16 = 0x65;
    pub const NUMPAD6: u16 = 0x66;
    pub const NUMPAD7: u16 = 0x67;
    pub const NUMPAD8: u16 = 0x68;
    pub const NUMPAD9: u16 = 0x69;
    pub const MULTIPLY: u16 = 0x6a;
    pub const ADD: u16 = 0x6b;
    pub const SEPARATOR: u16 = 0x6c;
    pub const SUBTRACT: u16 = 0x6d;
    pub const DECIMAL: u16 = 0x6e;
    pub const DIVIDE: u16 = 0x6f;
    pub const F1: u16 = 0x70;
    pub const F2: u16 = 0x71;
    pub const F3: u16 = 0x72;
    pub const F4: u16 = 0x73;
    pub const F5: u16 = 0x74;
    pub const F6: u16 = 0x75;
    pub const F7: u16 = 0x76;
    pub const F8: u16 = 0x77;
    pub const F9: u16 = 0x78;
    pub const F10: u16 = 0x79;
    pub const F11: u16 = 0x7a;
    pub const F12: u16 = 0x7b;
    pub const NUMLOCK: u16 = 0x90;
    pub const SCROLL: u16 = 0x91;
    pub const LSHIFT: u16 = 0xa0;
    pub const RSHIFT: u16 = 0xa1;
    pub const LCONTROL: u16 = 0xa2;
    pub const RCONTROL: u16 = 0xa3;
    pub const LMENU: u16 = 0xa4;
    pub const RMENU: u16 = 0xa5;
    pub const OEM_NEC_EQUAL: u16 = 0x92;
    pub const VOLUME_MUTE: u16 = 0xad;
    pub const VOLUME_DOWN: u16 = 0xae;
    pub const VOLUME_UP: u16 = 0xaf;
}

/// Original-protocol control key to its documented Windows virtual key.
///
/// `CtrlAltDel` (100) and `LockScreen` (101) are Secure Attention Sequences:
/// Windows refuses them through `SendInput`, so they are refused here instead of
/// being remapped onto a different chord. `Power` targets a mobile controlled
/// side and has no Windows virtual key.
pub fn windows_virtual_key(key: ControlKey) -> Option<u16> {
    use ControlKey::*;
    Some(match key {
        Unknown => return None,
        Alt => vk::MENU,
        Backspace => vk::BACK,
        CapsLock => vk::CAPITAL,
        Control => vk::CONTROL,
        Delete => vk::DELETE,
        DownArrow => vk::DOWN,
        End => vk::END,
        Escape => vk::ESCAPE,
        F1 => vk::F1,
        F2 => vk::F2,
        F3 => vk::F3,
        F4 => vk::F4,
        F5 => vk::F5,
        F6 => vk::F6,
        F7 => vk::F7,
        F8 => vk::F8,
        F9 => vk::F9,
        F10 => vk::F10,
        F11 => vk::F11,
        F12 => vk::F12,
        Home => vk::HOME,
        LeftArrow => vk::LEFT,
        Meta => vk::LWIN,
        // `Option` and `Menu` are deprecated upstream in favour of `Alt`.
        Option | Menu => vk::MENU,
        PageDown => vk::NEXT,
        PageUp => vk::PRIOR,
        Return => vk::RETURN,
        RightArrow => vk::RIGHT,
        Shift => vk::SHIFT,
        Space => vk::SPACE,
        Tab => vk::TAB,
        UpArrow => vk::UP,
        Numpad0 => vk::NUMPAD0,
        Numpad1 => vk::NUMPAD1,
        Numpad2 => vk::NUMPAD2,
        Numpad3 => vk::NUMPAD3,
        Numpad4 => vk::NUMPAD4,
        Numpad5 => vk::NUMPAD5,
        Numpad6 => vk::NUMPAD6,
        Numpad7 => vk::NUMPAD7,
        Numpad8 => vk::NUMPAD8,
        Numpad9 => vk::NUMPAD9,
        NumpadEnter => vk::RETURN,
        Cancel => vk::CLEAR,
        Clear => vk::CLEAR,
        Pause => vk::PAUSE,
        Kana | Convert => vk::KANA,
        Hangul => vk::HANGEUL,
        Junja => vk::JUNJA,
        Final => vk::FINAL,
        Hanja | Kanji => vk::HANJA,
        Select => vk::SELECT,
        Print | Snapshot => vk::SNAPSHOT,
        Execute => vk::EXECUTE,
        Insert => vk::INSERT,
        Help => vk::HELP,
        Sleep => vk::SLEEP,
        Separator => vk::SEPARATOR,
        Scroll => vk::SCROLL,
        NumLock => vk::NUMLOCK,
        RWin => vk::RWIN,
        Apps => vk::APPS,
        Multiply => vk::MULTIPLY,
        Add => vk::ADD,
        Subtract => vk::SUBTRACT,
        Decimal => vk::DECIMAL,
        Divide => vk::DIVIDE,
        Equals => vk::OEM_NEC_EQUAL,
        RShift => vk::RSHIFT,
        RControl => vk::RCONTROL,
        RAlt => vk::RMENU,
        VolumeMute => vk::VOLUME_MUTE,
        VolumeUp => vk::VOLUME_UP,
        VolumeDown => vk::VOLUME_DOWN,
        Power => return None,
        CtrlAltDel | LockScreen => return None,
    })
}

/// Normalize an absolute virtual-desktop point into the `SendInput`
/// `MOUSEEVENTF_ABSOLUTE|MOUSEEVENTF_VIRTUALDESK` range (0..=65535). Points
/// outside the desktop are clamped, never wrapped onto the opposite edge.
pub fn normalize_absolute(
    x: i32,
    y: i32,
    origin_x: i32,
    origin_y: i32,
    width: i32,
    height: i32,
) -> (i32, i32) {
    const ABSOLUTE_MAX: i64 = 65_535;
    let span_x = (i64::from(width) - 1).max(1);
    let span_y = (i64::from(height) - 1).max(1);
    let rel_x = (i64::from(x) - i64::from(origin_x)).clamp(0, span_x);
    let rel_y = (i64::from(y) - i64::from(origin_y)).clamp(0, span_y);
    (
        ((rel_x * ABSOLUTE_MAX) / span_x) as i32,
        ((rel_y * ABSOLUTE_MAX) / span_y) as i32,
    )
}

/// Decode one inbound message. Returns `None` for anything that is not input or
/// that this host must not act on.
pub fn decode_message(message: &message::Union) -> Option<InputAction> {
    match message {
        message::Union::MouseEvent(event) => decode_mouse(event).map(InputAction::Mouse),
        message::Union::KeyEvent(event) => decode_key(event).map(InputAction::Key),
        // Touch gestures are expanded by the viewer into pointer events today;
        // accepting the raw gesture stream here would double-apply them.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hbb_common::message_proto::KeyEvent;

    #[test]
    fn absolute_normalization_spans_the_whole_virtual_desktop() {
        assert_eq!(normalize_absolute(0, 0, 0, 0, 1920, 1080), (0, 0));
        assert_eq!(
            normalize_absolute(1919, 1079, 0, 0, 1920, 1080),
            (65_535, 65_535)
        );
        // A virtual desktop whose origin is negative (a monitor left of the
        // primary) still maps its first pixel to zero.
        assert_eq!(
            normalize_absolute(-1920, -200, -1920, -200, 3840, 1280),
            (0, 0)
        );
    }

    #[test]
    fn points_outside_the_desktop_are_clamped_not_wrapped() {
        assert_eq!(normalize_absolute(-5000, -5000, 0, 0, 1920, 1080), (0, 0));
        assert_eq!(
            normalize_absolute(9999, 9999, 0, 0, 1920, 1080),
            (65_535, 65_535)
        );
    }

    #[test]
    fn secure_attention_sequences_are_refused_rather_than_remapped() {
        assert_eq!(windows_virtual_key(ControlKey::CtrlAltDel), None);
        assert_eq!(windows_virtual_key(ControlKey::LockScreen), None);
        assert_eq!(windows_virtual_key(ControlKey::Unknown), None);
        assert_eq!(windows_virtual_key(ControlKey::Power), None);
        assert_eq!(windows_virtual_key(ControlKey::Return), Some(vk::RETURN));
    }

    #[test]
    fn side_specific_modifiers_map_to_their_own_virtual_keys() {
        assert_ne!(vk::SHIFT, vk::RSHIFT);
        assert_ne!(vk::CONTROL, vk::RCONTROL);
        assert_ne!(vk::MENU, vk::RMENU);
        assert_ne!(vk::LWIN, vk::RWIN);
        assert_eq!(windows_virtual_key(ControlKey::RShift), Some(vk::RSHIFT));
        assert_eq!(
            windows_virtual_key(ControlKey::RControl),
            Some(vk::RCONTROL)
        );
        assert_eq!(windows_virtual_key(ControlKey::RAlt), Some(vk::RMENU));
        assert_eq!(windows_virtual_key(ControlKey::RWin), Some(vk::RWIN));
        // The generic keys stay generic: the protocol does not name a side.
        assert_eq!(windows_virtual_key(ControlKey::Shift), Some(vk::SHIFT));
        assert_eq!(windows_virtual_key(ControlKey::Control), Some(vk::CONTROL));
        assert_eq!(windows_virtual_key(ControlKey::Alt), Some(vk::MENU));
        assert_eq!(windows_virtual_key(ControlKey::Meta), Some(vk::LWIN));
    }

    #[test]
    fn every_control_key_is_either_mapped_or_explicitly_refused() {
        // `windows_virtual_key` matches exhaustively, so a new protocol key is a
        // compile error rather than a silent fallthrough. This test pins the
        // refusal set so an accidental remap of a Secure Attention Sequence or a
        // mobile-only key cannot pass review.
        const REFUSED: [ControlKey; 4] = [
            ControlKey::Unknown,
            ControlKey::Power,
            ControlKey::CtrlAltDel,
            ControlKey::LockScreen,
        ];
        for key in REFUSED {
            assert_eq!(
                windows_virtual_key(key),
                None,
                "{key:?} must be explicitly refused"
            );
        }
        const MAPPED: [ControlKey; 12] = [
            ControlKey::Return,
            ControlKey::Tab,
            ControlKey::Escape,
            ControlKey::Backspace,
            ControlKey::Space,
            ControlKey::F12,
            ControlKey::Numpad9,
            ControlKey::NumpadEnter,
            ControlKey::LeftArrow,
            ControlKey::VolumeUp,
            ControlKey::Kana,
            ControlKey::NumLock,
        ];
        for key in MAPPED {
            assert!(
                windows_virtual_key(key).is_some(),
                "{key:?} must map to a virtual key"
            );
        }
    }

    fn mouse(mask: i32, x: i32, y: i32) -> MouseEvent {
        MouseEvent {
            mask,
            x,
            y,
            ..Default::default()
        }
    }

    #[test]
    fn mouse_kinds_decode_or_are_dropped_never_guessed() {
        assert_eq!(
            decode_mouse(&mouse(MOUSE_TYPE_MOVE, 10, 20)),
            Some(MouseAction::MoveAbsolute { x: 10, y: 20 })
        );
        assert_eq!(
            decode_mouse(&mouse((MOUSE_BUTTON_LEFT << 3) | MOUSE_TYPE_DOWN, 1, 2)),
            Some(MouseAction::ButtonDown(MouseButton::Left))
        );
        assert_eq!(
            decode_mouse(&mouse((MOUSE_BUTTON_RIGHT << 3) | MOUSE_TYPE_UP, 1, 2)),
            Some(MouseAction::ButtonUp(MouseButton::Right))
        );
        // A chord is not a single button transition.
        assert_eq!(
            decode_mouse(&mouse(
                ((MOUSE_BUTTON_LEFT | MOUSE_BUTTON_RIGHT) << 3) | MOUSE_TYPE_DOWN,
                0,
                0
            )),
            None
        );
        // Unknown kind and out-of-range coordinates are rejected.
        assert_eq!(decode_mouse(&mouse(6, 0, 0)), None);
        assert_eq!(
            decode_mouse(&mouse(MOUSE_TYPE_MOVE, MAX_ABSOLUTE_COORDINATE + 1, 0)),
            None
        );
        // A negative coordinate is legal: the virtual desktop can start left of
        // the primary monitor, so only the magnitude is bounded.
        assert_eq!(
            decode_mouse(&mouse(MOUSE_TYPE_MOVE, -1, -1)),
            Some(MouseAction::MoveAbsolute { x: -1, y: -1 })
        );
        assert_eq!(
            decode_mouse(&mouse(MOUSE_TYPE_MOVE, i32::MIN, 0)),
            None,
            "i32::MIN must not panic or wrap into a valid coordinate"
        );
    }

    #[test]
    fn wheel_negates_the_protocol_horizontal_axis() {
        // A positive protocol x is a left scroll on the peer.
        assert_eq!(
            decode_mouse(&mouse(MOUSE_TYPE_WHEEL, 3, 0)),
            Some(MouseAction::Wheel { dx: -3, dy: 0 })
        );
        assert_eq!(
            decode_mouse(&mouse(MOUSE_TYPE_WHEEL, 0, -2)),
            Some(MouseAction::Wheel { dx: 0, dy: -2 })
        );
        assert_eq!(
            decode_mouse(&mouse(MOUSE_TYPE_WHEEL, i32::MIN, 0)),
            None,
            "negating i32::MIN must not overflow"
        );
    }

    #[test]
    fn relative_movement_keeps_signed_deltas_and_rejects_huge_ones() {
        assert_eq!(
            decode_mouse(&mouse(MOUSE_TYPE_MOVE_RELATIVE, -4, 7)),
            Some(MouseAction::MoveRelative { dx: -4, dy: 7 })
        );
        assert_eq!(
            decode_mouse(&mouse(MOUSE_TYPE_MOVE_RELATIVE, MAX_WHEEL_DELTA + 1, 0)),
            None
        );
    }

    #[test]
    fn key_events_require_an_implemented_mode_and_a_known_identity() {
        let mut event = KeyEvent::new();
        event.mode = KeyboardMode::Legacy.into();
        event.down = true;
        event.set_control_key(ControlKey::Return);
        assert_eq!(
            decode_key(&event),
            Some(KeyAction::Control {
                key: ControlKey::Return,
                down: true
            })
        );

        // Translate/Map modes are not implemented by this host; guessing legacy
        // semantics for them would inject the wrong key.
        event.mode = KeyboardMode::Translate.into();
        assert_eq!(decode_key(&event), None);

        event.mode = KeyboardMode::Legacy.into();
        event.set_seq("你好".into());
        assert_eq!(decode_key(&event), Some(KeyAction::Text("你好".into())));

        event.set_chr('a' as u32);
        assert_eq!(
            decode_key(&event),
            Some(KeyAction::Character {
                character: 'a',
                down: true
            })
        );

        // A control character belongs to a control key, not to text.
        event.set_chr(0x1b);
        assert_eq!(decode_key(&event), None);
        event.set_chr(0x110000);
        assert_eq!(decode_key(&event), None);
    }

    #[test]
    fn non_input_messages_are_never_treated_as_input() {
        assert_eq!(
            decode_message(&message::Union::CursorPosition(Default::default())),
            None
        );
        assert_eq!(
            decode_message(&message::Union::AudioFrame(Default::default())),
            None
        );
    }
}
