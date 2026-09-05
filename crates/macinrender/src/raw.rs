#![allow(dead_code)]
use std::ffi::{c_char, c_void};

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RendererConfig {
    pub size: u32,
    pub renderer: i32,
    pub layout: *const c_char,
    pub sofa: *const c_char,
    pub geometry: i32,
    pub speaker_spread: i32,
    pub binaural_spread: i32,
    pub lfe: i32,
    pub smoothing: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct StreamConfig {
    pub size: u32,
    pub rendering: RendererConfig,
    pub input_rate: u32,
    pub output_rate: u32,
    pub input_samples: u32,
    pub output_frames: u32,
    pub watermark: u32,
    pub reserved: u32,
    pub input_bytes: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Element {
    pub size: u32,
    pub role: i32,
    pub id: u64,
    pub label: *const c_char,
    pub flags: u64,
    pub has_position: i32,
    pub reserved: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub identity: *const c_void,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct State {
    pub size: u32,
    pub reserved: u32,
    pub valid: u64,
    pub active: i32,
    pub gain: f32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub width: f32,
    pub height: f32,
    pub depth: f32,
    pub diffuse: f32,
    pub divergence: f32,
    pub channel_lock: i32,
    pub screen: i32,
    pub head_locked: i32,
    pub divergence_azimuth: f32,
    pub divergence_position: f32,
    pub has_lock_distance: i32,
    pub lock_distance: f32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Plane {
    pub size: u32,
    pub reserved: u32,
    pub id: u64,
    pub samples: *const f32,
    pub count: u32,
    pub stride: u32,
    pub has_signal: i32,
    pub reserved2: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Initial {
    pub size: u32,
    pub reserved: u32,
    pub id: u64,
    pub state: State,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Update {
    pub size: u32,
    pub reserved: u32,
    pub id: u64,
    pub offset: u32,
    pub ramp: u32,
    pub jump: i32,
    pub reserved2: u32,
    pub changed: u64,
    pub state: State,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Frame {
    pub size: u32,
    pub flags: u32,
    pub epoch: u64,
    pub generation: u64,
    pub start: i64,
    pub duration: u32,
    pub plane_count: u32,
    pub planes: *const Plane,
    pub initial_count: u32,
    pub update_count: u32,
    pub initial: *const Initial,
    pub updates: *const Update,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct OutputConfig {
    pub size: u32,
    pub kind: i32,
    pub layout: *const c_char,
    pub device: *const c_char,
    pub geometry: i32,
    pub reserved: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct OutputStatus {
    pub size: u32,
    pub state: i32,
    pub epoch: u64,
    pub consumed: u64,
    pub presented: u64,
    pub queued: u64,
    pub underruns: u64,
    pub clock: i32,
    pub recovering: i32,
    pub failed: i32,
    pub ended: i32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct HeadSample {
    pub size: u32,
    pub state: i32,
    pub sequence: u64,
    pub timestamp: f64,
    pub w: f64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}
