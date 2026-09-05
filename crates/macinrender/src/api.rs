use std::ffi::{c_char, c_void};
use std::path::PathBuf;

use libloading::Library;

use crate::raw;

pub fn library_paths(stem: &str) -> Vec<PathBuf> {
    let name = if cfg!(target_os = "windows") {
        format!("{stem}.dll")
    } else {
        format!("lib{stem}.dylib")
    };
    let mut paths = Vec::new();
    if let Ok(executable) = std::env::current_exe()
        && let Some(parent) = executable.parent()
    {
        paths.push(parent.join(&name));
        paths.push(parent.join("../Frameworks").join(&name));
    }
    if let Some(directory) = option_env!("MACINRENDER_BUILT_LIBRARY_DIR") {
        paths.push(PathBuf::from(directory).join(name));
    }
    paths
}

pub fn load_library(stem: &str) -> Result<Library, String> {
    let mut failures = Vec::new();
    for path in library_paths(stem) {
        if !path.is_file() {
            continue;
        }
        // SAFETY: these are the app's packaged or Cargo-built native libraries.
        // Every function pointer is validated and kept alive by its owning Library.
        #[cfg(target_os = "windows")]
        let loaded = unsafe {
            libloading::os::windows::Library::load_with_flags(&path, 0x100 | 0x1000)
                .map(Library::from)
        };
        #[cfg(not(target_os = "windows"))]
        let loaded = unsafe { Library::new(&path) };
        match loaded {
            Ok(library) => return Ok(library),
            Err(error) => failures.push(format!("{}: {error}", path.display())),
        }
    }
    Err(format!(
        "Cannot load {stem}; rebuild or reinstall the complete player. {}",
        failures.join("; ")
    ))
}

macro_rules! api {
    ($($name:ident ($($arg:ty),*) -> $result:ty;)*) => {
        pub struct Api {
            _library: Library,
            $(pub $name: unsafe extern "C" fn($($arg),*) -> $result,)*
        }
        impl Api {
            pub fn load() -> Result<Self, String> {
                let library = load_library("mradm_capi")?;
                // SAFETY: signatures match the pinned C header; ABI tests check POD layouts.
                $(let $name = *unsafe { library.get::<unsafe extern "C" fn($($arg),*) -> $result>(
                    concat!(stringify!($name), "\0").as_bytes()) }.map_err(|e| e.to_string())?;)*
                let api = Self { _library: library, $($name,)* };
                // SAFETY: validated version entrypoints take no pointers.
                if unsafe { (api.adm_api_version_major)() } != 1 || unsafe { (api.adm_api_version_minor)() } < 36 {
                    return Err("MacinRender C ABI v1.36 or later is required".into());
                }
                Ok(api)
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
