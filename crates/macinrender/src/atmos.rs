//! Player-private macOS Atmos content-label helper. It never consumes Scene PCM.
use std::ffi::{CStr, c_char, c_void};
use std::marker::PhantomData;
use std::rc::Rc;

#[repr(C)]
struct RawSnapshot {
    size: u32,
    state: u32,
    generation: u64,
    frames: u64,
    loops: u64,
    tap_errors: u64,
    channels: u32,
    live_items: u32,
    live_taps: u32,
    default_device: u32,
    error: [c_char; 384],
}

unsafe extern "C" {
    fn mr_atmos_create(bytes: *const u8, length: usize, flags: u32) -> *mut c_void;
    fn mr_atmos_set_mode(handle: *mut c_void, mode: u32);
    fn mr_atmos_poll(handle: *mut c_void, snapshot: *mut RawSnapshot) -> i32;
    fn mr_atmos_destroy(handle: *mut c_void);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Starting,
    Active,
    Paused,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub state: State,
    pub frames: u64,
    pub loops: u64,
    pub tap_errors: u64,
    pub channels: u32,
    pub live_items: u32,
    pub live_taps: u32,
    pub default_device: u32,
    pub error: String,
}

/// UI/controller-owned. Commands dispatch asynchronously to the macOS main queue.
/// Deliberately !Send/!Sync; native callbacks never access Rust objects.
pub struct Assist {
    handle: *mut c_void,
    _controller_thread: PhantomData<Rc<()>>,
}

impl Assist {
    pub fn new() -> Result<Self, String> {
        let bytes = include_bytes!("../../../assets/audio/atmos-assist.m4a");
        // SAFETY: native copies this immutable byte slice before returning.
        let handle = unsafe { mr_atmos_create(bytes.as_ptr(), bytes.len(), 0) };
        if handle.is_null() {
            return Err("Cannot create the Atmos label helper".into());
        }
        Ok(Self {
            handle,
            _controller_thread: PhantomData,
        })
    }

    pub fn play(&mut self, playing: bool) {
        // SAFETY: self exclusively owns a live native handle.
        unsafe { mr_atmos_set_mode(self.handle, if playing { 2 } else { 1 }) }
    }

    pub fn stop(&mut self) {
        // SAFETY: stop invalidates asynchronous work before queued teardown.
        unsafe { mr_atmos_set_mode(self.handle, 0) }
    }

    pub fn snapshot(&self) -> Result<Snapshot, String> {
        let mut raw = RawSnapshot {
            size: super::size::<RawSnapshot>(),
            state: 0,
            generation: 0,
            frames: 0,
            loops: 0,
            tap_errors: 0,
            channels: 0,
            live_items: 0,
            live_taps: 0,
            default_device: 0,
            error: [0; 384],
        };
        // SAFETY: the output struct matches the private C header; native checks size.
        if unsafe { mr_atmos_poll(self.handle, &raw mut raw) } == 0 {
            return Err("Cannot read the Atmos label helper status".into());
        }
        // SAFETY: native always NUL-terminates the fixed-size error array.
        let error = unsafe { CStr::from_ptr(raw.error.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        Ok(Snapshot {
            state: match raw.state {
                0 => State::Idle,
                1 => State::Starting,
                2 => State::Active,
                3 => State::Paused,
                _ => State::Failed,
            },
            frames: raw.frames,
            loops: raw.loops,
            tap_errors: raw.tap_errors,
            channels: raw.channels,
            live_items: raw.live_items,
            live_taps: raw.live_taps,
            default_device: raw.default_device,
            error,
        })
    }
}

impl Drop for Assist {
    fn drop(&mut self) {
        // SAFETY: last owner. Native teardown never waits on the main queue, so
        // dropping during a synchronous renderer-worker join cannot deadlock it.
        unsafe { mr_atmos_destroy(self.handle) }
    }
}
