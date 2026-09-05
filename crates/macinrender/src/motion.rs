//! A polled `CoreMotion` handle. Keep it on the independent head-control thread.
use crate::{api::load_library, raw, size};
use libloading::Library;
use std::ffi::c_void;

#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub state: i32,
    pub sequence: u64,
    pub quaternion: [f64; 4],
}

pub struct Motion {
    _library: Library,
    handle: *mut c_void,
    poll: unsafe extern "C" fn(*mut c_void, *mut raw::HeadSample) -> i32,
    destroy: unsafe extern "C" fn(*mut c_void),
}
impl Motion {
    pub fn new() -> Result<Self, String> {
        if !cfg!(target_os = "macos") {
            return Err("AirPods motion is available on macOS".into());
        }
        let library = load_library("mr_headtrack")?;
        // SAFETY: checked symbols implement the standalone shim's pinned header.
        let create = *unsafe {
            library.get::<unsafe extern "C" fn() -> *mut c_void>(b"mr_headmotion_create\0")
        }
        .map_err(|e| e.to_string())?;
        let poll = *unsafe {
            library.get::<unsafe extern "C" fn(*mut c_void, *mut raw::HeadSample) -> i32>(
                b"mr_headmotion_poll\0",
            )
        }
        .map_err(|e| e.to_string())?;
        let destroy = *unsafe {
            library.get::<unsafe extern "C" fn(*mut c_void)>(b"mr_headmotion_destroy\0")
        }
        .map_err(|e| e.to_string())?;
        // SAFETY: constructor takes no arguments and transfers ownership.
        let handle = unsafe { create() };
        if handle.is_null() {
            return Err("Cannot create AirPods motion session".into());
        }
        Ok(Self {
            _library: library,
            handle,
            poll,
            destroy,
        })
    }
    pub fn sample(&mut self) -> Result<Sample, String> {
        let mut sample = raw::HeadSample {
            size: size::<raw::HeadSample>(),
            ..Default::default()
        };
        // SAFETY: this thread exclusively owns handle and the correctly sized sample.
        if unsafe { (self.poll)(self.handle, &raw mut sample) } == 0 {
            return Err("AirPods motion poll failed".into());
        }
        Ok(Sample {
            state: sample.state,
            sequence: sample.sequence,
            quaternion: [sample.w, sample.x, sample.y, sample.z],
        })
    }
}
impl Drop for Motion {
    fn drop(&mut self) {
        // SAFETY: last owner, on the sampling thread; no native callbacks enter Rust.
        unsafe {
            (self.destroy)(self.handle);
        }
    }
}
