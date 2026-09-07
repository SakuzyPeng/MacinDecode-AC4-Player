//! A polled `CoreMotion` handle. Keep it on the independent head-control thread.
use crate::{raw, size};
use std::ffi::c_void;

#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub state: i32,
    pub sequence: u64,
    pub quaternion: [f64; 4],
}

pub struct Motion {
    handle: *mut c_void,
    poll: unsafe extern "C" fn(*mut c_void, *mut raw::HeadSample) -> i32,
    destroy: unsafe extern "C" fn(*mut c_void),
}
impl Motion {
    pub fn new() -> Result<Self, String> {
        #[cfg(not(target_os = "macos"))]
        return Err("AirPods motion is available on macOS".into());
        #[cfg(target_os = "macos")]
        {
            unsafe extern "C" {
                fn mr_headmotion_create() -> *mut c_void;
                fn mr_headmotion_poll(handle: *mut c_void, sample: *mut raw::HeadSample) -> i32;
                fn mr_headmotion_destroy(handle: *mut c_void);
            }
            // SAFETY: constructor takes no arguments and transfers ownership.
            let handle = unsafe { mr_headmotion_create() };
            if handle.is_null() {
                return Err("Cannot create AirPods motion session".into());
            }
            Ok(Self {
                handle,
                poll: mr_headmotion_poll,
                destroy: mr_headmotion_destroy,
            })
        }
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
