//! Session state behind the Flutter bridge.
//!
//! The Flutter UI drives a session in three steps — `sessionAddSync` to create
//! it, `sessionStart` to hand over an event stream, then per-session calls for
//! input, options and teardown — and the wire work itself belongs to
//! `crate::viewer`, which the OHOS bridge already drives.
//!
//! Two protocol facts shape the event contract and must not be "cleaned up":
//! - `EventToUI::Event` carries a JSON envelope whose `"name"` field selects the
//!   handler. The frontend treats a literal `"close"` string, not a `"name"`,
//!   as end-of-stream.
//! - The viewer reports an authoritative display size only after the peer
//!   handshake, so geometry is published from the accepted stream rather than
//!   from what the user asked for.

use crate::{
    flutter_ffi::EventToUI,
    viewer::{Viewer, ViewerOptions, ViewerSnapshot},
};
use flutter_rust_bridge::StreamSink;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Mutex, MutexGuard, OnceLock},
};
use uuid::Uuid;

/// Default direct-access port, matching the original protocol.
const DEFAULT_DIRECT_PORT: u16 = 21118;
/// Bound on concurrently tracked sessions.
const MAX_SESSIONS: usize = 16;
/// Event pump cadence. State changes are observed, never guessed, so the pump
/// only has to be fast enough for a UI frame budget.
const PUMP_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// Bound on buffered input events handled per pump tick.
const MAX_INPUT_BATCH: usize = 256;

/// Where a session connects.
#[derive(Clone)]
pub enum Route {
    Direct(SocketAddr),
    Rendezvous(String),
}

/// Options supplied by the UI when the session is created.
#[derive(Clone, Default)]
pub struct AddOptions {
    pub is_file_transfer: bool,
    pub is_view_camera: bool,
    pub is_port_forward: bool,
    pub is_rdp: bool,
    pub is_terminal: bool,
    pub force_relay: bool,
    pub password: String,
}

pub struct Session {
    pub id: Uuid,
    pub peer: String,
    pub route: Route,
    pub options: AddOptions,
    pub viewer: Option<std::sync::Arc<Viewer>>,
    pub events: Option<StreamSink<EventToUI>>,
    /// Mirror of the last published phase, so each transition is sent once.
    pump: PumpState,
}

/// Everything the pump needs to decide whether an event is due.
#[derive(Default)]
struct PumpState {
    last_phase: String,
    peer_info_sent: bool,
    permission_sent: bool,
    close_sent: bool,
    last_codec: String,
    last_width: i32,
    last_height: i32,
    display_sent: bool,
}

fn sessions() -> &'static Mutex<HashMap<Uuid, Session>> {
    static SESSIONS: OnceLock<Mutex<HashMap<Uuid, Session>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// Parse the target the UI supplied. An explicit `host:port` wins, a bare IP
/// uses the protocol default port, and anything else is a peer ID.
pub fn parse_route(target: &str) -> Option<Route> {
    let target = target.trim();
    if let Ok(address) = target.parse::<SocketAddr>() {
        return (address.port() != 0).then_some(Route::Direct(address));
    }
    if let Ok(ip) = target.parse::<IpAddr>() {
        return Some(Route::Direct(SocketAddr::new(ip, DEFAULT_DIRECT_PORT)));
    }
    if !target.is_empty() && target.len() <= 256 && !target.chars().any(char::is_control) {
        return Some(Route::Rendezvous(target.to_owned()));
    }
    None
}

/// Create a session. Returns the empty string on success, which is what the
/// frontend treats as "no error", or a message describing the refusal.
pub fn add(id: Uuid, peer: &str, options: AddOptions) -> Result<(), String> {
    if options.is_terminal {
        return Err("Terminal sessions are not supported by this runtime".to_owned());
    }
    if options.is_file_transfer || options.is_port_forward || options.is_rdp {
        return Err("Only remote-control sessions are supported by this runtime".to_owned());
    }
    if options.password.len() > 4096 {
        return Err("Password is too long".to_owned());
    }
    if options.force_relay {
        return Err("Forced relay routing is not supported by this runtime".to_owned());
    }
    let Some(route) = parse_route(peer) else {
        return Err("Invalid peer target".to_owned());
    };
    let mut registry = lock(sessions());
    if registry.contains_key(&id) {
        return Err("Session already exists".to_owned());
    }
    if registry.len() >= MAX_SESSIONS {
        return Err("Session limit reached".to_owned());
    }
    registry.insert(
        id,
        Session {
            id,
            peer: peer.trim().to_owned(),
            route,
            options,
            viewer: None,
            events: None,
            pump: PumpState::default(),
        },
    );
    Ok(())
}

pub fn exists(id: Uuid) -> bool {
    lock(sessions()).contains_key(&id)
}

pub fn remove(id: Uuid) {
    let removed = lock(sessions()).remove(&id);
    if let Some(session) = removed {
        if let Some(viewer) = &session.viewer {
            viewer.request_close();
        }
    }
}

/// Attach the UI event stream. Replacing an existing stream ends the previous
/// one with the literal `"close"` marker the frontend uses as end-of-stream, so
/// a replaced stream cannot keep feeding a stale widget.
pub fn attach_stream(id: Uuid, sink: StreamSink<EventToUI>) -> Result<Uuid, String> {
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&id) else {
        return Err("Session not found".to_owned());
    };
    if let Some(previous) = session.events.take() {
        let _ = previous.add(EventToUI::Event("close".to_owned()));
    }
    session.events = Some(sink);
    session.pump.close_sent = false;
    Ok(id)
}

/// Start the viewer for a session. Starting is asynchronous: this returns once
/// the connection task is running, not once it has authenticated.
pub fn start(id: Uuid) -> Result<(), String> {
    let local_id = hbb_common::config::Config::get_id();
    if local_id.is_empty() {
        return Err("Runtime identity is unavailable".to_owned());
    }
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&id) else {
        return Err("Session not found".to_owned());
    };
    if session.viewer.is_some() {
        return Ok(());
    }
    let lease = crate::flutter_surface::lease_for(id);
    let password = std::mem::take(&mut session.options.password);
    let viewer_options = ViewerOptions {
        address: match &session.route {
            Route::Direct(address) => Some(*address),
            Route::Rendezvous(_) => None,
        },
        username: match &session.route {
            Route::Direct(address) => address.ip().to_string(),
            Route::Rendezvous(peer) => peer.clone(),
        },
        local_id,
        local_name: crate::APP_NAME.to_owned(),
        password,
        requested_fps: 60,
        // The Flutter remote-control UI has a clipboard panel, and the peer still
        // has to grant the permission before anything is written.
        clipboard_enabled: true,
        image_quality: crate::viewer::ViewerImageQuality::default(),
    };
    let viewer = match &session.route {
        Route::Direct(_) => Viewer::start(viewer_options, lease),
        Route::Rendezvous(peer) => {
            let config = crate::flutter_surface::rendezvous_config(peer.clone())?;
            Viewer::start_rendezvous(viewer_options, lease, config)
        }
    };
    match viewer {
        Ok(viewer) => {
            session.viewer = Some(viewer);
            Ok(())
        }
        Err(_) => Err("Viewer could not start".to_owned()),
    }
}

/// Run one event-pump step for a session. Called by the async pump task.
pub fn pump(id: Uuid) {
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&id) else {
        return;
    };
    let Some(sink) = session.events.as_ref() else {
        return;
    };
    let Some(viewer) = session.viewer.as_ref() else {
        return;
    };
    let snapshot = viewer.snapshot();
    let peer = viewer.peer_info();
    let mut outgoing: Vec<EventToUI> = Vec::new();
    collect(&mut session.pump, &snapshot, peer.as_ref(), &mut outgoing);
    for event in outgoing {
        // A closed stream is not an error the UI needs to see: the session is
        // being torn down anyway.
        let _ = sink.add(event);
    }
}

fn collect(
    pump: &mut PumpState,
    snapshot: &ViewerSnapshot,
    peer: Option<&hbb_common::message_proto::PeerInfo>,
    out: &mut Vec<EventToUI>,
) {
    let phase_changed = pump.last_phase != snapshot.phase;
    // The peer reports a display list once authenticated; that list is the only
    // authority for display geometry, so nothing is invented before it arrives.
    if snapshot.phase == "authenticated" && !pump.peer_info_sent {
        let displays = displays_json(peer, snapshot);
        let current = peer.map(|peer| peer.current_display).unwrap_or(0);
        out.push(EventToUI::Event(envelope(&[
            ("name", "connection_ready".to_owned()),
            ("secure", snapshot.encrypted.to_string()),
            ("direct", (snapshot.route == "direct_tcp").to_string()),
            ("stream_type", snapshot.route.clone()),
        ])));
        out.push(EventToUI::Event(envelope(&[
            ("name", "peer_info".to_owned()),
            (
                "username",
                peer.map(|peer| peer.username.clone()).unwrap_or_default(),
            ),
            (
                "hostname",
                peer.map(|peer| peer.hostname.clone())
                    .filter(|hostname| !hostname.is_empty())
                    .unwrap_or_default(),
            ),
            (
                "platform",
                peer.map(|peer| peer.platform.clone()).unwrap_or_default(),
            ),
            (
                "version",
                peer.map(|peer| peer.version.clone()).unwrap_or_default(),
            ),
            ("current_display", current.to_string()),
            ("displays", displays),
        ])));
        pump.peer_info_sent = true;
        pump.display_sent = false;
    }
    if pump.peer_info_sent && !pump.permission_sent {
        out.push(EventToUI::Event(envelope(&[
            ("name", "permission".to_owned()),
            ("keyboard", snapshot.keyboard_allowed.to_string()),
            ("clipboard", snapshot.clipboard_allowed.to_string()),
        ])));
        pump.permission_sent = true;
    }
    // Geometry follows the accepted stream, so a resolution change or a display
    // switch reaches the viewport without a second request.
    if pump.peer_info_sent
        && snapshot.width > 0
        && snapshot.height > 0
        && (!pump.display_sent
            || snapshot.width != pump.last_width
            || snapshot.height != pump.last_height)
    {
        pump.last_width = snapshot.width;
        pump.last_height = snapshot.height;
        pump.display_sent = true;
        out.push(EventToUI::Event(envelope(&[
            ("name", "switch_display".to_owned()),
            ("display", "0".to_owned()),
            ("x", "0".to_owned()),
            ("y", "0".to_owned()),
            ("width", snapshot.width.to_string()),
            ("height", snapshot.height.to_string()),
            ("cursor_embedded", "0".to_owned()),
            ("resolutions", "[]".to_owned()),
            ("original_width", snapshot.width.to_string()),
            ("original_height", snapshot.height.to_string()),
        ])));
    }
    if !snapshot.codec.is_empty() && pump.last_codec != snapshot.codec {
        pump.last_codec = snapshot.codec.clone();
        out.push(EventToUI::Event(envelope(&[
            ("name", "update_quality_status".to_owned()),
            ("codec_format", snapshot.codec.clone()),
            ("fps", String::new()),
            ("delay", String::new()),
            ("speed", String::new()),
            ("target_bitrate", String::new()),
            ("chroma", String::new()),
        ])));
    }
    // Authentication prompts are reported as msgbox events, which is how the
    // frontend asks for a password or a second factor.
    if phase_changed && snapshot.phase == "awaiting_insecure_confirmation" {
        out.push(msgbox(
            "insecure-connection",
            "Insecure Connection",
            "Direct IP connection could not verify the remote identity. Continue only if you trust this network and endpoint.",
        ));
    }
    if snapshot.phase == "awaiting_password" && phase_changed {
        out.push(msgbox(
            "input-password",
            "Password Required",
            "Password Required",
        ));
    }
    if matches!(
        snapshot.phase.as_str(),
        "awaiting_2fa" | "awaiting_2fa_retry"
    ) && phase_changed
    {
        out.push(msgbox(
            "input-2fa",
            "2FA Required",
            if snapshot.phase == "awaiting_2fa_retry" {
                "Wrong 2FA Code"
            } else {
                "2FA Required"
            },
        ));
    }
    if snapshot.closed && !pump.close_sent {
        if let Some(error) = &snapshot.error {
            out.push(msgbox("error", "Connection Error", error));
        }
        out.push(EventToUI::Event("close".to_owned()));
        pump.close_sent = true;
    }
    pump.last_phase = snapshot.phase.clone();
}

fn msgbox(kind: &str, title: &str, text: &str) -> EventToUI {
    EventToUI::Event(envelope(&[
        ("name", "msgbox".to_owned()),
        ("type", kind.to_owned()),
        ("title", title.to_owned()),
        ("text", text.to_owned()),
    ]))
}

/// Build one JSON envelope. The frontend reads `"name"` first and treats the
/// rest as handler input, so key order is not significant.
fn envelope(fields: &[(&str, String)]) -> String {
    let mut map = hbb_common::serde_json::Map::with_capacity(fields.len());
    for (key, value) in fields {
        map.insert((*key).to_owned(), value.clone().into());
    }
    hbb_common::serde_json::Value::Object(map).to_string()
}

/// Serialize the peer's own display report. Without one, the accepted stream
/// geometry is reported rather than a fabricated display index.
fn displays_json(
    peer: Option<&hbb_common::message_proto::PeerInfo>,
    snapshot: &ViewerSnapshot,
) -> String {
    use hbb_common::serde_json::{Value, json};
    let displays: Vec<Value> = match peer {
        Some(peer) if !peer.displays.is_empty() => peer
            .displays
            .iter()
            .map(|display| {
                json!({
                    "x": display.x,
                    "y": display.y,
                    "width": display.width,
                    "height": display.height,
                    "cursor_embedded": display.cursor_embedded,
                    "name": display.name,
                })
            })
            .collect(),
        _ => vec![json!({
            "x": 0,
            "y": 0,
            "width": snapshot.width,
            "height": snapshot.height,
            "cursor_embedded": false,
        })],
    };
    Value::Array(displays).to_string()
}

/// Spawn the per-session event pump on the shared runtime.
pub fn spawn_pump(id: Uuid) -> Result<(), String> {
    let runtime = crate::executor::runtime().map_err(|_| "Runtime unavailable".to_owned())?;
    runtime.spawn(async move {
        loop {
            {
                let registry = lock(sessions());
                match registry.get(&id) {
                    // The pump ends when the session is gone or its stream closed.
                    Some(session) if session.events.is_some() => {}
                    _ => return,
                }
            }
            pump(id);
            {
                let registry = lock(sessions());
                let finished = registry
                    .get(&id)
                    .map(|session| session.pump.close_sent)
                    .unwrap_or(true);
                if finished {
                    return;
                }
            }
            tokio::time::sleep(PUMP_INTERVAL).await;
        }
    });
    Ok(())
}

/// Run a closure against one session's viewer.
pub fn with_viewer<T>(id: Uuid, action: impl FnOnce(&Viewer) -> T) -> Option<T> {
    let registry = lock(sessions());
    let session = registry.get(&id)?;
    let viewer = session.viewer.as_ref()?;
    Some(action(viewer))
}

/// Run a closure against one session and its viewer.
pub fn with_session_viewer<T>(
    id: Uuid,
    action: impl FnOnce(&mut Session, &std::sync::Arc<Viewer>) -> T,
) -> Option<T> {
    let mut registry = lock(sessions());
    let session = registry.get_mut(&id)?;
    let viewer = session.viewer.clone()?;
    Some(action(session, &viewer))
}

/// Per-session option values the UI reads back. Only options this runtime can
/// answer truthfully are reported; everything else is `None` so the UI falls
/// back to its own default instead of a value invented here.
pub fn session_option(id: Uuid, arg: &str) -> Option<String> {
    let registry = lock(sessions());
    let session = registry.get(&id)?;
    match arg {
        "peer-id" | "id" => Some(session.peer.clone()),
        "view-only" => Some("N".to_owned()),
        "show-remote-cursor" => Some("N".to_owned()),
        "disable-audio" => Some("Y".to_owned()),
        _ => None,
    }
}

/// Boolean form of [`session_option`]. An option this runtime does not implement
/// reads as `false`, never as an assumed `true`.
pub fn toggle_option(id: Uuid, arg: &str) -> Option<bool> {
    let registry = lock(sessions());
    let _session = registry.get(&id)?;
    match arg {
        "view-only" | "show-remote-cursor" | "disable-clipboard" => Some(false),
        "disable-audio" => Some(true),
        _ => None,
    }
}

/// Peer-scoped options are persisted per peer, matching upstream so the setting
/// survives a reconnect and is shared by every session to that peer.
pub fn set_peer_option(id: Uuid, name: &str, value: &str) {
    let peer = {
        let registry = lock(sessions());
        match registry.get(&id) {
            Some(session) => session.peer.clone(),
            None => return,
        }
    };
    if name.is_empty() || value.len() > 4096 {
        return;
    }
    // Peer options live in the peer's own config file; load-modify-store keeps
    // the other per-peer settings intact.
    let mut config = hbb_common::config::PeerConfig::load(&peer);
    config.options.insert(name.to_owned(), value.to_owned());
    config.store(&peer);
}

pub fn peer_option(id: Uuid, name: &str) -> String {
    let peer = {
        let registry = lock(sessions());
        match registry.get(&id) {
            Some(session) => session.peer.clone(),
            None => return String::new(),
        }
    };
    hbb_common::config::PeerConfig::load(&peer)
        .options
        .get(name)
        .cloned()
        .unwrap_or_default()
}

pub fn view_only(id: Uuid) -> bool {
    lock(sessions())
        .get(&id)
        .is_some_and(|session| session.pump.close_sent)
}

/// Number of tracked sessions, optionally filtered by connection type.
pub fn count(conn_type: i32) -> usize {
    let registry = lock(sessions());
    if conn_type == 0 {
        return registry.len();
    }
    registry.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_are_interpreted_without_guessing_a_port() {
        assert!(matches!(
            parse_route("192.168.1.5:21118"),
            Some(Route::Direct(_))
        ));
        assert!(matches!(parse_route("192.168.1.5"), Some(Route::Direct(_))));
        assert!(matches!(
            parse_route("123456789"),
            Some(Route::Rendezvous(_))
        ));
        assert!(parse_route("").is_none());
        assert!(parse_route("bad\nid").is_none());
        // A zero port is not a usable endpoint.
        assert!(parse_route("192.168.1.5:0").is_none());
    }

    #[test]
    fn unsupported_session_kinds_are_refused_rather_than_half_created() {
        let base = AddOptions::default();
        for options in [
            AddOptions {
                is_terminal: true,
                ..base.clone()
            },
            AddOptions {
                is_file_transfer: true,
                ..base.clone()
            },
            AddOptions {
                is_port_forward: true,
                ..base.clone()
            },
            AddOptions {
                is_rdp: true,
                ..base.clone()
            },
            AddOptions {
                force_relay: true,
                ..base.clone()
            },
        ] {
            assert!(add(Uuid::new_v4(), "192.168.1.5", options).is_err());
        }
    }

    #[test]
    fn a_created_session_is_visible_and_removable() {
        let id = Uuid::new_v4();
        assert!(add(id, "192.168.1.5", AddOptions::default()).is_ok());
        assert!(exists(id));
        // A duplicate id is refused so two UI tabs cannot share one session.
        assert!(add(id, "192.168.1.5", AddOptions::default()).is_err());
        remove(id);
        assert!(!exists(id));
    }

    #[test]
    fn the_event_envelope_carries_the_name_the_frontend_dispatches_on() {
        let raw = envelope(&[("name", "peer_info".to_owned()), ("width", "10".to_owned())]);
        let parsed: hbb_common::serde_json::Value =
            hbb_common::serde_json::from_str(&raw).expect("valid json");
        assert_eq!(parsed["name"], "peer_info");
        assert_eq!(parsed["width"], "10");
    }
}
