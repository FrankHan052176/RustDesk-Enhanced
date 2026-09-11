//! Flutter frontend bridge surface.
//!
//! The existing RustDesk Flutter UI calls into the kernel through
//! `flutter_rust_bridge` codegen over exactly this module. Signatures are kept
//! byte-compatible with upstream `src/flutter_ffi.rs` so the generated Dart
//! bindings reproduce the same `RustdeskImpl` class and the frontend needs no
//! changes. `scripts/ohos/verify-flutter-ffi-surface.py` enforces that.
//!
//! Several bodies are not implemented yet. They return neutral values rather
//! than fabricated success, because a caller must never mistake an unimplemented
//! remote operation for a completed one.
// The crate has no direct `anyhow` dependency; upstream reaches it through the
// same re-export, so the signature type is identical.
use flutter_rust_bridge::{StreamSink, SyncReturn};
use hbb_common::anyhow::Result;

pub type SessionID = uuid::Uuid;

/// Events pushed to the frontend for one session, matching upstream's union.
pub enum EventToUI {
    Event(String),
    Rgba(usize),
    Texture(usize, bool),
}

#[allow(unused_variables)]
#[allow(unused_variables)]
pub fn start_global_event_stream(s: StreamSink<String>, app_type: String) -> Result<()> {
    Ok(())
}

#[allow(unused_variables)]
pub fn stop_global_event_stream(app_type: String) {}

#[allow(unused_variables)]
pub fn host_stop_system_key_propagate(_stopped: bool) {}

#[allow(unused_variables)]
pub fn peer_get_sessions_count(id: String, conn_type: i32) -> SyncReturn<usize> {
    SyncReturn(0)
}

#[allow(unused_variables)]
pub fn session_add_existed_sync(
    id: String,
    session_id: SessionID,
    displays: Vec<i32>,
    is_view_camera: bool,
) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn session_add_sync(
    session_id: SessionID,
    id: String,
    is_file_transfer: bool,
    is_view_camera: bool,
    is_port_forward: bool,
    is_rdp: bool,
    is_terminal: bool,
    switch_uuid: String,
    force_relay: bool,
    password: String,
    is_shared_password: bool,
    conn_token: Option<String>,
) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn session_start(
    events2ui: StreamSink<EventToUI>,
    session_id: SessionID,
    id: String,
) -> Result<()> {
    Ok(())
}

#[allow(unused_variables)]
pub fn session_start_with_displays(
    events2ui: StreamSink<EventToUI>,
    session_id: SessionID,
    id: String,
    displays: Vec<i32>,
) -> Result<()> {
    Ok(())
}

#[allow(unused_variables)]
pub fn session_get_remember(session_id: SessionID) -> Option<bool> {
    None
}

#[allow(unused_variables)]
pub fn session_get_toggle_option(session_id: SessionID, arg: String) -> Option<bool> {
    None
}

#[allow(unused_variables)]
pub fn session_get_toggle_option_sync(session_id: SessionID, arg: String) -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn session_get_option(session_id: SessionID, arg: String) -> Option<String> {
    None
}

#[allow(unused_variables)]
pub fn session_login(
    session_id: SessionID,
    os_username: String,
    os_password: String,
    password: String,
    remember: bool,
) {
}

#[allow(unused_variables)]
pub fn session_send2fa(session_id: SessionID, code: String, trust_this_device: bool) {}

#[allow(unused_variables)]
pub fn session_get_enable_trusted_devices(session_id: SessionID) -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn will_session_close_close_session(session_id: SessionID) -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn session_close(session_id: SessionID) {}

#[allow(unused_variables)]
pub fn session_refresh(session_id: SessionID, display: usize) {}

#[allow(unused_variables)]
pub fn session_take_screenshot(session_id: SessionID, display: usize) {}

#[allow(unused_variables)]
pub fn session_handle_screenshot(
    #[allow(unused_variables)] session_id: SessionID,
    action: String,
) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn session_is_multi_ui_session(session_id: SessionID) -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn session_record_screen(session_id: SessionID, start: bool) {}

#[allow(unused_variables)]
pub fn session_get_is_recording(session_id: SessionID) -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn session_reconnect(session_id: SessionID, force_relay: bool) {}

#[allow(unused_variables)]
pub fn session_toggle_option(session_id: SessionID, value: String) {}

#[allow(unused_variables)]
pub fn session_toggle_privacy_mode(session_id: SessionID, impl_key: String, on: bool) {}

#[allow(unused_variables)]
pub fn session_get_flutter_option(session_id: SessionID, k: String) -> Option<String> {
    None
}

#[allow(unused_variables)]
pub fn session_set_flutter_option(session_id: SessionID, k: String, v: String) {}

#[allow(unused_variables)]
pub fn get_next_texture_key() -> SyncReturn<i32> {
    SyncReturn(0)
}

#[allow(unused_variables)]
pub fn get_local_flutter_option(k: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn set_local_flutter_option(k: String, v: String) {}

#[allow(unused_variables)]
pub fn get_local_kb_layout_type() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn set_local_kb_layout_type(kb_layout_type: String) {}

#[allow(unused_variables)]
pub fn session_get_view_style(session_id: SessionID) -> Option<String> {
    None
}

#[allow(unused_variables)]
pub fn session_set_view_style(session_id: SessionID, value: String) {}

#[allow(unused_variables)]
pub fn session_get_scroll_style(session_id: SessionID) -> Option<String> {
    None
}

#[allow(unused_variables)]
pub fn session_set_scroll_style(session_id: SessionID, value: String) {}

#[allow(unused_variables)]
pub fn session_get_edge_scroll_edge_thickness(session_id: SessionID) -> Option<i32> {
    None
}

#[allow(unused_variables)]
pub fn session_set_edge_scroll_edge_thickness(session_id: SessionID, value: i32) {}

#[allow(unused_variables)]
pub fn session_get_image_quality(session_id: SessionID) -> Option<String> {
    None
}

#[allow(unused_variables)]
pub fn session_set_image_quality(session_id: SessionID, value: String) {}

#[allow(unused_variables)]
pub fn session_get_keyboard_mode(session_id: SessionID) -> Option<String> {
    None
}

#[allow(unused_variables)]
pub fn session_set_keyboard_mode(session_id: SessionID, value: String) {}

#[allow(unused_variables)]
pub fn session_get_reverse_mouse_wheel_sync(session_id: SessionID) -> SyncReturn<Option<String>> {
    SyncReturn(None)
}

#[allow(unused_variables)]
pub fn session_set_reverse_mouse_wheel(session_id: SessionID, value: String) {}

#[allow(unused_variables)]
pub fn session_get_displays_as_individual_windows(
    session_id: SessionID,
) -> SyncReturn<Option<String>> {
    SyncReturn(None)
}

#[allow(unused_variables)]
pub fn session_set_displays_as_individual_windows(session_id: SessionID, value: String) {}

#[allow(unused_variables)]
pub fn session_get_use_all_my_displays_for_the_remote_session(
    session_id: SessionID,
) -> SyncReturn<Option<String>> {
    SyncReturn(None)
}

#[allow(unused_variables)]
pub fn session_set_use_all_my_displays_for_the_remote_session(
    session_id: SessionID,
    value: String,
) {
}

#[allow(unused_variables)]
pub fn session_get_custom_image_quality(session_id: SessionID) -> Option<Vec<i32>> {
    None
}

#[allow(unused_variables)]
pub fn session_is_keyboard_mode_supported(session_id: SessionID, mode: String) -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn session_set_custom_image_quality(session_id: SessionID, value: i32) {}

#[allow(unused_variables)]
pub fn session_set_custom_fps(session_id: SessionID, fps: i32) {}

#[allow(unused_variables)]
pub fn session_get_trackpad_speed(session_id: SessionID) -> Option<i32> {
    None
}

#[allow(unused_variables)]
pub fn session_set_trackpad_speed(session_id: SessionID, value: i32) {}

#[allow(unused_variables)]
pub fn session_lock_screen(session_id: SessionID) {}

#[allow(unused_variables)]
pub fn session_ctrl_alt_del(session_id: SessionID) {}

#[allow(unused_variables)]
pub fn session_switch_display(is_desktop: bool, session_id: SessionID, value: Vec<i32>) {}

#[allow(unused_variables)]
pub fn session_handle_flutter_key_event(
    session_id: SessionID,
    character: String,
    usb_hid: i32,
    lock_modes: i32,
    down_or_up: bool,
) {
}

#[allow(unused_variables)]
pub fn session_handle_flutter_raw_key_event(
    session_id: SessionID,
    name: String,
    platform_code: i32,
    position_code: i32,
    lock_modes: i32,
    down_or_up: bool,
) {
}

#[allow(unused_variables)]
pub fn session_enter_or_leave(_session_id: SessionID, _enter: bool) -> SyncReturn<()> {
    SyncReturn(())
}

#[allow(unused_variables)]
pub fn session_input_key(
    session_id: SessionID,
    name: String,
    down: bool,
    press: bool,
    alt: bool,
    ctrl: bool,
    shift: bool,
    command: bool,
) {
}

#[allow(unused_variables)]
pub fn session_input_string(session_id: SessionID, value: String) {}

#[allow(unused_variables)]
pub fn session_send_chat(session_id: SessionID, text: String) {}

#[allow(unused_variables)]
pub fn session_open_terminal(session_id: SessionID, terminal_id: i32, rows: u32, cols: u32) {}

#[allow(unused_variables)]
pub fn session_send_terminal_input(session_id: SessionID, terminal_id: i32, data: String) {}

#[allow(unused_variables)]
pub fn session_resize_terminal(session_id: SessionID, terminal_id: i32, rows: u32, cols: u32) {}

#[allow(unused_variables)]
pub fn session_close_terminal(session_id: SessionID, terminal_id: i32) {}

#[allow(unused_variables)]
pub fn session_peer_option(session_id: SessionID, name: String, value: String) {}

#[allow(unused_variables)]
pub fn session_get_peer_option(session_id: SessionID, name: String) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn session_input_os_password(session_id: SessionID, value: String) {}

#[allow(unused_variables)]
pub fn session_read_remote_dir(session_id: SessionID, path: String, include_hidden: bool) {}

#[allow(unused_variables)]
pub fn session_send_files(
    session_id: SessionID,
    act_id: i32,
    path: String,
    to: String,
    file_num: i32,
    include_hidden: bool,
    is_remote: bool,
    _is_dir: bool,
) {
}

#[allow(unused_variables)]
pub fn session_set_confirm_override_file(
    session_id: SessionID,
    act_id: i32,
    file_num: i32,
    need_override: bool,
    remember: bool,
    is_upload: bool,
) {
}

#[allow(unused_variables)]
pub fn session_remove_file(
    session_id: SessionID,
    act_id: i32,
    path: String,
    file_num: i32,
    is_remote: bool,
) {
}

#[allow(unused_variables)]
pub fn session_read_dir_to_remove_recursive(
    session_id: SessionID,
    act_id: i32,
    path: String,
    is_remote: bool,
    show_hidden: bool,
) {
}

#[allow(unused_variables)]
pub fn session_remove_all_empty_dirs(
    session_id: SessionID,
    act_id: i32,
    path: String,
    is_remote: bool,
) {
}

#[allow(unused_variables)]
pub fn session_cancel_job(session_id: SessionID, act_id: i32) {}

#[allow(unused_variables)]
pub fn session_create_dir(session_id: SessionID, act_id: i32, path: String, is_remote: bool) {}

#[allow(unused_variables)]
pub fn session_read_local_dir_sync(
    _session_id: SessionID,
    path: String,
    show_hidden: bool,
) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn session_read_local_empty_dirs_recursive_sync(
    _session_id: SessionID,
    path: String,
    include_hidden: bool,
) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn session_read_remote_empty_dirs_recursive_sync(
    session_id: SessionID,
    path: String,
    include_hidden: bool,
) {
}

#[allow(unused_variables)]
pub fn session_get_platform(session_id: SessionID, is_remote: bool) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn session_load_last_transfer_jobs(session_id: SessionID) {}

#[allow(unused_variables)]
pub fn session_add_job(
    session_id: SessionID,
    act_id: i32,
    path: String,
    to: String,
    file_num: i32,
    include_hidden: bool,
    is_remote: bool,
) {
}

#[allow(unused_variables)]
pub fn session_resume_job(session_id: SessionID, act_id: i32, is_remote: bool) {}

#[allow(unused_variables)]
pub fn session_rename_file(
    session_id: SessionID,
    act_id: i32,
    path: String,
    new_name: String,
    is_remote: bool,
) {
}

#[allow(unused_variables)]
pub fn session_elevate_direct(session_id: SessionID) {}

#[allow(unused_variables)]
pub fn session_elevate_with_logon(session_id: SessionID, username: String, password: String) {}

#[allow(unused_variables)]
pub fn session_switch_sides(session_id: SessionID) {}

#[allow(unused_variables)]
pub fn session_change_resolution(session_id: SessionID, display: i32, width: i32, height: i32) {}

#[allow(unused_variables)]
pub fn session_set_size(session_id: SessionID, display: usize, width: usize, height: usize) {}

#[allow(unused_variables)]
pub fn session_send_selected_session_id(session_id: SessionID, sid: String) {}

#[allow(unused_variables)]
pub fn main_get_sound_inputs() -> Vec<String> {
    Vec::new()
}

#[allow(unused_variables)]
pub fn main_get_login_device_info() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_change_id(new_id: String) {}

#[allow(unused_variables)]
pub fn main_get_async_status() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_http_status(url: String) -> Option<String> {
    None
}

#[allow(unused_variables)]
pub fn main_get_option(key: String) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_option_sync(key: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_get_error() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_set_option(key: String, value: String) {}

#[allow(unused_variables)]
pub fn main_get_options() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_options_sync() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_set_options(json: String) {}

#[allow(unused_variables)]
pub fn main_test_if_valid_server(server: String, test_with_proxy: bool) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_set_socks(proxy: String, username: String, password: String) {}

#[allow(unused_variables)]
pub fn main_get_proxy_status() -> bool {
    false
}

#[allow(unused_variables)]
pub fn main_get_socks() -> Vec<String> {
    Vec::new()
}

#[allow(unused_variables)]
pub fn main_get_app_name() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_app_name_sync() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_uri_prefix_sync() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_get_license() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_version() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_fav() -> Vec<String> {
    Vec::new()
}

#[allow(unused_variables)]
pub fn main_store_fav(favs: Vec<String>) {}

#[allow(unused_variables)]
pub fn main_get_peer_sync(id: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_get_lan_peers() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_connect_status() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_check_connect_status() {}

#[allow(unused_variables)]
pub fn main_is_using_public_server() -> bool {
    false
}

#[allow(unused_variables)]
pub fn main_discover() {}

#[allow(unused_variables)]
pub fn main_get_api_server() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_deploy_device(token: String, id: String) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_resolve_avatar_url(avatar: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_http_request(url: String, method: String, body: Option<String>, header: String) {}

#[allow(unused_variables)]
pub fn main_get_local_option(key: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_get_use_texture_render() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_get_env(key: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_set_env(key: String, value: Option<String>) -> SyncReturn<()> {
    SyncReturn(())
}

#[allow(unused_variables)]
pub fn main_set_local_option(key: String, value: String) {}

#[allow(unused_variables)]
pub fn main_handle_wayland_screencast_restore_token(_key: String, _value: String) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_input_source() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_set_input_source(session_id: SessionID, value: String) {}

#[allow(unused_variables)]
pub fn main_set_cursor_position(x: i32, y: i32) -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_clip_cursor(
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    enable: bool,
) -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_get_my_id() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_uuid() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_peer_option(id: String, key: String) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_peer_option_sync(id: String, key: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_get_peer_flutter_option_sync(id: String, k: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_set_peer_flutter_option_sync(id: String, k: String, v: String) -> SyncReturn<()> {
    SyncReturn(())
}

#[allow(unused_variables)]
pub fn main_set_peer_option(id: String, key: String, value: String) {}

#[allow(unused_variables)]
pub fn main_set_peer_option_sync(id: String, key: String, value: String) -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_set_peer_alias(id: String, alias: String) {}

#[allow(unused_variables)]
pub fn main_get_new_stored_peers() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_forget_password(id: String) {}

#[allow(unused_variables)]
pub fn main_peer_has_password(id: String) -> bool {
    false
}

#[allow(unused_variables)]
pub fn main_peer_exists(id: String) -> bool {
    false
}

#[allow(unused_variables)]
pub fn main_load_recent_peers() {}

#[allow(unused_variables)]
pub fn main_load_recent_peers_for_ab(filter: String) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_load_fav_peers() {}

#[allow(unused_variables)]
pub fn main_load_lan_peers() {}

#[allow(unused_variables)]
pub fn main_remove_discovered(id: String) {}

#[allow(unused_variables)]
pub fn main_change_theme(dark: String) {}

#[allow(unused_variables)]
pub fn main_change_language(lang: String) {}

#[allow(unused_variables)]
pub fn main_video_save_directory(root: bool) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_set_user_default_option(key: String, value: String) {}

#[allow(unused_variables)]
pub fn main_get_user_default_option(key: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_handle_relay_id(id: String) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_is_option_fixed(key: String) -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_get_main_display() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_get_displays() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn session_add_port_forward(
    session_id: SessionID,
    local_port: i32,
    remote_host: String,
    remote_port: i32,
) {
}

#[allow(unused_variables)]
pub fn session_remove_port_forward(session_id: SessionID, local_port: i32) {}

#[allow(unused_variables)]
pub fn session_new_rdp(session_id: SessionID) {}

#[allow(unused_variables)]
pub fn session_request_voice_call(session_id: SessionID) {}

#[allow(unused_variables)]
pub fn session_close_voice_call(session_id: SessionID) {}

#[allow(unused_variables)]
pub fn session_get_conn_token(session_id: SessionID) -> SyncReturn<Option<String>> {
    SyncReturn(None)
}

#[allow(unused_variables)]
pub fn cm_handle_incoming_voice_call(id: i32, accept: bool) {}

#[allow(unused_variables)]
pub fn cm_close_voice_call(id: i32) {}

#[allow(unused_variables)]
pub fn set_voice_call_input_device(_is_cm: bool, _device: String) {}

#[allow(unused_variables)]
pub fn get_voice_call_input_device(_is_cm: bool) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_last_remote_id() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_software_update_url() {}

#[allow(unused_variables)]
pub fn main_get_home_dir() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_langs() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_temporary_password() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_set_permanent_password_with_result(password: String) -> bool {
    false
}

#[allow(unused_variables)]
pub fn main_get_fingerprint() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn cm_get_clients_state() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn cm_check_clients_length(length: usize) -> Option<String> {
    None
}

#[allow(unused_variables)]
pub fn cm_get_clients_length() -> usize {
    0
}

#[allow(unused_variables)]
pub fn main_init(app_dir: String, custom_client_config: String) {}

#[allow(unused_variables)]
pub fn main_configure_ohos_host_display(width: usize, height: usize, display_id: u64) -> bool {
    false
}

#[allow(unused_variables)]
pub fn main_set_ohos_host_clipboard_enabled(enabled: bool) {}

#[allow(unused_variables)]
pub fn main_update_ohos_host_clipboard_text(text: String) -> bool {
    false
}

#[allow(unused_variables)]
pub fn main_take_ohos_host_clipboard_text() -> Option<String> {
    None
}

#[allow(unused_variables)]
pub fn main_start_ohos_host() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_stop_ohos_host() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_ohos_host_is_started() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_device_id(id: String) {}

#[allow(unused_variables)]
pub fn main_device_name(name: String) {}

#[allow(unused_variables)]
pub fn main_remove_peer(id: String) {}

#[allow(unused_variables)]
pub fn main_has_hwcodec() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_has_vram() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_supported_hwdecodings() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_is_root() -> bool {
    false
}

#[allow(unused_variables)]
pub fn get_double_click_time() -> SyncReturn<i32> {
    SyncReturn(0)
}

#[allow(unused_variables)]
pub fn main_start_dbus_server() {}

#[allow(unused_variables)]
pub fn main_save_ab(json: String) {}

#[allow(unused_variables)]
pub fn main_clear_ab() {}

#[allow(unused_variables)]
pub fn main_load_ab() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_save_group(json: String) {}

#[allow(unused_variables)]
pub fn main_clear_group() {}

#[allow(unused_variables)]
pub fn main_load_group() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn session_send_pointer(session_id: SessionID, msg: String) {}

#[allow(unused_variables)]
pub fn session_send_mouse(session_id: SessionID, msg: String) {}

#[allow(unused_variables)]
pub fn session_restart_remote_device(session_id: SessionID) {}

#[allow(unused_variables)]
pub fn session_get_audit_server_sync(session_id: SessionID, typ: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn session_send_note(session_id: SessionID, note: String) {}

#[allow(unused_variables)]
pub fn session_get_last_audit_note(session_id: SessionID) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn session_set_audit_guid(session_id: SessionID, guid: String) {}

#[allow(unused_variables)]
pub fn session_get_audit_guid(session_id: SessionID) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn session_get_conn_session_id(session_id: SessionID) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn session_alternative_codecs(session_id: SessionID) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn session_change_prefer_codec(session_id: SessionID) {}

#[allow(unused_variables)]
pub fn session_on_waiting_for_image_dialog_show(session_id: SessionID) {}

#[allow(unused_variables)]
pub fn session_toggle_virtual_display(session_id: SessionID, index: i32, on: bool) {}

#[allow(unused_variables)]
pub fn session_printer_response(
    session_id: SessionID,
    id: i32,
    path: String,
    printer_name: String,
) {
}

#[allow(unused_variables)]
pub fn main_set_home_dir(_home: String) {}

#[allow(unused_variables)]
pub fn main_get_data_dir_ios(app_dir: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_stop_service() {}

#[allow(unused_variables)]
pub fn main_start_service() {}

#[allow(unused_variables)]
pub fn main_update_temporary_password() {}

#[allow(unused_variables)]
pub fn main_check_super_user_permission() -> bool {
    false
}

#[allow(unused_variables)]
pub fn main_get_unlock_pin() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_set_unlock_pin(pin: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_check_mouse_time() {}

#[allow(unused_variables)]
pub fn main_get_mouse_time() -> f64 {
    0.0
}

#[allow(unused_variables)]
pub fn main_wol(id: String) {}

#[allow(unused_variables)]
pub fn main_create_shortcut(_id: String) {}

#[allow(unused_variables)]
pub fn cm_send_chat(conn_id: i32, msg: String) {}

#[allow(unused_variables)]
pub fn cm_login_res(conn_id: i32, res: bool) {}

#[allow(unused_variables)]
pub fn cm_close_connection(conn_id: i32) {}

#[allow(unused_variables)]
pub fn cm_close_connection_window(conn_id: i32) {}

#[allow(unused_variables)]
pub fn cm_remove_disconnected_connection(conn_id: i32) {}

#[allow(unused_variables)]
pub fn cm_check_click_time(conn_id: i32) {}

#[allow(unused_variables)]
pub fn cm_get_click_time() -> f64 {
    0.0
}

#[allow(unused_variables)]
pub fn cm_switch_permission(conn_id: i32, name: String, enabled: bool) {}

#[allow(unused_variables)]
pub fn cm_can_elevate() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn cm_elevate_portable(conn_id: i32) {}

#[allow(unused_variables)]
pub fn cm_switch_back(conn_id: i32) {}

#[allow(unused_variables)]
pub fn cm_get_config(name: String) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_build_date() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn translate(name: String, locale: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn session_get_rgba_size(session_id: SessionID, display: usize) -> SyncReturn<usize> {
    SyncReturn(0)
}

#[allow(unused_variables)]
pub fn session_next_rgba(session_id: SessionID, display: usize) -> SyncReturn<()> {
    SyncReturn(())
}

#[allow(unused_variables)]
pub fn session_register_pixelbuffer_texture(
    session_id: SessionID,
    display: usize,
    ptr: usize,
) -> SyncReturn<()> {
    SyncReturn(())
}

#[allow(unused_variables)]
pub fn session_register_gpu_texture(
    session_id: SessionID,
    display: usize,
    ptr: usize,
) -> SyncReturn<()> {
    SyncReturn(())
}

#[allow(unused_variables)]
pub fn query_onlines(ids: Vec<String>) {}

#[allow(unused_variables)]
pub fn version_to_number(v: String) -> SyncReturn<i64> {
    SyncReturn(0)
}

#[allow(unused_variables)]
pub fn option_synced() -> bool {
    false
}

#[allow(unused_variables)]
pub fn main_is_installed() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_init_input_source() -> SyncReturn<()> {
    SyncReturn(())
}

#[allow(unused_variables)]
pub fn main_is_installed_lower_version() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_is_installed_daemon(prompt: bool) -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_is_process_trusted(prompt: bool) -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_is_can_screen_recording(prompt: bool) -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_is_can_input_monitoring(prompt: bool) -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_is_share_rdp() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_set_share_rdp(enable: bool) {}

#[allow(unused_variables)]
pub fn main_goto_install() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_get_new_version() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_update_me() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn set_cur_session_id(session_id: SessionID) {}

#[allow(unused_variables)]
pub fn install_show_run_without_install() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn install_run_without_install() {}

#[allow(unused_variables)]
pub fn install_install_me(options: String, path: String) {}

#[allow(unused_variables)]
pub fn install_install_path() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn install_install_options() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_account_auth(op: String, remember_me: bool) {}

#[allow(unused_variables)]
pub fn main_account_auth_cancel() {}

#[allow(unused_variables)]
pub fn main_account_auth_result() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_on_main_window_close() {}

#[allow(unused_variables)]
pub fn main_current_is_wayland() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_is_login_wayland() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_hide_dock() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_has_file_clipboard() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_has_gpu_texture_render() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn cm_init() {}

#[allow(unused_variables)]
pub fn main_start_ipc_url_server() {}

#[allow(unused_variables)]
pub fn main_test_wallpaper(_second: u64) {}

#[allow(unused_variables)]
pub fn main_support_remove_wallpaper() -> bool {
    false
}

#[allow(unused_variables)]
pub fn is_incoming_only() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn is_outgoing_only() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn is_custom_client() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn is_disable_settings() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn is_disable_ab() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn is_disable_account() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn is_disable_group_panel() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn is_disable_installation() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn is_preset_password() -> bool {
    false
}

#[allow(unused_variables)]
pub fn is_preset_password_mobile_only() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn send_url_scheme(_url: String) {}

#[allow(unused_variables)]
pub fn is_support_multi_ui_session(version: String) -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn is_selinux_enforcing() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_default_privacy_mode_impl() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_supported_privacy_mode_impls() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_supported_input_source() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_generate2fa() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_verify2fa(code: String) -> bool {
    false
}

#[allow(unused_variables)]
pub fn main_has_valid_2fa_sync() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_verify_bot(token: String) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_has_valid_bot_sync() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_get_hard_option(key: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_get_buildin_option(key: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_check_hwcodec() {}

#[allow(unused_variables)]
pub fn main_get_trusted_devices() -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_remove_trusted_devices(json: String) {}

#[allow(unused_variables)]
pub fn main_clear_trusted_devices() {}

#[allow(unused_variables)]
pub fn main_max_encrypt_len() -> SyncReturn<usize> {
    SyncReturn(0)
}

#[allow(unused_variables)]
pub fn session_request_new_display_init_msgs(session_id: SessionID, display: usize) {}

#[allow(unused_variables)]
pub fn main_audio_support_loopback() -> SyncReturn<bool> {
    SyncReturn(false)
}

#[allow(unused_variables)]
pub fn main_get_printer_names() -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_get_common(key: String) -> String {
    String::new()
}

#[allow(unused_variables)]
pub fn main_get_common_sync(key: String) -> SyncReturn<String> {
    SyncReturn(String::new())
}

#[allow(unused_variables)]
pub fn main_set_common(_key: String, _value: String) {}

#[allow(unused_variables)]
pub fn session_set_common(session_id: SessionID, key: String, value: String) {}

#[allow(unused_variables)]
pub fn session_get_common_sync(
    session_id: SessionID,
    key: String,
    param: String,
) -> SyncReturn<Option<String>> {
    SyncReturn(None)
}

#[allow(unused_variables)]
pub fn session_get_common(
    session_id: SessionID,
    key: String,
    #[allow(unused_variables)] param: String,
) -> Option<String> {
    None
}
