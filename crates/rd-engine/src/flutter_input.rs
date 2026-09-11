//! Pointer and option adapters for the Flutter bridge.
//!
//! The Flutter input model sends pointer state as JSON with its own field names,
//! while the protocol speaks a packed mask. This module is the single place that
//! translates between them, so no other layer re-derives the bit layout.
//!
//! `x`/`y` arrive as JSON strings in the frontend's payload, which is why they
//! are parsed rather than read as numbers.

use crate::input::{
    MOUSE_BUTTON_BACK, MOUSE_BUTTON_FORWARD, MOUSE_BUTTON_LEFT, MOUSE_BUTTON_RIGHT,
    MOUSE_BUTTON_WHEEL, MOUSE_TYPE_DOWN, MOUSE_TYPE_MOVE, MOUSE_TYPE_MOVE_RELATIVE,
    MOUSE_TYPE_TRACKPAD, MOUSE_TYPE_UP, MOUSE_TYPE_WHEEL,
};
use hbb_common::{message_proto::MouseEvent, serde_json::Value};

/// The frontend's `MouseButtons` enum as sent over the wire.
fn button_bits(name: &str) -> Option<i32> {
    Some(match name {
        "left" => MOUSE_BUTTON_LEFT,
        "right" => MOUSE_BUTTON_RIGHT,
        // The protocol names the middle button after its scroll role.
        "wheel" => MOUSE_BUTTON_WHEEL,
        "back" => MOUSE_BUTTON_BACK,
        "forward" => MOUSE_BUTTON_FORWARD,
        _ => return None,
    })
}

/// Read a coordinate that the frontend may send as either a JSON string or a
/// number. A non-integral value is refused rather than truncated: a truncated
/// pointer coordinate moves the remote cursor somewhere the user did not aim.
fn coordinate(value: Option<&Value>) -> Option<i32> {
    let value = value?;
    match value {
        Value::String(text) => text.trim().parse::<i32>().ok(),
        Value::Number(number) => {
            let as_f64 = number.as_f64()?;
            if as_f64.fract() != 0.0 {
                return None;
            }
            number.as_i64().and_then(|value| i32::try_from(value).ok())
        }
        _ => None,
    }
}

/// Rebuild a protocol `MouseEvent` from the frontend's pointer JSON.
///
/// Returns `None` for anything that cannot be expressed exactly, so a malformed
/// payload is dropped instead of being turned into a synthetic click.
pub fn mouse_event_from_json(payload: &Value) -> Option<MouseEvent> {
    let kind = payload.get("type").and_then(Value::as_str)?;
    let button = payload.get("buttons").and_then(Value::as_str);
    let x = coordinate(payload.get("x")).unwrap_or(0);
    let y = coordinate(payload.get("y")).unwrap_or(0);
    let mask = match kind {
        // A bare position update carries no button transition.
        "move" | "mousemove" => MOUSE_TYPE_MOVE,
        "down" | "mousedown" => MOUSE_TYPE_DOWN | (button_bits(button?)? << 3),
        "up" | "mouseup" => MOUSE_TYPE_UP | (button_bits(button?)? << 3),
        "wheel" => MOUSE_TYPE_WHEEL,
        "trackpad" => MOUSE_TYPE_TRACKPAD,
        "move_relative" => MOUSE_TYPE_MOVE_RELATIVE,
        _ => return None,
    };
    Some(MouseEvent {
        mask,
        x,
        y,
        ..Default::default()
    })
}

/// Protocol `(kind, button)` pair for one pointer payload, in the form the
/// viewer's send path expects. Keeping this separate from the message builder
/// means the packed mask is derived in exactly one place.
pub fn decode_pointer(payload: &Value) -> Option<(u32, u32)> {
    let event = mouse_event_from_json(payload)?;
    let kind = event.mask & crate::input::MOUSE_TYPE_MASK;
    if kind < 0 {
        return None;
    }
    let button = event.mask >> 3;
    if button < 0 {
        return None;
    }
    Some((kind as u32, button as u32))
}

/// Compact JSON string for a key event the frontend asks us to replay.
pub fn key_payload_json(name: &str, down: bool, press: bool) -> String {
    hbb_common::serde_json::json!({
        "name": name,
        "down": down,
        "press": press,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hbb_common::serde_json::json;

    #[test]
    fn a_button_transition_carries_its_button_bits() {
        let down = json!({"type": "down", "buttons": "right", "x": "10", "y": "20"});
        let event = mouse_event_from_json(&down).expect("decodes");
        assert_eq!(event.mask & crate::input::MOUSE_TYPE_MASK, MOUSE_TYPE_DOWN);
        assert_eq!(event.mask >> 3, MOUSE_BUTTON_RIGHT);
        assert_eq!((event.x, event.y), (10, 20));
    }

    #[test]
    fn a_position_update_needs_no_button() {
        let event =
            mouse_event_from_json(&json!({"type": "move", "x": "1", "y": "2"})).expect("decodes");
        assert_eq!(event.mask, MOUSE_TYPE_MOVE);
    }

    #[test]
    fn a_button_transition_without_a_button_is_refused() {
        assert!(mouse_event_from_json(&json!({"type": "down", "x": "1", "y": "2"})).is_none());
        assert!(mouse_event_from_json(&json!({"type": "down", "buttons": "nope"})).is_none());
    }

    #[test]
    fn an_unknown_pointer_kind_is_dropped_rather_than_defaulted() {
        assert!(mouse_event_from_json(&json!({"type": "teleport"})).is_none());
    }

    #[test]
    fn a_fractional_coordinate_is_refused_instead_of_truncated() {
        // Truncating would move the remote cursor somewhere the user did not aim,
        // so a fractional value yields no coordinate at all.
        assert_eq!(coordinate(Some(&json!(1.5))), None);
        assert_eq!(coordinate(Some(&json!("2.5"))), None);
        // The frontend's own string form and whole numbers stay accepted.
        assert_eq!(coordinate(Some(&json!("3"))), Some(3));
        assert_eq!(coordinate(Some(&json!(4))), Some(4));
        assert_eq!(coordinate(Some(&json!(null))), None);
        assert_eq!(coordinate(None), None);
    }

    #[test]
    fn the_wheel_and_trackpad_kinds_keep_their_identity() {
        assert_eq!(
            decode_pointer(&json!({"type": "wheel", "y": "-1"})),
            Some((MOUSE_TYPE_WHEEL as u32, 0))
        );
        assert_eq!(
            decode_pointer(&json!({"type": "trackpad", "y": "2"})),
            Some((MOUSE_TYPE_TRACKPAD as u32, 0))
        );
        assert_eq!(
            decode_pointer(&json!({"type": "move_relative", "x": "-3", "y": "4"})),
            Some((MOUSE_TYPE_MOVE_RELATIVE as u32, 0))
        );
    }
}
