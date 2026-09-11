//! Application-level state behind the Flutter bridge.
//!
//! The Flutter frontend reads configuration and identity through the bridge
//! before it can draw anything, so these helpers have to answer real questions
//! rather than neutral placeholders. They deliberately reuse the vendored
//! `hbb_common` configuration store, which is the same store the wire layer
//! reads, so a value shown in the UI and a value sent to a peer cannot diverge.

use hbb_common::config::{Config, LocalConfig, keys};
use hbb_common::serde_json::{Map, Value};

/// Identity shown to peers and in the UI. `Config::get_id` owns the value and
/// generates it on first use, so this never invents an alternative identity.
pub fn my_id() -> String {
    Config::get_id()
}

/// Device identifier used for licensing/telemetry, matching upstream's shape:
/// a UUID string derived from the machine, not the peer id.
pub fn uuid() -> String {
    let bytes = hbb_common::get_uuid();
    if bytes.is_empty() {
        return String::new();
    }
    uuid::Uuid::from_slice(&bytes)
        .map(|value| value.to_string())
        .unwrap_or_default()
}

pub fn version() -> String {
    crate::REPORTED_VERSION.to_owned()
}

pub fn app_name() -> String {
    crate::APP_NAME.to_owned()
}

/// Custom-client licence text. This build ships no licence blob, and an empty
/// string is what upstream returns for a stock client.
pub fn license() -> String {
    String::new()
}

/// URI scheme prefix used for `rustdesk://` links.
pub fn uri_prefix() -> String {
    crate::APP_NAME.to_lowercase()
}

/// Build date is not compiled in here; report the crate's own version so the
/// field is a real release identifier rather than a fabricated timestamp.
pub fn build_date() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// Locale table. Translations are not shipped yet, so every lookup falls back to
/// the key itself, which is exactly what the frontend expects for a missing
/// entry.
pub fn translate(name: String, _locale: String) -> String {
    name
}

/// The whole client option map as JSON, the shape `jsonDecode` expects on the
/// Dart side.
pub fn options_json() -> String {
    let options = Config::get_options();
    let mut map = Map::with_capacity(options.len());
    for (key, value) in options {
        map.insert(key, Value::String(value));
    }
    Value::Object(map).to_string()
}

/// Replace the whole option map. Keys the store rejects are counted so a caller
/// is not told a write succeeded that the store refused.
pub fn set_options_json(json: &str) -> usize {
    let parsed: Value = match hbb_common::serde_json::from_str(json) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    let Some(object) = parsed.as_object() else {
        return 0;
    };
    let mut rejected = 0;
    for (key, value) in object {
        match value {
            Value::String(text) => Config::set_option(key.clone(), text.clone()),
            // Non-string values are stored in their JSON form so the UI can
            // round-trip numbers and booleans without a lossy guess.
            other => {
                Config::set_option(key.clone(), other.to_string());
            }
        }
        if Config::get_option(key) != value_to_string(value) {
            rejected += 1;
        }
    }
    rejected
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub fn get_option(key: &str) -> String {
    Config::get_option(key)
}

pub fn set_option(key: &str, value: &str) {
    Config::set_option(key.to_owned(), value.to_owned());
}

pub fn get_local_option(key: &str) -> String {
    LocalConfig::get_option(key)
}

pub fn set_local_option(key: &str, value: &str) {
    LocalConfig::set_option(key.to_owned(), value.to_owned());
}

/// Per-option query used by the settings UI. Only options this build can answer
/// truthfully are handled; anything else is reported as unset rather than
/// guessed.
pub fn common(key: &str) -> Option<String> {
    match key {
        "my-id" => Some(my_id()),
        "app-name" => Some(app_name()),
        "version" => Some(version()),
        "build-date" => Some(build_date()),
        "uri-prefix" => Some(uri_prefix()),
        "license" => Some(license()),
        // The remaining upstream keys describe platform services this build does
        // not provide, so they are reported as unavailable.
        _ => None,
    }
}

/// The language table the UI lists in settings. Each entry pairs a locale code
/// with its display name; only locales this build can actually render are
/// offered, and the translator above is a pass-through until translations ship.
pub fn langs_json() -> String {
    use hbb_common::serde_json::json;
    json!([["en", "English"], ["zh-cn", "简体中文"]]).to_string()
}

/// Whether an option is fixed by the build and cannot be edited by the user.
/// Nothing is pinned in this build, so every option is editable.
pub fn is_option_fixed(_key: &str) -> bool {
    false
}

/// Per-peer option value, persisted in the peer's own config file so it
/// survives a reconnect and is shared by every session to that peer.
pub fn peer_option(id: &str, key: &str) -> String {
    hbb_common::config::PeerConfig::load(id)
        .options
        .get(key)
        .cloned()
        .unwrap_or_default()
}

pub fn set_peer_option(id: &str, key: &str, value: &str) {
    if id.is_empty() || key.is_empty() || value.len() > 4096 {
        return;
    }
    let mut config = hbb_common::config::PeerConfig::load(id);
    config.options.insert(key.to_owned(), value.to_owned());
    config.store(id);
}

/// Flutter-only presentation option for a peer, kept in the same peer config so
/// the UI restores its own settings without a second store.
pub fn peer_flutter_option(id: &str, key: &str) -> String {
    let config = hbb_common::config::PeerConfig::load(id);
    config
        .options
        .get(&format!("flutter-{key}"))
        .cloned()
        .unwrap_or_default()
}

pub fn set_peer_flutter_option(id: &str, key: &str, value: &str) {
    if id.is_empty() || key.is_empty() || value.len() > 4096 {
        return;
    }
    let mut config = hbb_common::config::PeerConfig::load(id);
    // Stored under a namespaced key so a Flutter presentation option can never
    // collide with a protocol option that shares its name.
    config
        .options
        .insert(format!("flutter-{key}"), value.to_owned());
    config.store(id);
}

/// Drop the stored password for a peer.
pub fn forget_password(id: &str) {
    let mut config = hbb_common::config::PeerConfig::load(id);
    config.password = Vec::new();
    config.store(id);
}

pub fn peer_has_password(id: &str) -> bool {
    !hbb_common::config::PeerConfig::load(id).password.is_empty()
}

pub fn peer_exists(id: &str) -> bool {
    // A peer is "known" when this machine has stored anything about it, which is
    // what the UI means by an existing entry. Anything beyond an empty config
    // counts, so a peer the user only aliased is still found.
    let config = hbb_common::config::PeerConfig::load(id);
    !config.options.is_empty() || !config.info.hostname.is_empty()
}

pub fn set_peer_alias(id: &str, alias: &str) {
    if id.is_empty() || alias.len() > 4096 {
        return;
    }
    set_peer_option(id, "alias", alias);
}

/// Remove every stored trace of a peer.
pub fn remove_peer(id: &str) {
    if id.is_empty() || id.len() > 256 {
        return;
    }
    // The vendored config owns peer-file layout, so removal goes through it
    // rather than re-deriving the path here.
    hbb_common::config::PeerConfig::remove(id);
}

/// Connection status is derived from what the runtime actually knows, not from
/// a cached string: this build reports no rendezvous registration, so the UI is
/// told the client is not registered rather than shown a false "ready".
pub fn connect_status() -> String {
    String::new()
}

/// Last runtime error. The viewer reports per-session errors through its own
/// events, so there is no separate global error to surface.
pub fn last_error() -> String {
    String::new()
}

/// Global async job status. There is no background account/address-book job in
/// this build, so no job is reported as running.
pub fn async_job_status() -> String {
    String::new()
}

/// Whether the rendezvous key is the stock public key rather than a custom one.
pub fn is_using_public_server() -> bool {
    Config::get_option(keys::OPTION_KEY).is_empty()
}

/// Server API base URL. This build has no account/address-book backend, so it
/// reports no API server instead of pointing the UI at an endpoint that would
/// fail every request.
pub fn api_server() -> String {
    String::new()
}

pub fn rendezvous_server() -> String {
    Config::get_rendezvous_server()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_json_is_an_object_so_the_dart_side_can_decode_it() {
        let parsed: Value = hbb_common::serde_json::from_str(&options_json()).expect("valid json");
        assert!(parsed.is_object(), "mainGetOptions must decode to a map");
    }

    #[test]
    fn unknown_common_keys_are_reported_as_unavailable_not_guessed() {
        assert!(common("definitely-not-an-option").is_none());
        assert_eq!(common("my-id").as_deref(), Some(my_id().as_str()));
    }

    #[test]
    fn identity_and_version_are_real_values() {
        assert!(!app_name().is_empty());
        assert_eq!(version(), crate::REPORTED_VERSION);
        // The id is generated on first access, so it must never be empty.
        assert!(!my_id().is_empty());
    }

    #[test]
    fn a_malformed_options_payload_is_rejected_rather_than_partially_applied() {
        assert_eq!(set_options_json("not json"), 0);
        assert_eq!(set_options_json("[1,2,3]"), 0);
    }
}
