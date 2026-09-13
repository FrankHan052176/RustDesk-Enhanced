//! Frame-rate expectation for the video Surface.
//!
//! A Surface producer draws on its own, outside ArkUI's animation and drawing
//! paths, so an ArkTS-only request does not reach the panel: an ArkTS
//! `displaySync` range was measured leaving this device at its base rate with
//! `expectedRefreshRate` still -1 while the decoder produced about 119 frames per
//! second.
//!
//! The ArkUI node route is closed on this platform: nothing an ArkTS
//! `getFrameNodeById` returns is accepted as an XComponent instance (every range
//! was refused with `ARKUI_ERROR_CODE_PARAM_INVALID`, including the documented
//! sample's), and a lookup by the component's own `id` parameter resolves nothing
//! at all, because that parameter belongs to the native XComponent. So the native
//! XComponent itself is adopted instead, through the object the framework hands
//! the module in `onLoad`, and the API that takes it dates from API 11.
use napi_derive_ohos::napi;
use napi_ohos::bindgen_prelude::Object;
use napi_ohos::{Env, JsValue};
use std::sync::atomic::{AtomicPtr, Ordering};

/// The native XComponent the ArkTS side handed over, or null before that.
static COMPONENT: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Mirrors `OH_NativeXComponent_ExpectedRateRange`; passed by pointer here.
#[repr(C)]
struct ExpectedRateRange {
    /// Lowest rate the system may fall back to. Zero lets it drop for power.
    min: i32,
    /// Highest rate this content can use.
    max: i32,
    /// The rate the system should aim for.
    expected: i32,
}

#[link(name = "ace_ndk.z")]
unsafe extern "C" {
    fn OH_NativeXComponent_SetExpectedFrameRateRange(
        component: *mut std::ffi::c_void,
        range: *const ExpectedRateRange,
    ) -> i32;
}

/// Adopts the native XComponent that arrives with the Surface's `onLoad`.
///
/// The framework publishes its native peer as a named property of the object it
/// passes to `onLoad`, and that property is a wrapped pointer, so it is unwrapped
/// rather than read as a value. Returns `0` once the component is held, the NAPI
/// status when the handshake fails, and `-1` when the property is absent.
#[napi]
pub fn xcomponent_attach(env: Env, context: Object) -> i32 {
    const NATIVE_OBJECT: &[u8] = b"__NATIVE_XCOMPONENT_OBJ__\0";
    let mut property: napi_ohos::sys::napi_value = std::ptr::null_mut();
    let status = unsafe {
        napi_ohos::sys::napi_get_named_property(
            env.raw(),
            context.raw(),
            NATIVE_OBJECT.as_ptr().cast(),
            &mut property,
        )
    };
    if status != 0 || property.is_null() {
        return if status == 0 { -1 } else { status };
    }
    let mut component: *mut std::ffi::c_void = std::ptr::null_mut();
    let unwrapped = unsafe { napi_ohos::sys::napi_unwrap(env.raw(), property, &mut component) };
    if unwrapped != 0 || component.is_null() {
        return if unwrapped == 0 { -1 } else { unwrapped };
    }
    COMPONENT.store(component, Ordering::Release);
    0
}

/// Declares the rate the remote picture is being produced at.
///
/// Returns the platform's result code, so a refusal stays visible instead of
/// being mistaken for a raised panel. `-1` means the arguments were rejected
/// before the platform saw them, `-3` that no component has been adopted yet.
#[napi]
pub fn xcomponent_set_frame_rate_range_native(expected: i32, min: i32, max: i32) -> i32 {
    // The platform's own bounds for this value; anything else is a caller bug.
    if !(1..=240).contains(&expected) || min < 0 || min > expected || max < expected || max > 240 {
        return -1;
    }
    let component = COMPONENT.load(Ordering::Acquire);
    if component.is_null() {
        return -3;
    }
    let range = ExpectedRateRange {
        min,
        max,
        expected,
    };
    unsafe { OH_NativeXComponent_SetExpectedFrameRateRange(component, &range) }
}
