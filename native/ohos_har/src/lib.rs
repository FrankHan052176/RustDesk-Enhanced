//! Modern-only HAR. src/lib.rs is an uncompiled historical reference.
//! No legacy application initialization, protocol implementation or pixel path.
mod host_bridge;
use napi_derive_ohos::napi;
use napi_ohos::{Error, Result, Status};
use rd_engine::{
    media_capability::{
        self, AdvertisedCodec, CapabilityError, CodecDirection, ScreenCaptureRateEvidence,
    },
    rendezvous::RendezvousConfig,
    viewer::{SurfaceLease, Viewer, ViewerError, ViewerOptions},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
};
use zeroize::Zeroizing;

fn failed(message: &'static str) -> Error {
    Error::new(Status::GenericFailure, message)
}
fn invalid(message: &'static str) -> Error {
    Error::new(Status::InvalidArg, message)
}

struct Initialization {
    directory: std::path::PathBuf,
    id: Option<String>,
}
static INITIALIZATION: Mutex<Option<Initialization>> = Mutex::new(None);

/// Call only after privacy consent, using the trusted Ability.filesDir, before
/// any connection. This is hbb_common configuration, NOT old Core/global_init.
#[napi]
pub fn engine_initialize(app_dir: String) -> Result<String> {
    #[cfg(not(target_env = "ohos"))]
    {
        let _ = app_dir;
        Err(failed("Modern app initialization requires OpenHarmony"))
    }
    #[cfg(target_env = "ohos")]
    {
        use hbb_common::config::{Config, APP_DIR, APP_HOME_DIR};
        use std::path::{Component, Path};
        let path = Path::new(&app_dir);
        if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(invalid("Expected the absolute Ability files directory"));
        }
        let directory = path
            .canonicalize()
            .map_err(|_| invalid("App directory is unavailable"))?;
        if !directory.is_dir() || directory.parent().is_none() {
            return Err(invalid("Expected an existing private App directory"));
        }
        let mut initialization = INITIALIZATION
            .lock()
            .map_err(|_| failed("Initialization unavailable"))?;
        if let Some(previous) = initialization.as_ref() {
            if previous.directory != directory {
                return Err(invalid("Cannot reinitialize with another App directory"));
            }
            return previous
                .id
                .clone()
                .ok_or_else(|| failed("App initialization previously failed"));
        }
        // Reserve even on failure: a partially initialized Config must never be
        // rebound to a different identity store. Release both path locks BEFORE
        // the first Config method, whose lazy loader reads those same globals.
        *initialization = Some(Initialization {
            directory,
            id: None,
        });
        hbb_common::sodiumoxide::init().map_err(|_| failed("Crypto initialization failed"))?;
        *APP_DIR
            .write()
            .map_err(|_| failed("App directory configuration unavailable"))? = app_dir.clone();
        *APP_HOME_DIR
            .write()
            .map_err(|_| failed("App home configuration unavailable"))? = app_dir;
        let id = Config::get_id();
        if id.is_empty() || !Config::file().is_file() {
            return Err(failed("Authoritative local identity unavailable"));
        }
        if let Some(state) = initialization.as_mut() {
            state.id = Some(id.clone());
        }
        Ok(id)
    }
}

fn require_local_identity(id: &str) -> Result<()> {
    let initialization = INITIALIZATION
        .lock()
        .map_err(|_| failed("Initialization unavailable"))?;
    match initialization
        .as_ref()
        .and_then(|state| state.id.as_deref())
    {
        Some(authoritative) if authoritative == id => Ok(()),
        Some(_) => Err(invalid(
            "Local identity does not match initialized configuration",
        )),
        None => Err(failed("Initialize after privacy consent before connecting")),
    }
}

/// UI contract: normal navigation keeps the live XComponent mounted until
/// engineClose resolves. If the platform itself destroys the Surface first, its
/// lifecycle callback must synchronously request cancellation; this lease's
/// explicit NativeWindow reference keeps the native object alive until close.
/// A numeric ID or an arbitrary napi_unwrap pointer cannot fulfill this lease.
struct NativeSurfaceLease {
    id: u64,
    #[cfg(target_env = "ohos")]
    window: std::ptr::NonNull<std::ffi::c_void>,
}

#[cfg(target_env = "ohos")]
#[link(name = "native_window")]
extern "C" {
    fn OH_NativeWindow_CreateNativeWindowFromSurfaceId(
        id: u64,
        window: *mut *mut std::ffi::c_void,
    ) -> i32;
    fn OH_NativeWindow_DestroyNativeWindow(window: *mut std::ffi::c_void);
    fn OH_NativeWindow_NativeObjectReference(object: *mut std::ffi::c_void) -> i32;
    fn OH_NativeWindow_NativeObjectUnreference(object: *mut std::ffi::c_void) -> i32;
}

impl NativeSurfaceLease {
    fn acquire(id: u64) -> Result<Arc<Self>> {
        #[cfg(target_env = "ohos")]
        {
            // Official API12; surface must have been created in THIS process.
            // https://developer.huawei.com/consumer/cn/doc/harmonyos-references/capi-external-window-h
            // Acquire before launching workers. The registry excludes overlapping
            // sessions on this surface. Reference operations are not concurrent
            // with final ArkUI/decoder teardown under the UI close contract.
            let mut raw = std::ptr::null_mut();
            let code = unsafe { OH_NativeWindow_CreateNativeWindowFromSurfaceId(id, &mut raw) };
            if code != 0 {
                return Err(failed("Cannot acquire live in-process NativeWindow"));
            }
            let window = std::ptr::NonNull::new(raw)
                .ok_or_else(|| failed("NativeWindow acquisition returned null"))?;
            // Separate explicit retained reference from the creation reference.
            // Destroy balances Create; Drop balances NativeObjectReference.
            let code = unsafe { OH_NativeWindow_NativeObjectReference(window.as_ptr()) };
            unsafe { OH_NativeWindow_DestroyNativeWindow(window.as_ptr()) };
            if code != 0 {
                return Err(failed("Cannot retain NativeWindow"));
            }
            Ok(Arc::new(Self { id, window }))
        }
        #[cfg(not(target_env = "ohos"))]
        {
            let _ = id;
            Err(failed("Native Surface viewer requires OpenHarmony"))
        }
    }
}

// Safety: no mutable SDK/window operation is exposed. Only the stable ID is
// shared. One official native ref is owned until the last Arc drops, after joined
// decoder teardown (or never while quarantined). UI must follow the close gate.
unsafe impl Send for NativeSurfaceLease {}
unsafe impl Sync for NativeSurfaceLease {}
unsafe impl SurfaceLease for NativeSurfaceLease {
    fn surface_id(&self) -> u64 {
        self.id
    }
}
impl Drop for NativeSurfaceLease {
    fn drop(&mut self) {
        #[cfg(target_env = "ohos")]
        unsafe {
            // A failed unreference cannot force-free the object. The pointer is
            // never published, reinterpreted from JS, or used after this call.
            let _ = OH_NativeWindow_NativeObjectUnreference(self.window.as_ptr());
        }
    }
}

struct Entry {
    viewer: Arc<Viewer>,
    // Registry retains an independent anchor, including on close failure or
    // cancellation, even if the core has already moved its lease to quarantine.
    lease: Arc<NativeSurfaceLease>,
    closing: AtomicBool,
    join_pending: AtomicBool,
}
const MAX_SESSIONS: usize = 4;
static CAPABILITY_QUERY_RUNNING: AtomicBool = AtomicBool::new(false);
struct CapabilityQueryGuard;
impl Drop for CapabilityQueryGuard {
    fn drop(&mut self) {
        CAPABILITY_QUERY_RUNNING.store(false, Ordering::Release);
    }
}
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static SESSIONS: OnceLock<Mutex<HashMap<String, Arc<Entry>>>> = OnceLock::new();
fn sessions() -> &'static Mutex<HashMap<String, Arc<Entry>>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn lookup(id: &str) -> Result<Arc<Entry>> {
    if id.len() > 64 {
        return Err(invalid("Invalid session ID"));
    }
    sessions()
        .lock()
        .map_err(|_| failed("Session registry unavailable"))?
        .get(id)
        .cloned()
        .ok_or_else(|| invalid("Unknown modern session"))
}
fn integer(value: f64, min: i64, max: i64) -> Result<i64> {
    if !value.is_finite() || value.fract() != 0.0 || value < min as f64 || value > max as f64 {
        Err(invalid("Expected an integer in the supported range"))
    } else {
        Ok(value as i64)
    }
}

#[napi]
pub fn engine_connect(
    surface_id: String,
    address: String,
    username: String,
    local_id: String,
    local_name: String,
    password: String,
    fps: f64,
) -> Result<String> {
    connect(
        surface_id,
        address,
        username,
        local_id,
        local_name,
        Zeroizing::new(password),
        fps,
        None,
    )
}

/// Explicit out-of-band peer pin. A server key or a self-reported connection
/// key is not an acceptable source of trust. Invalid input never opens a socket.
#[napi]
pub fn engine_connect_verified(
    surface_id: String,
    address: String,
    username: String,
    local_id: String,
    local_name: String,
    password: String,
    requested_fps: f64,
    expected_peer_id: String,
    peer_signing_key_base64: String,
) -> Result<String> {
    use hbb_common::base64::{engine::general_purpose::STANDARD, Engine as _};
    use hbb_common::sodiumoxide::crypto::sign;
    let password = Zeroizing::new(password);
    if expected_peer_id.is_empty()
        || expected_peer_id.len() > 512
        || peer_signing_key_base64.len() != 44
    {
        return Err(invalid("Invalid out-of-band host pin"));
    }
    let bytes = STANDARD
        .decode(peer_signing_key_base64.as_bytes())
        .map_err(|_| invalid("Expected a base64 Ed25519 peer signing public key"))?;
    let key = sign::PublicKey::from_slice(&bytes)
        .ok_or_else(|| invalid("Expected a 32-byte Ed25519 peer signing public key"))?;
    // The signed peer ID is also the original LoginRequest target. An IP route
    // must never replace it merely because ArkTS left the legacy username empty.
    let username = if username.is_empty() {
        expected_peer_id.clone()
    } else {
        username
    };
    connect(
        surface_id,
        address,
        username,
        local_id,
        local_name,
        password,
        requested_fps,
        Some((expected_peer_id, key)),
    )
}

/// Original RustDesk ID route. The initialized Config selects hbbs and its
/// public trust anchor; both hbbs-signed IdPk and peer-signed SignedId are
/// mandatory before password authentication. No address identity or TOFU.
#[napi]
pub fn engine_connect_id(
    surface_id: String,
    peer_id: String,
    local_id: String,
    local_name: String,
    password: String,
    requested_fps: f64,
) -> Result<String> {
    use hbb_common::{
        base64::{engine::general_purpose::STANDARD, Engine as _},
        config::{keys::OPTION_KEY, Config, RS_PUB_KEY},
        sodiumoxide::crypto::sign,
    };
    require_local_identity(&local_id)?;
    let surface_id = surface_id
        .parse::<u64>()
        .ok()
        .filter(|id| *id != 0)
        .ok_or_else(|| invalid("Invalid Surface ID"))?;
    let peer_id = peer_id.trim().to_owned();
    let mut password = Zeroizing::new(password);
    if peer_id.is_empty()
        || peer_id.len() > 256
        || peer_id.chars().any(char::is_control)
        || local_id.is_empty()
        || local_id.len() > 256
        || local_name.len() > 256
        || password.len() > 4096
    {
        return Err(invalid("Invalid modern ID connection options"));
    }
    let requested_fps = integer(requested_fps, 1, 240)? as u32;
    let rendezvous_server = Config::get_rendezvous_server();
    let configured_key = Config::get_option(OPTION_KEY);
    let (key_base64, licence_key) = if configured_key.is_empty() {
        (RS_PUB_KEY, RS_PUB_KEY.to_owned())
    } else {
        (configured_key.as_str(), configured_key.clone())
    };
    let key_bytes = STANDARD
        .decode(key_base64.as_bytes())
        .map_err(|_| failed("Configured rendezvous trust anchor is invalid"))?;
    let server_key = sign::PublicKey::from_slice(&key_bytes)
        .ok_or_else(|| failed("Configured rendezvous trust anchor is invalid"))?;
    let options = ViewerOptions {
        address: None,
        username: peer_id.clone(),
        local_id,
        local_name,
        password: std::mem::take(&mut *password),
        requested_fps,
    };
    let config = RendezvousConfig {
        id: peer_id,
        rendezvous_server,
        server_key,
        licence_key,
        relay_server: None,
        connect_timeout: std::time::Duration::from_secs(45),
    };
    register_viewer(surface_id, options, move |options, lease| {
        Viewer::start_rendezvous(options, lease, config)
    })
}

fn connect(
    surface_id: String,
    address: String,
    username: String,
    local_id: String,
    local_name: String,
    mut password: Zeroizing<String>,
    fps: f64,
    pin: Option<(String, hbb_common::sodiumoxide::crypto::sign::PublicKey)>,
) -> Result<String> {
    // Do not format/log arguments. Validation errors are fixed/redacted. Core
    // receives sole ownership of the password on success; early failures wipe it.
    require_local_identity(&local_id)?;
    let surface_id = surface_id
        .parse::<u64>()
        .ok()
        .filter(|id| *id != 0)
        .ok_or_else(|| invalid("Invalid Surface ID"))?;
    let address: SocketAddr = address
        .parse()
        .map_err(|_| invalid("Use an IP address and port"))?;
    // Original direct-IP LoginRequest target convention, NOT a verified peer
    // identity. Never promote an IP-derived username into signature evidence.
    let username = if username.is_empty() {
        address.ip().to_string()
    } else {
        username
    };
    if address.port() == 0
        || username.is_empty()
        || username.len() > 256
        || local_id.is_empty()
        || local_id.len() > 256
        || local_name.len() > 256
        || password.len() > 4096
    {
        return Err(invalid("Invalid modern connection options"));
    }
    let requested_fps = integer(fps, 1, 240)? as u32;
    let options = ViewerOptions {
        address: Some(address),
        username,
        local_id,
        local_name,
        password: std::mem::take(&mut *password),
        requested_fps,
    };
    // start is nonblocking; no authentication or media work runs on the UI thread.
    register_viewer(surface_id, options, move |options, lease| match pin {
        Some((expected_id, key)) => Viewer::start_verified(options, lease, expected_id, key),
        None => Viewer::start(options, lease),
    })
}

fn register_viewer(
    surface_id: u64,
    options: ViewerOptions,
    start: impl FnOnce(
        ViewerOptions,
        Arc<NativeSurfaceLease>,
    ) -> std::result::Result<Arc<Viewer>, ViewerError>,
) -> Result<String> {
    let mut registry = sessions()
        .lock()
        .map_err(|_| failed("Session registry unavailable"))?;
    if registry.len() >= MAX_SESSIONS {
        return Err(failed(
            "Modern session limit reached; pending/quarantined sessions retain their slots",
        ));
    }
    if registry.values().any(|entry| entry.lease.id == surface_id) {
        return Err(failed("Surface is still owned by a modern session"));
    }
    let sequence = NEXT_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| failed("Session identifier space exhausted"))?;
    let lease = NativeSurfaceLease::acquire(surface_id)?;
    let viewer =
        start(options, lease.clone()).map_err(|_| failed("Modern viewer could not start"))?;
    let id = format!("modern-{sequence:x}");
    registry.insert(
        id.clone(),
        Arc::new(Entry {
            viewer,
            lease,
            closing: AtomicBool::new(false),
            join_pending: AtomicBool::new(false),
        }),
    );
    Ok(id)
}

#[napi]
pub fn engine_snapshot(id: String) -> Result<String> {
    let entry = lookup(&id)?;
    let state = entry.viewer.snapshot();
    // Core's snapshot contract is redacted; counters are decode/render submission
    // evidence, NEVER physical presentation rate or measured HDR capability.
    Ok(json!({
        "phase": state.phase, "error": state.error, "width": state.width,
        "height": state.height, "codec": state.codec, "route": state.route,
        "received_units": state.received_units, "pushed_units": state.pushed_units,
        "render_submissions": state.render_submissions,
        "keyboard_allowed": state.keyboard_allowed, "closed": state.closed
        , "encrypted": state.encrypted, "peer_verified": state.peer_verified
    })
    .to_string())
}

#[napi]
pub fn engine_mouse(id: String, kind: f64, button: f64, x: f64, y: f64) -> Result<bool> {
    let kind = integer(kind, 0, u32::MAX as i64)? as u32;
    let button = integer(button, 0, u32::MAX as i64)? as u32;
    let x = integer(x, i32::MIN as i64, i32::MAX as i64)? as i32;
    let y = integer(y, i32::MIN as i64, i32::MAX as i64)? as i32;
    let entry = lookup(&id)?;
    if entry.closing.load(Ordering::Acquire) {
        return Ok(false);
    }
    // Core alone maps upstream event kinds and gates on auth/remote/local policy.
    Ok(entry.viewer.send_mouse(kind, button, x, y).is_ok())
}

/// Synchronous N-API cancellation for UI/surface teardown. No deferred JS/async
/// task is needed to request cancellation, and this NEVER releases the lease.
#[napi]
pub fn engine_request_close(id: String) -> Result<()> {
    let entry = lookup(&id)?;
    entry.closing.store(true, Ordering::Release);
    entry.viewer.request_close();
    Ok(())
}

#[napi]
pub async fn engine_close(id: String) -> Result<()> {
    let entry = lookup(&id)?;
    entry.closing.store(true, Ordering::Release);
    entry.viewer.request_close();
    if entry.join_pending.swap(true, Ordering::AcqRel) {
        return Err(failed(
            "Close already pending or quarantined; keep the Surface mounted",
        ));
    }
    // Neither cancellation nor an error removes the registry/Surface anchor.
    // No timeout force-free: a stuck native close keeps this Promise pending.
    entry
        .viewer
        .close()
        .await
        .map_err(|_| failed("Decoder teardown not proven; keep the Surface mounted"))?;
    sessions()
        .lock()
        .map_err(|_| failed("Session registry unavailable; keep the Surface mounted"))?
        .remove(&id);
    Ok(())
}

fn evidence<T>(
    result: std::result::Result<T, CapabilityError>,
    value: impl FnOnce(T) -> Value,
) -> Value {
    match result {
        Ok(v) => json!({"ok": true, "value": value(v)}),
        // This enum contains only platform API names/codes and codec direction;
        // unlike session options it has no credentials or peer identifiers.
        Err(error) => json!({"ok": false, "error": format!("{error:?}")}),
    }
}
fn codec_metadata(codec: AdvertisedCodec) -> Value {
    let sizes: Vec<Value> = codec.sizes.into_iter().map(|size| json!({
        "width": size.target.width, "height": size.target.height, "fps": size.target.fps,
        "size_supported": size.size_supported, "size_and_rate_supported": size.size_and_rate_supported,
        "frame_rate_range": evidence(size.frame_rate_range, |r| json!({"min": r.min, "max": r.max}))
    })).collect();
    json!({
        "direction": match codec.direction { CodecDirection::Encode => "encode", CodecDirection::Decode => "decode" },
        "mime": codec.mime, "codec_name": codec.codec_name, "hardware": codec.hardware,
        "profiles": evidence(codec.profiles, |v| json!(v)),
        "main10_advertised": evidence(codec.main10_advertised, |v| json!(v)),
        "main10_levels": evidence(codec.main10_levels, |v| json!(v)),
        "native_buffer_formats": evidence(codec.native_buffer_formats, |v| json!(v)),
        "pixel_formats": evidence(codec.pixel_formats, |v| json!(v)),
        "width_alignment": evidence(codec.width_alignment, |v| json!(v)),
        "height_alignment": evidence(codec.height_alignment, |v| json!(v)), "sizes": sizes
    })
}

#[napi]
pub async fn engine_capabilities() -> Result<String> {
    if CAPABILITY_QUERY_RUNNING.swap(true, Ordering::AcqRel) {
        return Err(failed("Capability query already pending"));
    }
    let guard = CapabilityQueryGuard;
    hbb_common::tokio::task::spawn_blocking(move || {
        // Bound actual blocking work, even if its JS Promise is abandoned.
        let _guard = guard;
        let report = media_capability::query_hevc_hardware_capabilities();
        let capture = match report.screen_capture_rate {
            ScreenCaptureRateEvidence::OhosDocumentedMaximum { fps, source } => {
                json!({"kind": "documented-maximum-only", "fps": fps, "source": source})
            }
            ScreenCaptureRateEvidence::NotApplicableToPlatform => {
                json!({"kind": "unsupported-platform"})
            }
        };
        json!({
            "evidence": "advertised-only", "codec_family": "HEVC",
            "encoder": evidence(report.encoder, codec_metadata),
            "decoder": evidence(report.decoder, codec_metadata),
            "screen_capture_rate": capture, "codec_throughput": "not-measured",
            "hdr_display": "not-measured", "screen_capture_hdr": "not-measured",
            "reference": media_capability::CAPABILITY_REFERENCE
        })
        .to_string()
    })
    .await
    .map_err(|_| failed("Capability query worker failed"))
}
