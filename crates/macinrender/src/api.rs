use std::ffi::{c_char, c_void};
#[cfg(native_macinrender)]
use std::sync::OnceLock;

use crate::raw;

macro_rules! api {
    ($($name:ident ($($arg:ty),*) -> $result:ty;)*) => {
        #[derive(Clone)]
        pub struct Api {
            $(pub $name: unsafe extern "C" fn($($arg),*) -> $result,)*
        }
        #[cfg(native_macinrender)]
        unsafe extern "C" {
            $(fn $name($(_: $arg),*) -> $result;)*
        }
        impl Api {
            pub fn load() -> Result<Self, String> {
                #[cfg(not(native_macinrender))]
                return Err("MacinRender is available on macOS and Windows".into());
                #[cfg(native_macinrender)]
                {
                    // The statically linked native cache lives for the process lifetime.
                    static CACHED: OnceLock<Api> = OnceLock::new();
                    if let Some(api) = CACHED.get() {
                        return Ok(api.clone());
                }
                let api = Self { $($name,)* };
                // SAFETY: validated version entrypoints take no pointers.
                if unsafe { (api.adm_api_version_major)() } != 1 || unsafe { (api.adm_api_version_minor)() } < 36 {
                    return Err("MacinRender C ABI v1.36 or later is required".into());
                }
                let _ = CACHED.set(api.clone());
                Ok(api)
                }
            }
        }
    };
}

api! {
    adm_api_version_major() -> i32;
    adm_api_version_minor() -> i32;
    adm_create_context() -> *mut c_void;
    adm_destroy_context(*mut c_void) -> ();
    adm_context_last_error_message(*const c_void) -> *const c_char;
    adm_create_scene_stream(*mut c_void, *const raw::StreamConfig, *mut *mut c_void) -> i32;
    adm_destroy_scene_stream(*mut c_void) -> ();
    adm_scene_stream_last_error_message(*const c_void) -> *const c_char;
    adm_scene_stream_configure_generation(*mut c_void, u64, u64, *const raw::Element, u32) -> i32;
    adm_scene_stream_submit_frame(*mut c_void, *const raw::Frame, u32, *mut i32) -> i32;
    adm_scene_stream_signal_end(*mut c_void, u64, i64) -> i32;
    adm_scene_stream_switch_backend_ex(*const c_void, *const raw::RendererConfig, *mut *mut c_char) -> i32;
    adm_scene_stream_set_listener_orientation(*mut c_void, f32, f32, f32) -> i32;
    adm_create_scene_output(*mut c_void, *const c_void, *const raw::OutputConfig, *mut *mut c_void) -> i32;
    adm_destroy_scene_output(*mut c_void) -> ();
    adm_scene_output_last_error_message(*const c_void) -> *const c_char;
    adm_scene_output_begin_epoch(*mut c_void, u64, i64) -> i32;
    adm_scene_output_play(*mut c_void) -> i32;
    adm_scene_output_pause(*mut c_void) -> i32;
    adm_scene_output_set_volume(*mut c_void, f32) -> i32;
    adm_scene_output_get_status(*mut c_void, *mut raw::OutputStatus) -> i32;
    adm_monitor_output_devices_json(*mut c_void, *mut *mut c_char) -> i32;
    adm_free_string(*mut c_char) -> ();
}
