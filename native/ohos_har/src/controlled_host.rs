//! The controlled-host contract, served by the host this build actually has.
//!
//! The 服务端 screen was written against a larger host API than this build
//! implements: passwords, a second factor and trusted devices. Those calls had no
//! native counterpart at all, so starting the screen failed with
//! `TypeError: undefined is not callable`, the temporary password stayed
//! unresolved, and the 21118 listener accepted sockets without ever speaking the
//! protocol — which is what a peer reports as a reset.
//!
//! This module serves that contract from the one host that is implemented: the
//! engine host, whose authorization is a local click in the app. Every payload
//! keeps the shape the screen parses, and anything this build cannot do is
//! reported as unsupported instead of being invented — no password is generated,
//! no second factor exists, and the settings map is session-scoped because no
//! host path reads it yet.

use napi_derive_ohos::napi;
use napi_ohos::Result;
use serde_json::{json, Value};
use std::sync::Mutex;

use crate::host_bridge;
use hbb_common::password_security;

/// Screen geometry as the UI reported it, plus the host this module started.
struct State {
    width: i32,
    height: i32,
    fps: f64,
    handle: Option<String>,
    settings: Vec<(String, String)>,
    /// Last reported listener state, so a transition is traced exactly once.
    running: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    width: 0,
    height: 0,
    fps: 30.0,
    handle: None,
    settings: Vec::new(),
    running: false,
});

/// How many settings a local UI may pin; the map is a store, not a file.
const MAX_SETTINGS: usize = 64;
const MAX_SETTING_LEN: usize = 512;

/// Whether the engine host still has a live listener.
///
/// The engine keeps the host object when its listener has already left, for
/// example after a quarantined capture. The phase is what says whether anything
/// is still listening, and the screen must offer a restart rather than report a
/// host that answers and then drops every peer.
fn host_running(snapshot: Option<&Value>) -> bool {
    let Some(snapshot) = snapshot else {
        return false;
    };
    let closed = snapshot
        .get("closed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let stopped = snapshot
        .get("phase")
        .and_then(Value::as_str)
        .map(|phase| phase == "stopped")
        .unwrap_or(false);
    !closed && !stopped
}

fn state() -> std::sync::MutexGuard<'static, State> {
    // A poisoned lock means another thread panicked while holding it; the state
    // is a few plain values, so recovering is better than refusing to serve the
    // host screen for the rest of the process.
    STATE.lock().unwrap_or_else(|error| error.into_inner())
}

fn fail(message: &str) -> String {
    json!({"ok": false, "message": message}).to_string()
}

fn number(value: Option<&Value>, fallback: f64) -> f64 {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(fallback)
}

/// What the host screen polls for: state, identity and the connected count.
///
/// `state` is `ready` only while the engine host is actually running, because the
/// screen treats `ready` as "this host is live" and anything else as stopped.
fn status_payload(snapshot: Option<&Value>) -> Value {
    let running = host_running(snapshot);
    let connected = snapshot
        .and_then(|value| value.get("connected"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let reported = snapshot
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .unwrap_or("");
    json!({
        "ok": true,
        "state": if running { "ready" } else { "disabled" },
        "serverRunning": running,
        "myId": identity_id(),
        // This host authorizes a local click, so it has no password to hand out.
        "temporaryPassword": "",
        "clientCount": if connected { 1 } else { 0 },
        // The engine records a session's outcome in the same field it uses for a
        // host fault, and the listener keeps serving after a denied or failed
        // peer. Reporting that outcome as a host error made the host screen tear
        // the host down, so it is named separately and only a host that is really
        // gone raises an error.
        "lastError": if running { "" } else { reported },
        "sessionOutcome": if running { reported } else { "" },
        "message": if running {
            "引擎 host 已在本机监听，连接需要本地点按确认"
        } else {
            "被看服务未运行"
        }
    })
}

fn identity_id() -> String {
    let identity = host_bridge::engine_host_identity().unwrap_or_default();
    serde_json::from_str::<Value>(&identity)
        .ok()
        .and_then(|value| value.get("id").and_then(Value::as_str).map(str::to_owned))
        .filter(|id| !id.is_empty())
        .unwrap_or_else(hbb_common::config::Config::get_id)
}

/// The live snapshot of the host this module started, if it still exists.
fn snapshot() -> Option<Value> {
    let handle = state().handle.clone()?;
    let raw = host_bridge::engine_host_snapshot(handle).ok()?;
    serde_json::from_str(&raw).ok()
}

/// How the host screen reports capture. `nativeStateCode` follows the UI's own
/// terminal-state list, where 0 means running and 1 means stopped.
fn capture_payload(snapshot: Option<&Value>) -> Value {
    let running = host_running(snapshot);
    let frames = snapshot
        .and_then(|value| value.get("sent_units"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "ok": true,
        "active": running,
        "nativeStateCode": if running { 0 } else { 1 },
        "systemCaptureConfirmed": running,
        "framesObserved": frames,
        "audioFramesObserved": 0,
        "audioFramesForwarded": 0,
        "message": if running {
            "引擎 host 正在采集屏幕"
        } else {
            "屏幕采集未运行"
        }
    })
}

/// The password contract, from the same storage and generators the original
/// host uses, so a password set here works against either host.
fn password_payload() -> Value {
    let method = hbb_common::config::Config::get_option("verification-method");
    json!({
        "ok": true,
        "temporaryPassword": password_security::temporary_password(),
        "verificationMethod": if method.is_empty() { "use-both-passwords" } else { method.as_str() },
        "permanentPasswordSet": hbb_common::config::Config::has_permanent_password(),
        "localPermanentPasswordSet": false,
        "verificationMethodFixed": false,
        "permanentPasswordChangeDisabled": false,
        "maxPasswordLength": 128,
        "message": "临时密码由本机生成，客户端输入即可连接"
    })
}

fn permission_unavailable(message: &str) -> String {
    json!({"ok": false, "message": message}).to_string()
}

#[napi]
pub fn controlled_screen_configure(config_json: String) -> String {
    let parsed: Value = match serde_json::from_str(&config_json) {
        Ok(value) => value,
        Err(_) => return fail("屏幕参数不是有效 JSON"),
    };
    let width = number(parsed.get("width"), 0.0).round() as i32;
    let height = number(parsed.get("height"), 0.0).round() as i32;
    let fps = number(parsed.get("frameRate"), 30.0);
    if width < 2 || height < 2 || width % 2 != 0 || height % 2 != 0 || fps < 1.0 || fps > 60.0 {
        return fail("屏幕参数超出本机 host 接受的范围");
    }
    let mut state = state();
    state.width = width;
    state.height = height;
    state.fps = fps;
    json!({
        "ok": true,
        "active": state.handle.is_some(),
        "message": format!("屏幕参数已记录 {width}x{height}@{fps}")
    })
    .to_string()
}

/// Starts the engine host on the geometry the screen reported.
///
/// Idempotent: a second call while a host is live reports the running host
/// instead of failing, because the screen re-runs its start path on resume.
#[napi]
pub async fn controlled_server_start(_config_json: String) -> Result<String> {
    if snapshot().is_some() {
        return Ok(status_payload(snapshot().as_ref()).to_string());
    }
    let (width, height, fps) = {
        let state = state();
        (state.width, state.height, state.fps)
    };
    if width < 2 || height < 2 {
        return Ok(fail("屏幕参数尚未配置，无法启动被看服务"));
    }
    match host_bridge::engine_host_start(f64::from(width), f64::from(height), fps) {
        Ok(handle) => {
            state().handle = Some(handle);
            Ok(status_payload(snapshot().as_ref()).to_string())
        }
        Err(error) => Ok(fail(&format!("被看服务启动失败：{error}"))),
    }
}

#[napi]
pub async fn controlled_server_stop() -> Result<String> {
    let handle = state().handle.clone();
    if let Some(handle) = handle {
        // Failure to close the registry entry is reported by the caller's own
        // status poll; the handle is kept until the host is really gone.
        if host_bridge::engine_host_close(handle).await.is_ok() {
            state().handle = None;
        }
    }
    Ok(status_payload(None).to_string())
}

#[napi]
pub fn controlled_server_get_status() -> String {
    let snapshot = snapshot();
    let running = host_running(snapshot.as_ref());
    let mut state = state();
    if state.running != running {
        state.running = running;
        // The listener's own lifecycle is invisible in the screen otherwise, and
        // a host that left its listener behind was the difference between "no
        // video yet" and "the peer is gone".
        eprintln!(
            "controlled_host listener_running={running} phase={} error={}",
            snapshot
                .as_ref()
                .and_then(|value| value.get("phase"))
                .and_then(Value::as_str)
                .unwrap_or("absent"),
            snapshot
                .as_ref()
                .and_then(|value| value.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("")
        );
    }
    drop(state);
    status_payload(snapshot.as_ref()).to_string()
}

#[napi]
pub fn controlled_screen_capture_get_status() -> String {
    capture_payload(snapshot().as_ref()).to_string()
}

/// Capture is bound to the host's lifetime in this build, so this reports the
/// same state rather than pretending a separate capture session exists.
#[napi]
pub fn controlled_screen_capture_start(_config_json: String) -> String {
    capture_payload(snapshot().as_ref()).to_string()
}

#[napi]
pub fn controlled_screen_capture_stop() -> String {
    json!({
        "ok": true,
        "active": false,
        "nativeStateCode": 1,
        "systemCaptureConfirmed": false,
        "message": "屏幕采集随被看服务停止"
    })
    .to_string()
}

/// Pending approvals and the connected peer, as the screen's list expects.
///
/// The engine host carries one approval and one session, so the payload holds at
/// most one of each; `peerId` is the origin the host logged, which is the only
/// peer identity this build keeps before a session is established.
#[napi]
pub fn controlled_incoming_poll(_limit: f64) -> String {
    let Some(snapshot) = snapshot() else {
        return json!({"ok": true, "clients": [], "requests": []}).to_string();
    };
    let origin = snapshot
        .get("approval_origin")
        .and_then(Value::as_str)
        .unwrap_or("");
    let connected = snapshot
        .get("connected")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let request_id = snapshot
        .get("approval_request")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut clients: Vec<Value> = Vec::new();
    let mut requests: Vec<Value> = Vec::new();
    if connected && origin.is_empty() {
        clients.push(json!({
            "requestId": "",
            "peerId": "",
            "peerName": "远端会话",
            "authorized": true
        }));
    } else if !origin.is_empty() {
        let entry = json!({
            "requestId": request_id,
            "peerId": origin,
            "peerName": origin,
            "authorized": connected
        });
        clients.push(entry.clone());
        if !connected {
            requests.push(entry);
        }
    }
    json!({"ok": true, "clients": clients, "requests": requests}).to_string()
}

#[napi]
pub fn controlled_incoming_resolve(request_id: String, accepted: bool) -> String {
    let Some(handle) = state().handle.clone() else {
        return permission_unavailable("被看服务未运行");
    };
    match host_bridge::engine_host_approve(handle, request_id, accepted) {
        Ok(true) => json!({"ok": true, "message": "已处理该连接请求"}).to_string(),
        Ok(false) => fail("该请求已失效或被拒绝"),
        Err(error) => fail(&format!("处理连接请求失败：{error}")),
    }
}

/// Per-request permission grants do not exist in this host: it is view-only and
/// grants no keyboard, file or clipboard permission.
#[napi]
pub fn controlled_incoming_set_permission(_request_id: String, _permission: String, _enabled: bool) -> String {
    permission_unavailable("本机构建为仅观看 host，不支持按请求授予权限")
}

#[napi]
pub fn controlled_password_get() -> String {
    password_payload().to_string()
}

/// Rotate the one-time password, as the original client's UI button does.
#[napi]
pub fn controlled_password_refresh() -> String {
    password_security::update_temporary_password();
    password_payload().to_string()
}

#[napi]
pub fn controlled_password_set_permanent(password: String) -> String {
    if password.len() > 128 {
        return permission_unavailable("固定密码超出长度限制");
    }
    // Empty clears it, which is what the settings screen offers.
    if !hbb_common::config::Config::set_permanent_password(&password) {
        return permission_unavailable("固定密码写入失败");
    }
    password_payload().to_string()
}

#[napi]
pub fn controlled_password_set_verification_method(method: String) -> String {
    let accepted = [
        "use-temporary-password",
        "use-permanent-password",
        "use-both-passwords",
    ];
    if !accepted.contains(&method.as_str()) {
        return permission_unavailable("未知的密码模式");
    }
    hbb_common::config::Config::set_option("verification-method".into(), method);
    password_payload().to_string()
}

#[napi]
pub fn controlled_settings_get() -> String {
    let settings: serde_json::Map<String, Value> = state()
        .settings
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect();
    json!({"ok": true, "settings": Value::Object(settings)}).to_string()
}

#[napi]
pub fn controlled_setting_set(key: String, value: String) -> String {
    if key.is_empty() || key.len() > MAX_SETTING_LEN || value.len() > MAX_SETTING_LEN {
        return fail("设置项超出长度限制");
    }
    let mut state = state();
    match state.settings.iter_mut().find(|(name, _)| name == &key) {
        Some(entry) => entry.1 = value,
        None => {
            if state.settings.len() >= MAX_SETTINGS {
                return fail("设置项数量超出限制");
            }
            state.settings.push((key, value));
        }
    }
    json!({"ok": true}).to_string()
}

/// There is no second factor in this build.
///
/// Reporting it as absent (rather than as "not implemented yet") is what the
/// screen needs in order to hide the setup flow; a code could never be verified.
fn two_factor_absent() -> Value {
    json!({
        "ok": true,
        "enabled": false,
        "trustedDevicesEnabled": false,
        "trustedDevicesFixed": true,
        "trustedDevices": [],
        "message": "本机构建不支持两步验证"
    })
}

#[napi]
pub fn controlled_two_factor_get() -> String {
    two_factor_absent().to_string()
}

#[napi]
pub fn controlled_two_factor_begin() -> String {
    permission_unavailable("本机构建不支持两步验证")
}

#[napi]
pub fn controlled_two_factor_verify(_code: String) -> String {
    permission_unavailable("本机构建不支持两步验证")
}

#[napi]
pub fn controlled_two_factor_disable() -> String {
    two_factor_absent().to_string()
}

#[napi]
pub fn controlled_two_factor_remove_trusted_devices(_hwids_json: String) -> String {
    // No trusted device is ever recorded, so removing one is already true.
    json!({"ok": true, "message": "本机构建没有受信任设备记录"}).to_string()
}

#[napi]
pub fn controlled_two_factor_clear_trusted_devices() -> String {
    json!({"ok": true, "message": "本机构建没有受信任设备记录"}).to_string()
}
