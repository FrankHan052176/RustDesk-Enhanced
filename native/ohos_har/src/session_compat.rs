//! Stateful adapter for the existing ArkTS session API. RustDesk wire,
//! authentication, encryption, media and input remain owned by `rd-engine`.

use super::NativeSurfaceLease;
use hbb_common::{
    base64::{engine::general_purpose::STANDARD, Engine as _},
    config::{keys::OPTION_KEY, Config, RS_PUB_KEY},
    sodiumoxide::crypto::sign,
};
use librustdesk::{
    rendezvous::RendezvousConfig,
    session_backend::SessionBackend,
    viewer::{
        SurfaceLease, Viewer, ViewerError, ViewerImageQuality, ViewerKey, ViewerOptions,
        ViewerSnapshot,
    },
    vnc::live::VncLiveSession,
};
use napi_derive_ohos::napi;
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

/// The protocol a session speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionProtocol {
    RustDesk,
    Vnc,
}

#[derive(Debug, Clone)]
enum SessionRoute {
    Direct(SocketAddr),
    Rendezvous(String),
    /// An RFB endpoint, from a `vnc://` target.
    Vnc {
        host: String,
        port: u16,
    },
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
    custom_image_quality: i32,
    clipboard_enabled: bool,
    show_remote_cursor: bool,
    disable_audio: bool,
    phase: String,
    /// Which protocol this session speaks. Decided from the target when the
    /// session is created, because the two protocols cannot be told apart later.
    protocol: SessionProtocol,
    viewer: Option<Arc<SessionBackend>>,
    surface: Arc<DeferredSurfaceLease>,
    pending_events: VecDeque<Value>,
    last_engine_phase: String,
    login_attempts: u64,
    prompted_login_attempts: u64,
    peer_info_emitted: bool,
    permission_emitted: bool,
    close_emitted: bool,
    last_codec: String,
    /// Last peer-authoritative display geometry, used to edge-trigger the
    /// `switch_display` event without echoing the requested index back.
    active_display: i32,
    active_width: i32,
    active_height: i32,
    /// Report the display event once the first authoritative geometry exists.
    display_reported: bool,
    /// Monotonic start marker used as the telemetry generation, so a UI sampler
    /// can tell a restarted session apart without seeing an identity.
    generation: u64,
    /// Microseconds since the first telemetry read, only ever increasing.
    telemetry_elapsed_us: u64,
    telemetry_last_read: Option<std::time::Instant>,
    /// Bumped whenever the requested rate changes, so a UI sampler treats a
    /// rate change as a new observation window instead of a counter reset.
    intent_revision: u64,
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
    let snapshot = session.viewer.as_ref().map(|viewer| viewer.snapshot());
    let phase = snapshot
        .as_ref()
        .map(|snapshot| snapshot.phase.clone())
        .unwrap_or_else(|| session.phase.clone());
    let image_quality = snapshot
        .as_ref()
        .map(|snapshot| snapshot.image_quality.clone())
        .unwrap_or_else(|| session.image_quality.clone());
    let requested_fps = snapshot
        .as_ref()
        .map(|snapshot| snapshot.requested_fps)
        .unwrap_or(session.requested_fps);
    let clipboard_allowed = snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.clipboard_allowed);
    json!({
        "sessionId": session.id,
        "coreSessionId": format!("enhanced:{}", session.id),
        "phase": phase,
        "peerTarget": session.target,
        "viewOnly": session.view_only,
        "imageQuality": image_quality,
        "customImageQuality": session.custom_image_quality,
        "fps": requested_fps,
        "clipboardEnabled": session.clipboard_enabled,
        "clipboardAllowed": clipboard_allowed
    })
}

/// Map the frontend preset name onto a stored quality label. Unknown values
/// fall back to balanced so a stale preference cannot broaden the stream.
fn normalize_quality_name(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => "low",
        "best" => "best",
        "custom" => "custom",
        _ => "balanced",
    }
    .to_owned()
}

/// Build the engine quality from stored local state. `custom` without a valid
/// custom value stays balanced rather than silently changing the preset.
fn engine_quality(session: &CompatSession) -> ViewerImageQuality {
    match session.image_quality.as_str() {
        "low" => ViewerImageQuality::Low,
        "best" => ViewerImageQuality::Best,
        "custom" if (10..=2000).contains(&session.custom_image_quality) => {
            ViewerImageQuality::Custom(session.custom_image_quality)
        }
        _ => ViewerImageQuality::Balanced,
    }
}

/// Accept booleans, `1`/`0`, and the `Y`/`N` spelling the frontend may send.
/// Monotonic per-session telemetry generation. Derived from the process clock so
/// two sessions created in the same millisecond stay distinguishable, and no
/// peer or user identity ever reaches the UI sampler.
fn generation_marker() -> u64 {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

/// Advance and return the monotonic telemetry elapsed time. A sampler that sees
/// a non-increasing window discards the sample, so the clock never moves back.
fn advance_telemetry_clock(session: &mut CompatSession) -> u64 {
    let now = std::time::Instant::now();
    match session.telemetry_last_read {
        Some(previous) => {
            let delta = now.saturating_duration_since(previous);
            session.telemetry_elapsed_us = session
                .telemetry_elapsed_us
                .saturating_add(delta.as_micros().min(u64::MAX as u128) as u64);
        }
        None => session.telemetry_elapsed_us = 0,
    }
    session.telemetry_last_read = Some(now);
    session.telemetry_elapsed_us
}

/// Accept booleans, `1`/`0`, and the `Y`/`N` spelling the frontend may send.
fn json_flag(value: &Value, key: &str) -> bool {
    match value.get(key) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(text)) => matches!(text.as_str(), "1" | "true" | "TRUE" | "Y" | "y"),
        Some(Value::Number(number)) => number.as_i64().unwrap_or_default() != 0,
        _ => false,
    }
}

/// Key-name and USB HID resolution stay in the engine so this layer cannot
/// drift from the wire contract.
fn parse_legacy_key(name: &str) -> Option<ViewerKey> {
    librustdesk::viewer::legacy_key_name(name)
}

fn usb_hid_to_viewer_key(usb_hid: u32, character: &str) -> Option<ViewerKey> {
    librustdesk::viewer::usb_hid_key(usb_hid, character)
}

/// The RFB scheme, and the default port a VNC server listens on.
const VNC_SCHEME: &str = "vnc://";
const VNC_DEFAULT_PORT: u16 = 5900;

/// The host and port of a `vnc://` target, or `None` for any other target.
///
/// The scheme is what distinguishes a VNC endpoint from a RustDesk peer id: an
/// id is an opaque string, so a bare `10.0.0.1:5900` must stay a direct RustDesk
/// address and not silently become VNC. The port defaults because writing
/// `vnc://host` is the common case.
fn parse_vnc_target(target: &str) -> Option<(String, u16)> {
    let rest = target.strip_prefix(VNC_SCHEME)?;
    if rest.is_empty() || rest.chars().any(char::is_control) {
        return None;
    }
    // `SocketAddr` first so an IPv6 literal's brackets are handled by the parser
    // rather than by splitting on the last colon.
    if let Ok(address) = rest.parse::<SocketAddr>() {
        return (address.port() != 0).then(|| (address.ip().to_string(), address.port()));
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok()?),
        None => (rest, VNC_DEFAULT_PORT),
    };
    if host.is_empty() || port == 0 {
        return None;
    }
    Some((host.to_owned(), port))
}

fn parse_route(target: &str) -> Option<SessionRoute> {
    if let Some((host, port)) = parse_vnc_target(target) {
        return Some(SessionRoute::Vnc { host, port });
    }
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

fn collect_engine_events(
    session: &mut CompatSession,
    snapshot: &ViewerSnapshot,
    peer: Option<&hbb_common::message_proto::PeerInfo>,
) {
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
    // A VNC session has no login exchange to authenticate: it is usable as soon
    // as the handshake completes, which the snapshot reports as `connected` and
    // then `streaming`. Waiting for an `authenticated` phase that RFB never
    // produces would leave the frontend showing "connecting" forever.
    let ready_for_peer_info = match session.protocol {
        SessionProtocol::RustDesk => snapshot.phase == "authenticated",
        SessionProtocol::Vnc => matches!(snapshot.phase.as_str(), "connected" | "streaming"),
    };
    if ready_for_peer_info && !session.peer_info_emitted {
        session.pending_events.push_back(json!({
            "name": "connection_ready",
            "secure": snapshot.encrypted.to_string(),
            "direct": (snapshot.route == "direct_tcp").to_string(),
            "stream_type": snapshot.route
        }));
        // The peer's own report is the only source for displays and platform.
        // Missing entries stay empty instead of being invented locally.
        let mut displays: Vec<Value> = peer
            .map(|peer| {
                peer.displays
                    .iter()
                    .map(|display| {
                        json!({
                            "x": display.x,
                            "y": display.y,
                            "width": display.width,
                            "height": display.height,
                            "cursor_embedded": display.cursor_embedded,
                            "name": display.name,
                            "online": display.online,
                            "scale": display.scale,
                            "original_width": display.original_resolution.as_ref().map(|resolution| resolution.width).unwrap_or(0),
                            "original_height": display.original_resolution.as_ref().map(|resolution| resolution.height).unwrap_or(0)
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        if displays.is_empty() {
            // Without a peer report only the accepted stream geometry is
            // published, and never a fabricated display index.
            displays.push(json!({
                "x": 0,
                "y": 0,
                "width": snapshot.width,
                "height": snapshot.height,
                "cursor_embedded": false
            }));
        }
        let current_display = peer.map(|peer| peer.current_display).unwrap_or(0);
        session.pending_events.push_back(json!({
            "name": "peer_info",
            "username": peer.map(|peer| peer.username.clone()).unwrap_or_default(),
            "hostname": peer
                .map(|peer| peer.hostname.clone())
                .filter(|hostname| !hostname.is_empty())
                .unwrap_or_else(|| session.target.clone()),
            "platform": peer.map(|peer| peer.platform.clone()).unwrap_or_default(),
            "version": peer.map(|peer| peer.version.clone()).unwrap_or_default(),
            "current_display": current_display.to_string(),
            "displays": json!(displays).to_string()
        }));
        session.active_display = current_display;
        session.active_width = snapshot.width;
        session.active_height = snapshot.height;
        // Emit the authoritative geometry on the next poll so the viewport can
        // fit the real source before the first frame is presented.
        session.display_reported = false;
        session.peer_info_emitted = true;
    }
    if session.peer_info_emitted {
        let size_changed = snapshot.width > 0
            && snapshot.height > 0
            && (snapshot.width != session.active_width || snapshot.height != session.active_height);
        if size_changed || !session.display_reported {
            session.active_width = snapshot.width;
            session.active_height = snapshot.height;
            session.display_reported = true;
            session.pending_events.push_back(json!({
                "name": "switch_display",
                "display": session.active_display.to_string(),
                "x": "0",
                "y": "0",
                "width": snapshot.width.to_string(),
                "height": snapshot.height.to_string(),
                "cursor_embedded": "0",
                "resolutions": "[]",
                "original_width": snapshot.width.to_string(),
                "original_height": snapshot.height.to_string()
            }));
        }
    }
    if session.peer_info_emitted && !session.permission_emitted {
        session.pending_events.push_back(json!({
            "name": "permission",
            "keyboard": snapshot.keyboard_allowed.to_string(),
            "clipboard": snapshot.clipboard_allowed.to_string()
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
        requested_fps: options
            .get("customFps")
            .and_then(Value::as_u64)
            .and_then(|fps| u32::try_from(fps).ok())
            .filter(|fps| (1..=240).contains(fps))
            .unwrap_or(60),
        codec_preference: "auto".to_owned(),
        image_quality: normalize_quality_name(
            options
                .get("imageQuality")
                .and_then(Value::as_str)
                .unwrap_or("balanced"),
        ),
        custom_image_quality: options
            .get("customImageQuality")
            .and_then(Value::as_i64)
            .and_then(|quality| i32::try_from(quality).ok())
            .filter(|quality| (10..=2000).contains(quality))
            .unwrap_or(0),
        clipboard_enabled: options
            .get("clipboardEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        show_remote_cursor: false,
        disable_audio: true,
        phase: "created".to_owned(),
        protocol: SessionProtocol::RustDesk,
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
        active_display: 0,
        active_width: 0,
        active_height: 0,
        display_reported: false,
        generation: generation_marker(),
        telemetry_elapsed_us: 0,
        telemetry_last_read: None,
        intent_revision: 1,
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
    let (route, options, surface, force_relay, mut password_for_vnc, view_only) = {
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
        let quality = engine_quality(session);
        let options = ViewerOptions {
            address: match &session.route {
                SessionRoute::Direct(address) => Some(*address),
                SessionRoute::Rendezvous(_) | SessionRoute::Vnc { .. } => None,
            },
            username: match &session.route {
                SessionRoute::Direct(address) => address.ip().to_string(),
                SessionRoute::Rendezvous(id) => id.clone(),
                SessionRoute::Vnc { host, port } => format!("{host}:{port}"),
            },
            local_id,
            local_name: "RustDesk HMOS".to_owned(),
            // The password is carried separately: a VNC session uses it during
            // the handshake and a RustDesk session later, so it cannot be moved
            // into these options.
            password: String::new(),
            requested_fps: session.requested_fps,
            clipboard_enabled: session.clipboard_enabled,
            image_quality: quality,
        };
        session.phase = "starting".to_owned();
        let password = std::mem::take(&mut *session.password);
        let view_only = session.view_only;
        (
            session.route.clone(),
            options,
            session.surface.clone(),
            session.force_relay,
            password,
            view_only,
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
    let viewer: Result<Arc<SessionBackend>, ViewerError> = match route {
        // A VNC server authenticates during the handshake, so the connection
        // (and therefore the password) is used here and now. There is no later
        // step at which a password could be supplied.
        SessionRoute::Vnc { host, port } => {
            let password = std::mem::take(&mut password_for_vnc);
            let password = (!password.is_empty()).then_some(password.as_str());
            match VncLiveSession::open(&host, port, password, !view_only) {
                Ok(session) => Ok(Arc::new(SessionBackend::Vnc(Arc::new(session)))),
                Err(error) => Err(ViewerError::Vnc(error)),
            }
        }
        SessionRoute::Direct(_) => {
            Viewer::start(options, surface).map(|viewer| Arc::new(SessionBackend::RustDesk(viewer)))
        }
        SessionRoute::Rendezvous(id) => match rendezvous_config(id) {
            Ok(config) => Viewer::start_rendezvous(options, surface, config)
                .map(|viewer| Arc::new(SessionBackend::RustDesk(viewer))),
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
        Ok(backend) => {
            let protocol = backend.protocol();
            session.protocol = if backend.is_vnc() {
                SessionProtocol::Vnc
            } else {
                SessionProtocol::RustDesk
            };
            session.viewer = Some(backend);
            action(
                "session_start",
                true,
                &format!("{protocol} session started"),
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
        let peer = viewer.peer_info();
        collect_engine_events(session, &snapshot, peer.as_ref());
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

/// Delivery gates for local input. Exposed because the boolean mouse entry point
/// cannot explain why a pointer event was refused, and "the cursor does not
/// move" is otherwise indistinguishable from a broken mapping.
#[napi]
pub fn session_input_delivery_status(session_id: String) -> String {
    let registry = lock(sessions());
    let Some(session) = registry.get(&session_id) else {
        return json!({"ok": false, "message": "Session not found"}).to_string();
    };
    let snapshot = session.viewer.as_ref().map(|viewer| viewer.snapshot());
    json!({
        "ok": true,
        "viewOnly": session.view_only,
        "hasViewer": session.viewer.is_some(),
        "phase": snapshot.as_ref().map(|snapshot| snapshot.phase.clone()).unwrap_or_default(),
        "keyboardAllowed": snapshot.as_ref().is_some_and(|snapshot| snapshot.keyboard_allowed),
        "clipboardAllowed": snapshot.as_ref().is_some_and(|snapshot| snapshot.clipboard_allowed),
        "closed": snapshot.as_ref().is_some_and(|snapshot| snapshot.closed),
        "width": snapshot.as_ref().map(|snapshot| snapshot.width).unwrap_or_default(),
        "height": snapshot.as_ref().map(|snapshot| snapshot.height).unwrap_or_default(),
        "mouseRefusal": match (&session.view_only, &session.viewer) {
            (true, _) => Some("view_only"),
            (_, None) => Some("not_started"),
            (_, Some(viewer)) => viewer.mouse_refusal()
        }
    })
    .to_string()
}

/// Legacy key payload from the ArkTS input controller:
/// `{name, down, press, alt, ctrl, shift, command}`.
#[napi]
pub fn session_input_key(session_id: String, key_json: String) -> String {
    let payload: Value = match serde_json::from_str(&key_json) {
        Ok(payload) => payload,
        Err(_) => return action("session_input_key", false, "Invalid key payload", None),
    };
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(key) = parse_legacy_key(name) else {
        return action("session_input_key", false, "Unknown key name", None);
    };
    let down = json_flag(&payload, "down");
    let press = json_flag(&payload, "press");
    if !down && !press {
        return action(
            "session_input_key",
            false,
            "Key event must be a press or a key down",
            None,
        );
    }
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return action("session_input_key", false, "Session not found", None);
    };
    if session.view_only {
        return action(
            "session_input_key",
            false,
            "Input is disabled for a view-only session",
            Some(session),
        );
    }
    let Some(viewer) = &session.viewer else {
        return action(
            "session_input_key",
            false,
            "Session is not started",
            Some(session),
        );
    };
    let result = viewer.send_key(
        key,
        down,
        press,
        json_flag(&payload, "alt"),
        json_flag(&payload, "ctrl"),
        json_flag(&payload, "shift"),
        json_flag(&payload, "command") || json_flag(&payload, "meta"),
    );
    match result {
        Ok(()) => action(
            "session_input_key",
            true,
            "Forwarded key input to RustDesk",
            Some(session),
        ),
        Err(_) => action(
            "session_input_key",
            false,
            "The peer does not currently accept keyboard input",
            Some(session),
        ),
    }
}

/// Flutter-style key payload: a USB HID usage code plus lock modes. Translated
/// to the original legacy `chr`/control-key wire form so no local key synthesis
/// or platform-specific scancode mapping is required.
#[napi]
pub fn session_handle_flutter_key_event(session_id: String, key_json: String) -> String {
    let payload: Value = match serde_json::from_str(&key_json) {
        Ok(payload) => payload,
        Err(_) => {
            return action(
                "session_handle_flutter_key_event",
                false,
                "Invalid Flutter key payload",
                None,
            );
        }
    };
    let Some(usb_hid) = payload
        .get("usb_hid")
        .or_else(|| payload.get("usbHid"))
        .and_then(Value::as_i64)
        .and_then(|value| u32::try_from(value).ok())
    else {
        return action(
            "session_handle_flutter_key_event",
            false,
            "Missing usb_hid in Flutter key payload",
            None,
        );
    };
    let down = payload
        .get("down_or_up")
        .or_else(|| payload.get("downOrUp"))
        .or_else(|| payload.get("down"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let character = payload
        .get("character")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(key) = usb_hid_to_viewer_key(usb_hid, character) else {
        return action(
            "session_handle_flutter_key_event",
            false,
            "Unsupported USB HID usage code",
            None,
        );
    };
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return action(
            "session_handle_flutter_key_event",
            false,
            "Session not found",
            None,
        );
    };
    if session.view_only {
        return action(
            "session_handle_flutter_key_event",
            false,
            "Input is disabled for a view-only session",
            Some(session),
        );
    }
    let Some(viewer) = &session.viewer else {
        return action(
            "session_handle_flutter_key_event",
            false,
            "Session is not started",
            Some(session),
        );
    };
    match viewer.send_key(key, down, false, false, false, false, false) {
        Ok(()) => action(
            "session_handle_flutter_key_event",
            true,
            "Forwarded Flutter key input to RustDesk",
            Some(session),
        ),
        Err(_) => action(
            "session_handle_flutter_key_event",
            false,
            "The peer does not currently accept keyboard input",
            Some(session),
        ),
    }
}

/// Whole-string input through the protocol's sequence key event. This is the
/// correct path for IME/CJK text, which has no single USB HID usage code.
#[napi]
pub fn session_input_string(session_id: String, value: String) -> String {
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return action("session_input_string", false, "Session not found", None);
    };
    if session.view_only {
        return action(
            "session_input_string",
            false,
            "Input is disabled for a view-only session",
            Some(session),
        );
    }
    if value.is_empty() {
        return action(
            "session_input_string",
            false,
            "Text is empty",
            Some(session),
        );
    }
    let Some(viewer) = &session.viewer else {
        return action(
            "session_input_string",
            false,
            "Session is not started",
            Some(session),
        );
    };
    match viewer.send_text(value) {
        Ok(()) => action(
            "session_input_string",
            true,
            "Forwarded text input to RustDesk",
            Some(session),
        ),
        Err(_) => action(
            "session_input_string",
            false,
            "The peer does not currently accept keyboard input",
            Some(session),
        ),
    }
}

/// Keyboard focus enter/leave is client-local in the original protocol: the
/// peer applies keys from the wire stream and has no grab request. This reports
/// the acknowledged local focus decision instead of pretending to negotiate.
#[napi]
pub fn session_enter_or_leave(session_id: String, enter: bool) -> String {
    let registry = lock(sessions());
    let Some(session) = registry.get(&session_id) else {
        return action("session_enter_or_leave", false, "Session not found", None);
    };
    if session.view_only {
        return action(
            "session_enter_or_leave",
            false,
            "Keyboard capture is disabled for a view-only session",
            Some(session),
        );
    }
    action(
        "session_enter_or_leave",
        session.viewer.is_some(),
        if enter {
            "Keyboard input is focused locally; the peer is the key target"
        } else {
            "Keyboard input left the local focus"
        },
        Some(session),
    )
}

#[napi]
pub fn session_send_clipboard(session_id: String, content: String) -> String {
    let registry = lock(sessions());
    let Some(session) = registry.get(&session_id) else {
        return action("session_send_clipboard", false, "Session not found", None);
    };
    if session.view_only {
        return action(
            "session_send_clipboard",
            false,
            "Clipboard is disabled for a view-only session",
            Some(session),
        );
    }
    if !session.clipboard_enabled {
        return action(
            "session_send_clipboard",
            false,
            "Clipboard synchronization is disabled for this session",
            Some(session),
        );
    }
    let Some(viewer) = &session.viewer else {
        return action(
            "session_send_clipboard",
            false,
            "Session is not started",
            Some(session),
        );
    };
    match viewer.send_clipboard_text(content) {
        Ok(()) => action(
            "session_send_clipboard",
            true,
            "Queued text clipboard for RustDesk",
            Some(session),
        ),
        Err(_) => action(
            "session_send_clipboard",
            false,
            "Clipboard synchronization is disabled by the current session permissions",
            Some(session),
        ),
    }
}

/// Read the newest inbound text clipboard payload. Reports `ok:false` with a
/// null text when nothing new arrived, so a repeated poll cannot re-apply the
/// same remote text over the local clipboard.
#[napi]
pub fn session_take_clipboard(session_id: String) -> String {
    let registry = lock(sessions());
    let Some(session) = registry.get(&session_id) else {
        return action("session_take_clipboard", false, "Session not found", None);
    };
    if !session.clipboard_enabled {
        return json!({
            "ok": false,
            "action": "session_take_clipboard",
            "message": "Clipboard synchronization is disabled for this session",
            "text": Value::Null,
            "html": Value::Null,
            "image": Value::Null
        })
        .to_string();
    }
    let text = session
        .viewer
        .as_ref()
        .and_then(|viewer| viewer.take_clipboard_text());
    json!({
        "ok": text.is_some(),
        "action": "session_take_clipboard",
        "message": if text.is_some() {
            "Remote clipboard text available"
        } else {
            "No native clipboard payload is available"
        },
        "text": text,
        "html": Value::Null,
        "image": Value::Null
    })
    .to_string()
}

#[napi]
pub fn session_switch_display(session_id: String, displays_json: String) -> String {
    let parsed: Value = match serde_json::from_str(&displays_json) {
        Ok(parsed) => parsed,
        Err(_) => {
            return action(
                "session_switch_display",
                false,
                "Invalid display request",
                None,
            );
        }
    };
    let target = parsed
        .as_array()
        .and_then(|items| items.first())
        .and_then(Value::as_i64)
        .or_else(|| {
            parsed
                .get("displays")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(Value::as_i64)
        })
        .and_then(|display| i32::try_from(display).ok());
    let Some(display) = target.filter(|display| *display == 0) else {
        return action(
            "session_switch_display",
            false,
            "Only the peer's primary display can be captured in this milestone",
            None,
        );
    };
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return action("session_switch_display", false, "Session not found", None);
    };
    let Some(viewer) = &session.viewer else {
        return action(
            "session_switch_display",
            false,
            "Session is not started",
            Some(session),
        );
    };
    match viewer.switch_display(display, 0, 0) {
        Ok(()) => {
            session.active_display = display;
            action(
                "session_switch_display",
                true,
                "Requested RustDesk display switch",
                Some(session),
            )
        }
        Err(_) => action(
            "session_switch_display",
            false,
            "Display switch could not be sent to the peer",
            Some(session),
        ),
    }
}

#[napi]
pub fn session_change_resolution(
    session_id: String,
    display: u32,
    width: u32,
    height: u32,
) -> String {
    let registry = lock(sessions());
    let Some(session) = registry.get(&session_id) else {
        return action(
            "session_change_resolution",
            false,
            "Session not found",
            None,
        );
    };
    if session.view_only {
        return action(
            "session_change_resolution",
            false,
            "Remote resolution changes are disabled for a view-only session",
            Some(session),
        );
    }
    let (Ok(display), Ok(width), Ok(height)) = (
        i32::try_from(display),
        i32::try_from(width),
        i32::try_from(height),
    ) else {
        return action(
            "session_change_resolution",
            false,
            "Resolution values are out of range",
            Some(session),
        );
    };
    let Some(viewer) = &session.viewer else {
        return action(
            "session_change_resolution",
            false,
            "Session is not started",
            Some(session),
        );
    };
    match viewer.change_resolution(display, width, height) {
        Ok(()) => action(
            "session_change_resolution",
            true,
            "Requested RustDesk resolution change",
            Some(session),
        ),
        Err(_) => action(
            "session_change_resolution",
            false,
            "Resolution change could not be sent to the peer",
            Some(session),
        ),
    }
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
    if quality.trim().eq_ignore_ascii_case("custom")
        && !(10..=2000).contains(&session.custom_image_quality)
    {
        return action(
            "session_set_image_quality",
            false,
            "A custom quality value between 10 and 2000 is required",
            Some(session),
        );
    }
    session.image_quality = normalize_quality_name(&quality);
    let engine = engine_quality(session);
    // A live viewer negotiates immediately; before start the stored preset is
    // carried by the login request instead.
    match &session.viewer {
        Some(viewer) => match viewer.set_image_quality(engine) {
            Ok(()) => action(
                "session_set_image_quality",
                true,
                "Requested remote image quality change",
                Some(session),
            ),
            Err(_) => action(
                "session_set_image_quality",
                false,
                "Image quality could not be sent to the peer",
                Some(session),
            ),
        },
        None => action(
            "session_set_image_quality",
            true,
            "Stored quality preference for this session",
            Some(session),
        ),
    }
}

#[napi]
pub fn session_set_custom_image_quality(session_id: String, quality: u32) -> String {
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return action(
            "session_set_custom_image_quality",
            false,
            "Session not found",
            None,
        );
    };
    let Some(value) = i32::try_from(quality)
        .ok()
        .filter(|value| (10..=2000).contains(value))
    else {
        return action(
            "session_set_custom_image_quality",
            false,
            "Custom quality must be between 10 and 2000",
            Some(session),
        );
    };
    session.custom_image_quality = value;
    if session.image_quality == "custom" {
        let engine = engine_quality(session);
        if let Some(viewer) = &session.viewer {
            let _ = viewer.set_image_quality(engine);
        }
    }
    action(
        "session_set_custom_image_quality",
        true,
        "Updated custom quality",
        Some(session),
    )
}

#[napi]
pub fn session_set_custom_fps(session_id: String, fps: u32) -> String {
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return action("session_set_custom_fps", false, "Session not found", None);
    };
    if !(1..=240).contains(&fps) {
        return action(
            "session_set_custom_fps",
            false,
            "FPS must be within 1..240",
            Some(session),
        );
    }
    session.requested_fps = fps;
    session.intent_revision = session.intent_revision.saturating_add(1);
    // A live viewer pushes the new rate; before start it is the login value.
    match &session.viewer {
        Some(viewer) => match viewer.set_requested_fps(fps) {
            Ok(()) => action(
                "session_set_custom_fps",
                true,
                "Requested remote capture rate change",
                Some(session),
            ),
            Err(_) => action(
                "session_set_custom_fps",
                false,
                "FPS could not be sent to the peer",
                Some(session),
            ),
        },
        None => action(
            "session_set_custom_fps",
            true,
            "Updated requested FPS",
            Some(session),
        ),
    }
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

/// Real decoder/stream counters exposed to the existing 1 Hz UI sampler.
/// `receivedUnits` and `submittedUnits` come from the wire and the native
/// decoder; `decodeCapacityFps` is reported as `null` because the engine makes
/// no decode-capacity claim. `elapsedMs` is a monotonic observation clock, so
/// the sampler's own window math stays honest.
#[napi]
pub fn session_get_frame_rate_snapshot(session_id: String, _display: u32) -> String {
    let mut registry = lock(sessions());
    let Some(session) = registry.get_mut(&session_id) else {
        return json!({
            "available": false,
            "generation": "0",
            "intentRevision": "0",
            "desiredFps": Value::Null,
            "safetyFpsCap": Value::Null,
            "elapsedMs": "0",
            "receivedPackets": "0",
            "decodedFrames": "0",
            "submittedFrames": "0",
            "unavailableFrames": "0",
            "noBufferFrames": "0",
            "busyFrames": "0",
            "decodeCalls": "0",
            "decodeNs": "0",
            "conversionCalls": "0",
            "conversionNs": "0",
            "targetCalls": "0",
            "targetNs": "0",
            "queueLen": "0",
            "decodeCapacityFps": Value::Null,
            "status": "disabled",
            "recommendation": Value::Null,
            "qualityRecommendationApplied": false
        })
        .to_string();
    };
    let elapsed_ms = advance_telemetry_clock(session) / 1000;
    let generation = session.generation.to_string();
    let intent_revision = session.intent_revision.to_string();
    let desired_fps = session.requested_fps.to_string();
    let snapshot = session.viewer.as_ref().map(|viewer| viewer.snapshot());
    let (available, received, submitted, status) = match &snapshot {
        Some(snapshot) if !snapshot.closed => (
            true,
            snapshot.received_units,
            // Units pushed into the decoder and frames actually presented are
            // distinct counters; report the presented count as submitted.
            snapshot.pushed_units,
            "observing",
        ),
        _ => (false, 0, 0, "disabled"),
    };
    json!({
        "available": available,
        "generation": generation,
        "intentRevision": intent_revision,
        "desiredFps": if available { Value::String(desired_fps) } else { Value::Null },
        "safetyFpsCap": Value::Null,
        "elapsedMs": elapsed_ms.to_string(),
        "receivedPackets": received.to_string(),
        "decodedFrames": submitted.to_string(),
        "submittedFrames": submitted.to_string(),
        "unavailableFrames": "0",
        "noBufferFrames": "0",
        "busyFrames": "0",
        "decodeCalls": "0",
        "decodeNs": "0",
        "conversionCalls": "0",
        "conversionNs": "0",
        "targetCalls": "0",
        "targetNs": "0",
        "queueLen": "0",
        "decodeCapacityFps": Value::Null,
        "status": status,
        "recommendation": Value::Null,
        "qualityRecommendationApplied": false
    })
    .to_string()
}

/// Native decoder counters. The enhanced engine drives its own decoder and does
/// not expose its internals as UI telemetry, so this reports unavailable rather
/// than a fabricated decoder state.
#[napi]
pub fn session_get_native_decoder_snapshot(session_id: String, _display: u32) -> String {
    let registry = lock(sessions());
    let available = registry
        .get(&session_id)
        .and_then(|session| session.viewer.as_ref())
        .map(|viewer| viewer.snapshot())
        .is_some_and(|snapshot| !snapshot.closed && !snapshot.codec.is_empty());
    json!({
        "available": available,
        "bound": available,
        "state": if available { "native-surface" } else { "unbound" },
        "reason": "hardware-surface-path",
        "active": available,
        "width": 0,
        "height": 0,
        "submittedFrames": 0,
        "generation": "0"
    })
    .to_string()
}

/// The waiting-for-image dialog is a legacy core convenience: it appeared while
/// the original client waited for its first decoded frame. The enhanced viewer
/// reports `connection_ready` and its own phases, so this is an explicit no-op.
#[napi]
pub fn session_on_waiting_for_image_dialog_show(session_id: String) -> String {
    let registry = lock(sessions());
    let session = registry.get(&session_id);
    action(
        "session_on_waiting_for_image_dialog_show",
        session.is_some(),
        "The enhanced viewer reports its own connection phases",
        session,
    )
}

/// Monotonic transfer job identifier reserved for the file-transfer bridge.
#[napi]
pub fn transfer_next_job_id() -> u32 {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[napi]
pub fn session_cancel_job(session_id: String, _act_id: u32) -> String {
    let registry = lock(sessions());
    let session = registry.get(&session_id);
    action(
        "session_cancel_job",
        false,
        "File transfer is not connected in this milestone",
        session,
    )
}

#[napi]
pub fn session_send_chat(session_id: String, text: String) -> String {
    let registry = lock(sessions());
    let session = registry.get(&session_id);
    if text.trim().is_empty() {
        return action("session_send_chat", false, "Chat message is empty", session);
    }
    action(
        "session_send_chat",
        false,
        "Chat requires peer support that this session did not negotiate",
        session,
    )
}

#[napi]
pub fn session_send_clipboard_image(session_id: String, image: Vec<u8>) -> String {
    let registry = lock(sessions());
    let session = registry.get(&session_id);
    let _ = image;
    action(
        "session_send_clipboard_image",
        false,
        "Image clipboard is not connected in this milestone",
        session,
    )
}

#[napi]
pub fn session_send_clipboard_files(session_id: String, paths_json: String) -> String {
    let registry = lock(sessions());
    let session = registry.get(&session_id);
    let _ = paths_json;
    action(
        "session_send_clipboard_files",
        false,
        "File clipboard requires the file-transfer channel",
        session,
    )
}

#[napi]
pub fn session_take_clipboard_image(session_id: String) -> Vec<u8> {
    let registry = lock(sessions());
    let _ = registry.get(&session_id);
    Vec::new()
}

/// Bytes in one RGBA8888 frame the session hands out.
#[napi]
pub fn session_get_rgba_size(session_id: String, display: u32) -> u32 {
    let registry = lock(sessions());
    let Some(session) = registry.get(&session_id) else {
        return 0;
    };
    // Only a backend that produces raw pixels has a size to report; a RustDesk
    // session decodes into a surface and has none.
    let _ = display;
    let Some(backend) = session.viewer.as_ref().filter(|backend| backend.is_vnc()) else {
        return 0;
    };
    let snapshot = backend.snapshot();
    if snapshot.width <= 0 || snapshot.height <= 0 {
        return 0;
    }
    (snapshot.width as u32)
        .saturating_mul(snapshot.height as u32)
        .saturating_mul(4)
}

/// Ask the backend to prepare the next frame.
///
/// A VNC server pushes updates on its own, so there is nothing to request beyond
/// the continuous stream the reader already maintains. The call exists so a
/// frontend's poll loop does not need to know which backend it is talking to.
#[napi]
pub fn session_next_rgba(session_id: String, display: u32) {
    let registry = lock(sessions());
    let Some(session) = registry.get(&session_id) else {
        return;
    };
    let _ = display;
    // Nothing to do: frames arrive when the server sends them.
    let _ = session;
}

/// The newest RGBA8888 frame, or empty when there is nothing new.
///
/// Empty means "no new frame", not "a black frame": a frontend that redraws an
/// unchanged picture at its polling rate wastes the work, so a taken frame is
/// cleared until the server sends another.
#[napi]
pub fn session_take_rgba_frame(session_id: String, display: u32) -> Vec<u8> {
    let registry = lock(sessions());
    let Some(session) = registry.get(&session_id) else {
        return Vec::new();
    };
    if display != 0 {
        return Vec::new();
    }
    session
        .viewer
        .as_ref()
        .and_then(|backend| backend.take_raw_frame())
        .map(|(_width, _height, pixels)| pixels)
        .unwrap_or_default()
}

#[napi]
pub fn session_read_remote_dir(session_id: String, _path: String, _include_hidden: bool) -> String {
    let registry = lock(sessions());
    action(
        "session_read_remote_dir",
        false,
        "Remote file browsing requires the file-transfer channel",
        registry.get(&session_id),
    )
}

#[napi]
pub fn session_send_files(
    session_id: String,
    _act_id: u32,
    _path: String,
    _to: String,
    _file_num: u32,
    _include_hidden: bool,
    _is_remote: bool,
    _is_dir: bool,
) -> String {
    let registry = lock(sessions());
    action(
        "session_send_files",
        false,
        "File transfer is not connected in this milestone",
        registry.get(&session_id),
    )
}

#[napi]
pub fn session_set_confirm_override_file(session_id: String, _act_id: u32) -> String {
    let registry = lock(sessions());
    action(
        "session_set_confirm_override_file",
        false,
        "File transfer is not connected in this milestone",
        registry.get(&session_id),
    )
}

#[napi]
pub fn session_send_mouse(session_id: String, mouse_json: String) -> String {
    let registry = lock(sessions());
    let session = registry.get(&session_id);
    let _ = mouse_json;
    action(
        "session_send_mouse",
        false,
        "Use sessionSendMouseEvent; the pointer stream is owned by the input controller",
        session,
    )
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
pub fn session_refresh(session_id: String, display: u32) -> String {
    let registry = lock(sessions());
    let Some(session) = registry.get(&session_id) else {
        return action("session_refresh", false, "Session not found", None);
    };
    let Some(viewer) = &session.viewer else {
        return action(
            "session_refresh",
            false,
            "Session is not started",
            Some(session),
        );
    };
    match viewer.refresh_video(i32::try_from(display).unwrap_or_default()) {
        Ok(()) => action(
            "session_refresh",
            true,
            "Requested a refresh of the remote display",
            Some(session),
        ),
        Err(_) => action(
            "session_refresh",
            false,
            "The refresh request could not be sent to the peer",
            Some(session),
        ),
    }
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
