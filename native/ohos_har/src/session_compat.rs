//! Stateful adapter for the existing ArkTS session API. RustDesk wire,
//! authentication, encryption, media and input remain owned by `rd-engine`.

use super::NativeSurfaceLease;
use hbb_common::{
    base64::{engine::general_purpose::STANDARD, Engine as _},
    config::{keys::OPTION_KEY, Config, RS_PUB_KEY},
    sodiumoxide::crypto::sign,
};
use napi_derive_ohos::napi;
use rd_engine::{
    rendezvous::RendezvousConfig,
    viewer::{SurfaceLease, Viewer, ViewerOptions, ViewerSnapshot},
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock},
    time::Duration,
};
use zeroize::Zeroizing;

const DEFAULT_DIRECT_PORT: u16 = 21118;
const MAX_COMPAT_SESSIONS: usize = 6;

#[derive(Clone)]
enum SessionRoute {
    Direct(SocketAddr),
    Rendezvous(String),
}

struct DeferredSurfaceState {
    lease: Option<Arc<NativeSurfaceLease>>,
    cancelled: bool,
}

struct DeferredSurfaceLease {
    state: Mutex<DeferredSurfaceState>,
    ready: Condvar,
}

impl DeferredSurfaceLease {
    fn new() -> Self {
        Self {
            state: Mutex::new(DeferredSurfaceState {
                lease: None,
                cancelled: false,
            }),
            ready: Condvar::new(),
        }
    }

    fn bind(&self, id: u64) -> Result<(), String> {
        let lease =
            NativeSurfaceLease::acquire(id).map_err(|_| "Cannot retain Surface".to_owned())?;
        let mut state = lock(&self.state);
        if state.cancelled {
            return Err("Session is closing".to_owned());
        }
        if let Some(current) = &state.lease {
            return if current.id == id {
                Ok(())
            } else {
                Err("A running decoder cannot be rebound to another Surface".to_owned())
            };
        }
        state.lease = Some(lease);
        self.ready.notify_all();
        Ok(())
    }

    fn cancel(&self) {
        let mut state = lock(&self.state);
        state.cancelled = true;
        self.ready.notify_all();
    }

    fn bound_id(&self) -> Option<u64> {
        lock(&self.state).lease.as_ref().map(|lease| lease.id)
    }
}

// Safety: the method does not return until one retained NativeSurfaceLease is
// installed. That lease then remains stable until decoder/run teardown drops the
// final Arc. Cancellation returns zero only to abort an unopened decoder.
unsafe impl SurfaceLease for DeferredSurfaceLease {
    fn surface_id(&self) -> u64 {
        let mut state = lock(&self.state);
        loop {
            if let Some(lease) = &state.lease {
                return lease.id;
            }
            if state.cancelled {
                return 0;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }
}

struct CompatSession {
    id: String,
    target: String,
    route: SessionRoute,
    password: Zeroizing<String>,
    initial_password_supplied: bool,
    force_relay: bool,
    view_only: bool,
    requested_fps: u32,
    codec_preference: String,
    image_quality: String,
    show_remote_cursor: bool,
    disable_audio: bool,
    phase: String,
    viewer: Option<Arc<Viewer>>,
    surface: Arc<DeferredSurfaceLease>,
    pending_events: VecDeque<Value>,
    last_engine_phase: String,
    login_attempts: u64,
    prompted_login_attempts: u64,
    peer_info_emitted: bool,
    permission_emitted: bool,
    close_emitted: bool,
    last_codec: String,
}

static SESSIONS: OnceLock<Mutex<HashMap<String, CompatSession>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, CompatSession>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn action(
    action: &str,
    ok: bool,
    message: impl Into<String>,
    session: Option<&CompatSession>,
) -> String {
    let session = session.map(session_value).unwrap_or(Value::Null);
    json!({
        "ok": ok,
        "action": action,
        "message": message.into(),
        "session": session
    })
    .to_string()
}

fn session_value(session: &CompatSession) -> Value {
    let phase = session
        .viewer
        .as_ref()
        .map(|viewer| viewer.snapshot().phase)
        .unwrap_or_else(|| session.phase.clone());
    json!({
        "sessionId": session.id,
        "coreSessionId": format!("enhanced:{}", session.id),
        "phase": phase,
        "peerTarget": session.target,
        "viewOnly": session.view_only
    })
}

fn parse_route(target: &str) -> Option<SessionRoute> {
    if let Ok(address) = target.parse::<SocketAddr>() {
        return (address.port() != 0).then_some(SessionRoute::Direct(address));
    }
    if let Ok(ip) = target.parse::<IpAddr>() {
        return Some(SessionRoute::Direct(SocketAddr::new(
            ip,
            DEFAULT_DIRECT_PORT,
        )));
    }
    if !target.is_empty() && target.len() <= 256 && !target.chars().any(char::is_control) {
        return Some(SessionRoute::Rendezvous(target.to_owned()));
    }
    None
}

fn rendezvous_config(id: String) -> Result<RendezvousConfig, String> {
    let configured_key = Config::get_option(OPTION_KEY);
    let (key_base64, licence_key) = if configured_key.is_empty() {
        (RS_PUB_KEY, RS_PUB_KEY.to_owned())
    } else {
        (configured_key.as_str(), configured_key.clone())
    };
    let bytes = STANDARD
        .decode(key_base64.as_bytes())
        .map_err(|_| "Configured rendezvous trust anchor is invalid".to_owned())?;
    let server_key = sign::PublicKey::from_slice(&bytes)
        .ok_or_else(|| "Configured rendezvous trust anchor is invalid".to_owned())?;
    Ok(RendezvousConfig {
        id,
        rendezvous_server: Config::get_rendezvous_server(),
        server_key,
        licence_key,
        relay_server: None,
        connect_timeout: Duration::from_secs(45),
    })
}

fn collect_engine_events(session: &mut CompatSession, snapshot: &ViewerSnapshot) {
    let phase_changed = session.last_engine_phase != snapshot.phase;
    if snapshot.phase == "awaiting_insecure_confirmation" && phase_changed {
        session.pending_events.push_back(json!({
            "name": "msgbox",
            "type": "insecure-connection",
            "title": "Insecure Connection",
            "text": "Direct IP connection could not verify the remote identity. Continue only if you trust this network and endpoint."
        }));
    }
    if snapshot.phase == "awaiting_password"
        && (phase_changed || session.prompted_login_attempts != session.login_attempts)
    {
        let retry = session.initial_password_supplied || session.login_attempts > 0;
        session.pending_events.push_back(json!({
            "name": "msgbox",
            "type": if retry { "re-input-password" } else { "input-password" },
            "title": "Password Required",
            "text": if retry { "Wrong Password" } else { "Password Required" }
        }));
        session.prompted_login_attempts = session.login_attempts;
    }
    if matches!(
        snapshot.phase.as_str(),
        "awaiting_2fa" | "awaiting_2fa_retry"
    ) && phase_changed
    {
        session.pending_events.push_back(json!({
            "name": "msgbox",
            "type": "input-2fa",
            "title": "2FA Required",
            "text": if snapshot.phase == "awaiting_2fa_retry" { "Wrong 2FA Code" } else { "2FA Required" }
        }));
    }
    if snapshot.phase == "authenticated" && !session.peer_info_emitted {
        session.pending_events.push_back(json!({
            "name": "connection_ready",
            "secure": snapshot.encrypted.to_string(),
            "direct": (snapshot.route == "direct_tcp").to_string(),
            "stream_type": snapshot.route
        }));
        let displays = json!([{
            "x": 0,
            "y": 0,
            "width": snapshot.width,
            "height": snapshot.height,
            "cursor_embedded": false
        }]);
        session.pending_events.push_back(json!({
            "name": "peer_info",
            "username": "",
            "hostname": session.target,
            "platform": "",
            "version": "",
            "current_display": "0",
            "displays": displays.to_string()
        }));
        session.peer_info_emitted = true;
    }
    if session.peer_info_emitted && !session.permission_emitted {
        session.pending_events.push_back(json!({
            "name": "permission",
            "keyboard": snapshot.keyboard_allowed.to_string()
        }));
        session.permission_emitted = true;
    }
    if !snapshot.codec.is_empty() && session.last_codec != snapshot.codec {
        session.pending_events.push_back(json!({
            "name": "update_quality_status",
            "codec_format": snapshot.codec,
            "fps": "",
            "delay": "",
            "speed": "",
            "target_bitrate": "",
            "chroma": ""
        }));
        session.last_codec = snapshot.codec.clone();
    }
    if snapshot.closed && !session.close_emitted {
        if let Some(error) = &snapshot.error {
            session.pending_events.push_back(json!({
                "name": "msgbox",
                "type": "error",
                "title": "Connection Error",
                "text": error
            }));
        }
        session.pending_events.push_back(json!({"name": "close"}));
        session.close_emitted = true;
    }
    session.last_engine_phase = snapshot.phase.clone();
    session.phase = snapshot.phase.clone();
}

#[napi]
pub fn session_add(session_id: String, peer_target: String, options_json: String) -> String {
    let target = peer_target.trim().to_owned();
    let options: Value = match serde_json::from_str(&options_json) {
        Ok(options) => options,
        Err(_) => return action("session_add", false, "Invalid session options", None),
    };
    let Some(route) = parse_route(&target) else {
        return action("session_add", false, "Invalid peer target", None);
    };
    if session_id.is_empty() || session_id.len() > 64 || session_id.chars().any(char::is_control) {
        return action("session_add", false, "Invalid session ID", None);
    }
    if options
        .get("isFileTransfer")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || options
            .get("isViewCamera")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return action(
            "session_add",
            false,
            "This compatibility milestone supports desktop viewer sessions only",
            None,
        );
    }
    let password = options
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if password.len() > 4096 {
        return action("session_add", false, "Password is too long", None);
    }
    let mut registry = lock(sessions());
    if registry.contains_key(&session_id) {
        return action("session_add", false, "Session already exists", None);
    }
    if registry.len() >= MAX_COMPAT_SESSIONS {
        return action("session_add", false, "Session limit reached", None);
    }
    let session = CompatSession {
        id: session_id.clone(),
        target,
        route,
        initial_password_supplied: !password.is_empty(),
        password: Zeroizing::new(password),
        force_relay: options
            .get("forceRelay")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        view_only: options
            .get("isViewOnly")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        requested_fps: 60,
        codec_preference: "auto".to_owned(),
        image_quality: "balanced".to_owned(),
        show_remote_cursor: false,
        disable_audio: true,
        phase: "created".to_owned(),
        viewer: None,
        surface: Arc::new(DeferredSurfaceLease::new()),
        pending_events: VecDeque::new(),
        last_engine_phase: String::new(),
        login_attempts: 0,
        prompted_login_attempts: u64::MAX,
        peer_info_emitted: false,
        permission_emitted: false,
        close_emitted: false,
        last_codec: String::new(),
    };
    registry.insert(session_id.clone(), session);
    let session = registry.get(&session_id).expect("inserted session");
    action(
        "session_add",
        true,
        "Created RustDesk Enhanced session",
        Some(session),
    )
}

#[napi]
pub fn session_set_view_only(session_id: String, enabled: bool) -> String {
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return action("session_set_view_only", false, "Session not found", None);
    };
    if session.viewer.is_some() {
        return action(
            "session_set_view_only",
            false,
            "View-only policy cannot change after session start",
            Some(session),
        );
    }
    session.view_only = enabled;
    action(
        "session_set_view_only",
        true,
        "Updated local input policy",
        Some(session),
    )
}

#[napi]
pub fn session_set_codec_preference(session_id: String, codec: String) -> String {
    let normalized = codec.trim().to_ascii_lowercase();
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return action(
            "session_set_codec_preference",
            false,
            "Session not found",
            None,
        );
    };
    if !matches!(normalized.as_str(), "auto" | "h265") {
        return action(
            "session_set_codec_preference",
            false,
            "Explicit H264 preference is not connected in this milestone",
            Some(session),
        );
    }
    session.codec_preference = normalized;
    action(
        "session_set_codec_preference",
        true,
        "Codec preference accepted",
        Some(session),
    )
}

#[napi]
pub fn session_start(session_id: String) -> String {
    let (route, options, surface, force_relay) = {
        let mut registry = lock(sessions());
        let Some(session) = registry.get_mut(&session_id) else {
            return action("session_start", false, "Session not found", None);
        };
        if session.viewer.is_some() {
            return action(
                "session_start",
                true,
                "Session already started",
                Some(session),
            );
        }
        let local_id = Config::get_id();
        if local_id.is_empty() {
            return action(
                "session_start",
                false,
                "Runtime identity is unavailable",
                Some(session),
            );
        }
        let options = ViewerOptions {
            address: match &session.route {
                SessionRoute::Direct(address) => Some(*address),
                SessionRoute::Rendezvous(_) => None,
            },
            username: match &session.route {
                SessionRoute::Direct(address) => address.ip().to_string(),
                SessionRoute::Rendezvous(id) => id.clone(),
            },
            local_id,
            local_name: "RustDesk HMOS".to_owned(),
            password: std::mem::take(&mut *session.password),
            requested_fps: session.requested_fps,
        };
        session.phase = "starting".to_owned();
        (
            session.route.clone(),
            options,
            session.surface.clone(),
            session.force_relay,
        )
    };
    if force_relay {
        let registry = lock(sessions());
        let session = registry.get(&session_id);
        return action(
            "session_start",
            false,
            "Forced relay routing is not connected in this milestone",
            session,
        );
    }
    let viewer = match route {
        SessionRoute::Direct(_) => Viewer::start(options, surface),
        SessionRoute::Rendezvous(id) => match rendezvous_config(id) {
            Ok(config) => Viewer::start_rendezvous(options, surface, config),
            Err(message) => {
                let registry = lock(sessions());
                return action("session_start", false, message, registry.get(&session_id));
            }
        },
    };
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        if let Ok(viewer) = viewer {
            viewer.request_close();
        }
        return action("session_start", false, "Session was removed", None);
    };
    match viewer {
        Ok(viewer) => {
            session.viewer = Some(viewer);
            action(
                "session_start",
                true,
                "RustDesk Enhanced viewer started",
                Some(session),
            )
        }
        Err(_) => {
            session.phase = "failed".to_owned();
            action(
                "session_start",
                false,
                "RustDesk Enhanced viewer could not start",
                Some(session),
            )
        }
    }
}

#[napi]
pub fn session_bind_surface(session_id: String, display: u32, surface_id: String) -> String {
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return action("session_bind_surface", false, "Session not found", None);
    };
    if display != 0 {
        return action(
            "session_bind_surface",
            false,
            "Multi-display Surface binding is not connected in this milestone",
            Some(session),
        );
    }
    if surface_id.trim().is_empty() {
        return action(
            "session_bind_surface",
            session.surface.bound_id().is_none(),
            if session.surface.bound_id().is_none() {
                "Surface binding is already empty"
            } else {
                "A retained decoder Surface can only be released by closing the session"
            },
            Some(session),
        );
    }
    let Some(id) = surface_id.parse::<u64>().ok().filter(|id| *id != 0) else {
        return action(
            "session_bind_surface",
            false,
            "Invalid native Surface ID",
            Some(session),
        );
    };
    match session.surface.bind(id) {
        Ok(()) => action(
            "session_bind_surface",
            true,
            "Bound retained native Surface",
            Some(session),
        ),
        Err(message) => action("session_bind_surface", false, message, Some(session)),
    }
}

#[napi]
pub fn session_poll_events(session_id: String, limit: u32) -> String {
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return action("session_poll_events", false, "Session not found", None);
    };
    if let Some(viewer) = &session.viewer {
        let snapshot = viewer.snapshot();
        collect_engine_events(session, &snapshot);
    }
    let events: Vec<Value> = (0..limit.min(256))
        .filter_map(|_| session.pending_events.pop_front())
        .collect();
    json!({
        "ok": true,
        "action": "session_poll_events",
        "message": "Polled RustDesk Enhanced session",
        "events": events,
        "session": session_value(session)
    })
    .to_string()
}

#[napi]
pub fn session_login(session_id: String, login_json: String) -> String {
    let payload: Value = match serde_json::from_str(&login_json) {
        Ok(payload) => payload,
        Err(_) => return action("session_login", false, "Invalid login payload", None),
    };
    let password = payload
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return action("session_login", false, "Session not found", None);
    };
    let Some(viewer) = &session.viewer else {
        return action(
            "session_login",
            false,
            "Session is not started",
            Some(session),
        );
    };
    match viewer.submit_password(password) {
        Ok(()) => {
            session.login_attempts = session.login_attempts.saturating_add(1);
            session.phase = "authenticating".to_owned();
            action("session_login", true, "Password submitted", Some(session))
        }
        Err(_) => action(
            "session_login",
            false,
            "Password could not be submitted in the current state",
            Some(session),
        ),
    }
}

#[napi]
pub fn session_send2_fa(session_id: String, code: String, _trust_this_device: bool) -> String {
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return action("session_send_2fa", false, "Session not found", None);
    };
    let Some(viewer) = &session.viewer else {
        return action(
            "session_send_2fa",
            false,
            "Session is not started",
            Some(session),
        );
    };
    match viewer.submit_second_factor(code) {
        Ok(()) => action(
            "session_send_2fa",
            true,
            "Second factor submitted",
            Some(session),
        ),
        Err(_) => action(
            "session_send_2fa",
            false,
            "Second factor could not be submitted in the current state",
            Some(session),
        ),
    }
}

#[napi]
pub fn session_set_clipboard_file_root(session_id: String, _root: String) -> String {
    let registry = lock(sessions());
    let session = registry.get(&session_id);
    action(
        "session_set_clipboard_file_root",
        false,
        "Clipboard file transfer is not connected in this milestone",
        session,
    )
}

#[napi]
pub fn session_send_mouse_event(
    session_id: String,
    event_kind: u32,
    button: u32,
    x: i32,
    y: i32,
    _relative_marker: i32,
) -> bool {
    let registry = lock(sessions());
    let Some(session) = registry.get(&session_id) else {
        return false;
    };
    if session.view_only {
        return false;
    }
    session
        .viewer
        .as_ref()
        .is_some_and(|viewer| viewer.send_mouse(event_kind, button, x, y).is_ok())
}

#[napi]
pub fn session_close(session_id: String) -> String {
    let session = lock(sessions()).remove(&session_id);
    let Some(session) = session else {
        return action("session_close", true, "Session is already closed", None);
    };
    session.surface.cancel();
    if let Some(viewer) = &session.viewer {
        viewer.request_close();
    }
    action(
        "session_close",
        true,
        "Closed RustDesk Enhanced session",
        Some(&session),
    )
}

#[napi]
pub fn session_get_conn_token(_session_id: String) -> Option<String> {
    None
}

#[napi]
pub fn session_get_enable_trusted_devices(_session_id: String) -> bool {
    false
}

#[napi]
pub fn session_get_image_quality(session_id: String) -> Option<String> {
    lock(sessions())
        .get(&session_id)
        .map(|session| session.image_quality.clone())
}

#[napi]
pub fn session_set_image_quality(session_id: String, quality: String) -> String {
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return action(
            "session_set_image_quality",
            false,
            "Session not found",
            None,
        );
    };
    session.image_quality = quality;
    action(
        "session_set_image_quality",
        true,
        "Stored quality preference for this session",
        Some(session),
    )
}

#[napi]
pub fn session_set_custom_image_quality(_session_id: String, _quality: u32) {}

#[napi]
pub fn session_set_custom_fps(session_id: String, fps: u32) -> String {
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return action("session_set_custom_fps", false, "Session not found", None);
    };
    if session.viewer.is_some() || !(1..=240).contains(&fps) {
        return action(
            "session_set_custom_fps",
            false,
            "FPS must be set before session start and be within 1..240",
            Some(session),
        );
    }
    session.requested_fps = fps;
    action(
        "session_set_custom_fps",
        true,
        "Updated requested FPS",
        Some(session),
    )
}

#[napi]
pub fn session_set_show_remote_cursor(session_id: String, enabled: bool) -> String {
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return action(
            "session_set_show_remote_cursor",
            false,
            "Session not found",
            None,
        );
    };
    session.show_remote_cursor = enabled;
    action(
        "session_set_show_remote_cursor",
        true,
        "Updated local remote-cursor presentation policy",
        Some(session),
    )
}

#[napi]
pub fn session_get_toggle_option(session_id: String, option: String) -> bool {
    lock(sessions()).get(&session_id).is_some_and(|session| {
        if option == "show-remote-cursor" {
            session.show_remote_cursor
        } else if option == "disable-audio" {
            session.disable_audio
        } else {
            false
        }
    })
}

#[napi]
pub fn session_toggle_option(session_id: String, option: String) {
    if let Some(session) = lock(sessions()).get_mut(&session_id) {
        if option == "show-remote-cursor" {
            session.show_remote_cursor = !session.show_remote_cursor;
        } else if option == "disable-audio" {
            session.disable_audio = !session.disable_audio;
        }
    }
}

#[napi]
pub fn session_set_common(session_id: String, key: String, value: String) {
    let registry = lock(sessions());
    let Some(viewer) = registry
        .get(&session_id)
        .and_then(|session| session.viewer.as_ref())
    else {
        return;
    };
    if key == "continue-insecure-connection" {
        let allow = value.eq_ignore_ascii_case("Y");
        let _ = viewer.continue_insecure(allow);
        if !allow {
            viewer.request_close();
        }
    }
}

#[napi]
pub fn session_get_remote_audio_state(session_id: String) -> String {
    let registry = lock(sessions());
    let Some(session) = registry.get(&session_id) else {
        return action(
            "session_get_remote_audio_state",
            false,
            "Session not found",
            None,
        );
    };
    json!({
        "ok": true,
        "action": "session_get_remote_audio_state",
        "audio": {
            "available": false,
            "muted": session.disable_audio,
            "rendererActive": false,
            "errorText": "Remote audio is not connected in this milestone"
        },
        "session": session_value(session)
    })
    .to_string()
}

#[napi]
pub fn session_get_software_present_state(_session_id: String, _display: u32) -> String {
    json!({
        "available": false,
        "active": false,
        "state": "native-surface",
        "reason": "hardware-surface-path"
    })
    .to_string()
}

#[napi]
pub fn session_set_video_paused(session_id: String, paused: bool) -> String {
    let registry = lock(sessions());
    let session = registry.get(&session_id);
    action(
        "session_set_video_paused",
        !paused,
        if paused {
            "Video pause is not connected in this milestone"
        } else {
            "Viewer is already receiving video"
        },
        session,
    )
}

#[napi]
pub fn session_refresh(session_id: String, _display: u32) -> String {
    let registry = lock(sessions());
    let session = registry.get(&session_id);
    action(
        "session_refresh",
        session.is_some(),
        "The active stream supplies its initial refresh",
        session,
    )
}

#[napi]
pub fn session_set_size(session_id: String, _display: u32, _width: u32, _height: u32) -> String {
    let registry = lock(sessions());
    let session = registry.get(&session_id);
    action(
        "session_set_size",
        session.is_some(),
        "ArkUI Surface owns presentation size",
        session,
    )
}
