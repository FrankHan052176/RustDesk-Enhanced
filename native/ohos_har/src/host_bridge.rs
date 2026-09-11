//! Thin, explicitly user-started VIEW-ONLY host bridge. No capture/encoder or
//! authorization implementation lives here. Core requires each local approval
//! and the system screen-recording consent; wire flags cannot grant permission.
use super::{failed, integer, invalid, INITIALIZATION};
use hbb_common::{
    base64::{engine::general_purpose::STANDARD, Engine as _},
    config::Config,
    sodiumoxide::crypto::sign,
};
use napi_derive_ohos::napi;
use napi_ohos::{Error, Result, Status};
use rd_engine::{
    host::{Host, HostOptions},
    publisher::{CodecSelection, PublisherBackend},
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use zeroize::Zeroizing;

struct HostEntry {
    id: String,
    host: Host,
    closing: AtomicBool,
    join_pending: AtomicBool,
}
// One live/pending/quarantined host owner, never evicted to admit a replacement.
static HOST: Mutex<Option<Arc<HostEntry>>> = Mutex::new(None);
static NEXT_HOST_ID: AtomicU64 = AtomicU64::new(1);

fn local_id() -> Result<String> {
    INITIALIZATION
        .lock()
        .map_err(|_| failed("App initialization unavailable"))?
        .as_ref()
        .and_then(|state| state.id.clone())
        .ok_or_else(|| failed("Initialize after privacy consent before enabling a host"))
}

fn identity_material() -> Result<(String, sign::SecretKey, sign::PublicKey)> {
    // This gate precedes ALL Config access, so filesDir is already bound by
    // engineInitialize. Config remains the sole ID/key authority.
    let id = local_id()?;
    let (secret, public) = Config::get_key_pair();
    let secret = Zeroizing::new(secret);
    let signing_key = sign::SecretKey::from_slice(&secret)
        .ok_or_else(|| failed("Configured Ed25519 signing key has invalid length"))?;
    let public_key = sign::PublicKey::from_slice(&public)
        .ok_or_else(|| failed("Configured Ed25519 public key has invalid length"))?;
    // The copied secret Vec is wiped on every path; the owned sodium SecretKey
    // also wipes on Drop. Never derive Debug or serialize the key-pair tuple.
    Ok((id, signing_key, public_key))
}

fn lookup(id: &str) -> Result<Arc<HostEntry>> {
    if id.len() > 64 {
        return Err(invalid("Invalid modern host handle"));
    }
    HOST.lock()
        .map_err(|_| failed("Host registry unavailable"))?
        .as_ref()
        .filter(|entry| entry.id == id)
        .cloned()
        .ok_or_else(|| invalid("Unknown modern host"))
}

/// Explicit user enable only. This does not approve an incoming peer or imply
/// that screen capture consent was granted. The listener is view-only in Core.
#[napi]
pub fn engine_host_start(width: f64, height: f64, fps: f64) -> Result<String> {
    let width = integer(width, 2, 3840)? as i32;
    let height = integer(height, 2, 3840)? as i32;
    let fps = integer(fps, 1, 60)? as u32;
    if width % 2 != 0 || height % 2 != 0 || i64::from(width) * i64::from(height) > 3840 * 2160 {
        return Err(invalid(
            "Host geometry must be even and at most UHD pixel area",
        ));
    }
    let mut registry = HOST
        .lock()
        .map_err(|_| failed("Host registry unavailable"))?;
    if registry.is_some() {
        return Err(failed("A host is still active, closing or quarantined"));
    }
    let (id, signing_key, _public_key) = identity_material()?;
    let sequence = NEXT_HOST_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| failed("Host identifier space exhausted"))?;
    let host = Host::start(HostOptions {
        listen: ([0, 0, 0, 0], 21118).into(),
        id,
        signing_key,
        width,
        height,
        fps,
        bitrate: 16_000_000,
        platform: "HarmonyOS".into(),
        publisher_backend: PublisherBackend::Auto,
        output_index: 0,
        codec_selection: CodecSelection::Auto,
        // The OHOS controlled side has no input-injection backend yet, so the
        // host stays receive-only and never advertises the keyboard permission.
        // Enabling the option without a sink would be refused anyway; this keeps
        // the intent explicit at the call site.
        input_injection: false,
        input_sink: None,
    })
    .map_err(|error| Error::new(Status::GenericFailure, error.to_string()))?;
    let id = format!("modern-host-{sequence:x}");
    *registry = Some(Arc::new(HostEntry {
        id: id.clone(),
        host,
        closing: AtomicBool::new(false),
        join_pending: AtomicBool::new(false),
    }));
    Ok(id)
}

#[napi]
pub fn engine_host_snapshot(id: String) -> Result<String> {
    let entry = lookup(&id)?;
    let state = entry.host.snapshot();
    // HostError::Display is the Core's redacted contract. sent_units/bytes are
    // wire-send evidence, not capture FPS, physical presentation or HDR proof.
    Ok(json!({
        "phase": state.phase,
        "error": state.error.map(|error| error.to_string()),
        "approval_request": state.approval_request,
        "approval_origin": state.approval_origin,
        "width": state.width, "height": state.height, "fps": state.fps,
        "codec": state.codec, "connected": state.connected,
        "encrypted": state.encrypted, "sent_units": state.sent_units,
        "sent_bytes": state.sent_bytes, "closed": state.closed
    })
    .to_string())
}

#[napi]
pub fn engine_host_approve(id: String, request_id: String, allow: bool) -> Result<bool> {
    if request_id.is_empty() || request_id.len() > 256 {
        return Err(invalid("Invalid local approval request"));
    }
    let entry = lookup(&id)?;
    if entry.closing.load(Ordering::Acquire) {
        return Ok(false);
    }
    // Only this explicit local UI action reaches Core approval; no peer wire
    // message or host-start event implies acceptance. Input remains unsupported.
    Ok(entry.host.approve(&request_id, allow))
}

#[napi]
pub fn engine_host_request_close(id: String) -> Result<()> {
    let entry = lookup(&id)?;
    entry.closing.store(true, Ordering::Release);
    entry.host.request_close();
    Ok(())
}

#[napi]
pub async fn engine_host_close(id: String) -> Result<()> {
    let entry = lookup(&id)?;
    entry.closing.store(true, Ordering::Release);
    entry.host.request_close();
    if entry.join_pending.swap(true, Ordering::AcqRel) {
        return Err(failed("Host close already pending or quarantined"));
    }
    // No fire-and-forget free, timeout force-free or eviction. Failure/cancel
    // retains the sole registry owner while Core joins/quarantines native state.
    entry
        .host
        .close()
        .await
        .map_err(|error| Error::new(Status::GenericFailure, error.to_string()))?;
    let mut registry = HOST
        .lock()
        .map_err(|_| failed("Host registry unavailable"))?;
    if registry.as_ref().is_some_and(|current| current.id == id) {
        registry.take();
    }
    Ok(())
}

/// Only the local public identity is returned for deliberate out-of-band pinning.
/// This is NOT a peer-discovery/TOFU endpoint and never exports a secret key.
#[napi]
pub fn engine_host_identity() -> Result<String> {
    let (id, _secret, public) = identity_material()?;
    Ok(json!({"id": id, "peer_signing_key_base64": STANDARD.encode(public.0)}).to_string())
}
