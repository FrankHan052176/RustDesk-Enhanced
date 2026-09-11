//! Compatibility entry points required while the existing ArkTS frontend is
//! moved onto `rd-engine`. This module adapts DTOs only; it never implements
//! RustDesk wire or authentication semantics.

use hbb_common::config::Config;
use librustdesk::media_capability::{
    query_h264_hardware_decoder, query_hevc_hardware_capabilities, AdvertisedCodec, CapabilityError,
};
use napi_derive_ohos::napi;
use napi_ohos::Result;
use serde_json::{json, Value};

const UNSUPPORTED_MESSAGE: &str = "Not implemented by RustDesk Enhanced";

fn action(ok: bool, message: &str) -> String {
    json!({"ok": ok, "message": message}).to_string()
}

fn decoder_row(
    codec: &str,
    mime: &str,
    result: std::result::Result<AdvertisedCodec, CapabilityError>,
) -> Value {
    match result {
        Ok(capability) => {
            let available = capability.hardware && !capability.codec_name.is_empty();
            let name = capability.codec_name;
            json!({
                "codec": codec,
                "mime": mime,
                "recommendedAvailable": available,
                "mimeAvailable": available,
                "recommendedName": name.clone(),
                "recommendedIsHardware": available,
                "hardwareAvailable": available,
                "hardwareName": name,
                "softwareAvailable": false,
                "softwareName": ""
            })
        }
        Err(error) => json!({
            "codec": codec,
            "mime": mime,
            "recommendedAvailable": false,
            "mimeAvailable": false,
            "recommendedName": "",
            "recommendedIsHardware": false,
            "hardwareAvailable": false,
            "hardwareName": "",
            "softwareAvailable": false,
            "softwareName": "",
            "error": format!("{error:?}")
        }),
    }
}

#[napi]
pub fn backend_summary() -> String {
    "RustDesk Enhanced / rd-engine / native Surface".to_owned()
}

#[napi]
pub fn build_marker() -> String {
    env!("BUILD_MARKER").to_owned()
}

#[napi]
pub fn healthcheck() -> String {
    action(true, "RustDesk Enhanced native runtime loaded")
}

#[napi]
pub fn native_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

#[napi]
pub fn runtime_init(app_dir: String, _device_name: String) -> Result<String> {
    let local_id = super::engine_initialize(app_dir)?;
    Ok(json!({
        "ok": true,
        "message": "RustDesk Enhanced runtime initialized",
        "localId": local_id
    })
    .to_string())
}

#[napi]
pub fn runtime_get_api_server() -> String {
    Config::get_option("api-server")
}

#[napi]
pub fn runtime_get_account_state() -> String {
    json!({
        "ok": true,
        "loggedIn": false,
        "apiServer": Config::get_option("api-server"),
        "user": null,
        "message": "Account integration is not implemented by RustDesk Enhanced"
    })
    .to_string()
}

#[napi]
pub fn runtime_get_decoder_capabilities() -> String {
    let h264 = decoder_row("H264", "video/avc", query_h264_hardware_decoder());
    let h265 = decoder_row(
        "H265",
        "video/hevc",
        query_hevc_hardware_capabilities().decoder,
    );
    json!({"ok": true, "capabilities": [h264, h265]}).to_string()
}

#[napi]
pub fn runtime_get_server_config() -> String {
    let id_server = Config::get_rendezvous_server();
    let relay_server = Config::get_option("relay-server");
    let api_server = Config::get_option("api-server");
    let key = Config::get_option("key");
    let custom = !Config::get_option("custom-rendezvous-server").is_empty();
    json!({
        "ok": true,
        "config": {
            "idServer": id_server,
            "relayServer": relay_server,
            "apiServer": api_server.clone(),
            "effectiveApiServer": api_server,
            "key": key,
            "mode": if custom { "custom" } else { "official" },
            "usingPublicServer": !custom,
            "remoteReachable": false,
            "accountAvailable": false,
            "resultState": "idle",
            "message": "",
            "idServerTestState": "untested",
            "relayServerTestState": "untested",
            "apiServerTestState": "untested"
        }
    })
    .to_string()
}

#[napi]
pub fn runtime_start_account_login_options() -> String {
    action(
        false,
        "Account login options are not implemented by RustDesk Enhanced",
    )
}

#[napi]
pub fn runtime_poll_account_login_options() -> String {
    json!({
        "ok": false,
        "state": "unsupported",
        "message": "Account login is not implemented by RustDesk Enhanced",
        "apiServer": Config::get_option("api-server"),
        "providers": []
    })
    .to_string()
}

#[napi]
pub fn runtime_list_recent_peers() -> String {
    json!({"ok": true, "peers": []}).to_string()
}

#[napi]
pub fn runtime_list_lan_peers() -> String {
    json!({"ok": true, "peers": []}).to_string()
}

#[napi]
pub fn runtime_list_favorites() -> String {
    json!({"ok": true, "favorites": []}).to_string()
}

#[napi]
pub fn runtime_list_address_book_peers() -> String {
    json!({"ok": true, "addressBooks": []}).to_string()
}

#[napi]
pub fn input_interceptor_start() -> String {
    action(false, UNSUPPORTED_MESSAGE)
}

#[napi]
pub fn input_interceptor_stop() -> String {
    action(true, "Input interceptor was not active")
}

#[napi]
pub fn input_interceptor_poll_events(_limit: f64) -> String {
    json!({"ok": false, "message": UNSUPPORTED_MESSAGE, "events": []}).to_string()
}

#[napi]
pub fn controlled_server_get_status() -> String {
    json!({
        "ok": true,
        "state": "stopped",
        "serverRunning": false,
        "myId": Config::get_id(),
        "temporaryPassword": "",
        "lastError": "",
        "message": "HarmonyOS host compatibility is not started"
    })
    .to_string()
}

#[napi]
pub fn controlled_screen_capture_get_status() -> String {
    json!({
        "ok": true,
        "state": "stopped",
        "active": false,
        "message": "Screen capture is not active"
    })
    .to_string()
}
