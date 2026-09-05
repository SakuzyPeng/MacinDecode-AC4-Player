//! Safe owner of the `MacinRender` C ABI. Only `Session` can produce Scene frames;
//! cloned controls retain its handles and serialize calls/error-string borrowing.
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

mod api;
pub mod motion;
mod raw;

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::{Arc, Mutex};

use api::Api;

fn size<T>() -> u32 {
    u32::try_from(size_of::<T>()).expect("C ABI structure exceeds u32")
}
fn string(value: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| "Native text contains NUL".into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererSettings {
    pub binaural: bool,
    pub layout: String,
    pub sofa: String,
    pub split_lfe: bool,
}

impl RendererSettings {
    fn with_raw<T>(&self, action: impl FnOnce(&raw::RendererConfig) -> T) -> Result<T, String> {
        let layout = string(if self.binaural {
            "binaural"
        } else {
            &self.layout
        })?;
        let sofa = string(&self.sofa)?;
        Ok(action(&raw::RendererConfig {
            size: size::<raw::RendererConfig>(),
            renderer: if self.binaural { 6 } else { 2 },
            layout: layout.as_ptr(),
            sofa: sofa.as_ptr(),
            geometry: 1, // Player's fixed Apple geometry, on both operating systems.
            lfe: i32::from(self.split_lfe && self.layout == "9+10+3" && !self.binaural),
            ..Default::default()
        }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    SystemSpatial,
    Stereo,
    Null,
}
#[derive(Debug, Clone)]
pub struct Config {
    pub renderer: RendererSettings,
    pub output: OutputKind,
    pub device_id: String,
    pub input_rate: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ObjectState {
    pub active: bool,
    pub gain: f32,
    pub position: Option<[f32; 3]>,
}
impl ObjectState {
    fn raw(self) -> raw::State {
        let [x, y, z] = self.position.unwrap_or([0.0; 3]);
        raw::State {
            size: size::<raw::State>(),
            valid: 3 | if self.position.is_some() { 4 } else { 0 },
            active: i32::from(self.active),
            gain: self.gain,
            x,
            y,
            z,
            ..Default::default()
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Update {
    pub element: u64,
    pub offset: u32,
    pub ramp: u32,
    /// Renderer-native mask: active=1, linear gain=2, Cartesian position=4.
    pub changed: u64,
    pub state: ObjectState,
}
pub struct Plane<'a> {
    pub element: u64,
    pub samples: &'a [f32],
}
pub struct Frame<'a> {
    pub epoch: u64,
    pub generation: u64,
    pub start: i64,
    pub duration: u32,
    pub complete: bool,
    pub planes: &'a [Plane<'a>],
    pub initial: &'a [(u64, ObjectState)],
    pub updates: &'a [Update],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Paused,
    Playing,
    Buffering,
    Draining,
    Ended,
    Failed,
}
#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub phase: Phase,
    pub epoch: u64,
    pub consumed: u64,
    pub presented: u64,
    pub queued: u64,
    pub underruns: u64,
    pub media_clock: bool,
    pub recovering: bool,
}

struct Shared {
    api: Api,
    context: *mut c_void,
    stream: *mut c_void,
    output: *mut c_void,
    gate: Mutex<()>,
}
// SAFETY: the native Scene/output APIs support concurrent producer/control calls.
// gate covers borrowed-error calls; backend preparation owns its per-call error. All
// production methods require the unique Session; controls cannot submit PCM.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}
impl Drop for Shared {
    fn drop(&mut self) {
        // SAFETY: last Arc, so no caller remains. Output stops native callbacks
        // before releasing stream/context; each destroy explicitly permits NULL.
        unsafe {
            (self.api.adm_destroy_scene_output)(self.output);
            (self.api.adm_destroy_scene_stream)(self.stream);
            (self.api.adm_destroy_context)(self.context);
        }
    }
}
impl Shared {
    fn error(&self, code: i32, domain: u8) -> Result<(), String> {
        if code == 0 {
            return Ok(());
        }
        // SAFETY: callers hold gate. The selected handle lives in this Arc, and
        // the pointer is copied before another C ABI call can invalidate it.
        let pointer = unsafe {
            match domain {
                1 => (self.api.adm_scene_stream_last_error_message)(self.stream),
                2 => (self.api.adm_scene_output_last_error_message)(self.output),
                _ => (self.api.adm_context_last_error_message)(self.context),
            }
        };
        let message = copy_text(pointer);
        Err(if message.is_empty() {
            format!("MacinRender error {code}")
        } else {
            message
        })
    }
}
fn copy_text(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    // SAFETY: internal call sites hold the appropriate native string lifetime.
    unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned()
}

pub struct Session {
    inner: Arc<Shared>,
}
#[derive(Clone)]
pub struct Control {
    inner: Arc<Shared>,
}

impl Session {
    pub fn new(config: &Config) -> Result<Self, String> {
        let api = Api::load()?;
        // SAFETY: validated no-argument constructor.
        let context = unsafe { (api.adm_create_context)() };
        if context.is_null() {
            return Err("Cannot create MacinRender context".into());
        }
        let mut owned = Shared {
            api,
            context,
            stream: std::ptr::null_mut(),
            output: std::ptr::null_mut(),
            gate: Mutex::new(()),
        };
        let code = config.renderer.with_raw(|rendering| {
            let raw = raw::StreamConfig {
                size: size::<raw::StreamConfig>(),
                rendering: *rendering,
                input_rate: config.input_rate,
                output_rate: 48_000,
                ..Default::default()
            };
            // SAFETY: every borrowed string/config lives until this synchronous call returns.
            unsafe {
                (owned.api.adm_create_scene_stream)(context, &raw const raw, &raw mut owned.stream)
            }
        })?;
        owned.error(code, 0)?;
        let layout = string(&config.renderer.layout)?;
        let device = string(&config.device_id)?;
        let raw = raw::OutputConfig {
            size: size::<raw::OutputConfig>(),
            kind: match config.output {
                OutputKind::SystemSpatial => 0,
                OutputKind::Stereo => 1,
                OutputKind::Null => 2,
            },
            layout: layout.as_ptr(),
            device: device.as_ptr(),
            geometry: 1,
            reserved: 0,
        };
        // SAFETY: stream/config are live; ownership of the result passes to Shared.
        let code = unsafe {
            (owned.api.adm_create_scene_output)(
                context,
                owned.stream,
                &raw const raw,
                &raw mut owned.output,
            )
        };
        owned.error(code, 0)?;
        Ok(Self {
            inner: Arc::new(owned),
        })
    }
    #[must_use]
    pub fn control(&self) -> Control {
        Control {
            inner: Arc::clone(&self.inner),
        }
    }
    pub fn reset(&mut self, epoch: u64, target: i64) -> Result<(), String> {
        let s = &self.inner;
        let _guard = s.gate.lock().unwrap();
        // SAFETY: output owns the device reset barrier; this is the sole producer.
        s.error(
            unsafe { (s.api.adm_scene_output_begin_epoch)(s.output, epoch, target) },
            2,
        )
    }
    pub fn configure(
        &mut self,
        epoch: u64,
        generation: u64,
        objects: &[u64],
        lfe: Option<u64>,
    ) -> Result<(), String> {
        let mut elements: Vec<_> = objects
            .iter()
            .map(|&id| raw::Element {
                size: size::<raw::Element>(),
                id,
                ..Default::default()
            })
            .collect();
        if let Some(id) = lfe {
            elements.push(raw::Element {
                size: size::<raw::Element>(),
                id,
                role: 2,
                label: c"LFE1".as_ptr(),
                ..Default::default()
            });
        }
        let count = u32::try_from(elements.len()).map_err(|_| "Too many Scene elements")?;
        let s = &self.inner;
        let _guard = s.gate.lock().unwrap();
        // SAFETY: this descriptor array is deeply copied before the call returns.
        s.error(
            unsafe {
                (s.api.adm_scene_stream_configure_generation)(
                    s.stream,
                    epoch,
                    generation,
                    elements.as_ptr(),
                    count,
                )
            },
            1,
        )
    }
    /// Returns false for backpressure. The caller retains and retries that frame.
    pub fn submit(&mut self, frame: &Frame<'_>) -> Result<bool, String> {
        let planes: Vec<_> = frame
            .planes
            .iter()
            .map(|plane| {
                Ok(raw::Plane {
                    size: size::<raw::Plane>(),
                    id: plane.element,
                    samples: plane.samples.as_ptr(),
                    count: if plane.samples.is_empty() {
                        frame.duration
                    } else {
                        u32::try_from(plane.samples.len()).map_err(|_| "PCM plane too long")?
                    },
                    stride: 1,
                    has_signal: i32::from(!plane.samples.is_empty()),
                    ..Default::default()
                })
            })
            .collect::<Result<_, &str>>()?;
        let initial: Vec<_> = frame
            .initial
            .iter()
            .map(|(id, state)| raw::Initial {
                size: size::<raw::Initial>(),
                id: *id,
                state: state.raw(),
                reserved: 0,
            })
            .collect();
        let updates: Vec<_> = frame
            .updates
            .iter()
            .map(|u| raw::Update {
                size: size::<raw::Update>(),
                id: u.element,
                offset: u.offset,
                ramp: u.ramp,
                jump: i32::from(u.ramp == 0 && u.changed & 4 != 0),
                changed: u.changed,
                state: u.state.raw(),
                ..Default::default()
            })
            .collect();
        let raw = raw::Frame {
            size: size::<raw::Frame>(),
            flags: u32::from(frame.complete),
            epoch: frame.epoch,
            generation: frame.generation,
            start: frame.start,
            duration: frame.duration,
            plane_count: u32::try_from(planes.len()).map_err(|_| "Too many planes")?,
            planes: planes.as_ptr(),
            initial_count: u32::try_from(initial.len()).map_err(|_| "Too many initial states")?,
            initial: initial.as_ptr(),
            update_count: u32::try_from(updates.len()).map_err(|_| "Too many metadata events")?,
            updates: updates.as_ptr(),
        };
        let s = &self.inner;
        let _guard = s.gate.lock().unwrap();
        let mut accepted = 3;
        // SAFETY: every pointed-to array remains alive until submit deep-copies it.
        s.error(
            unsafe {
                (s.api.adm_scene_stream_submit_frame)(
                    s.stream,
                    &raw const raw,
                    0,
                    &raw mut accepted,
                )
            },
            1,
        )?;
        match accepted {
            0 => Ok(true),
            1 | 2 => Ok(false),
            _ => Err("MacinRender Scene input closed".into()),
        }
    }
    pub fn end(&mut self, epoch: u64, end: i64) -> Result<(), String> {
        let s = &self.inner;
        let _guard = s.gate.lock().unwrap();
        // SAFETY: called only by the producer after its last accepted frame.
        s.error(
            unsafe { (s.api.adm_scene_stream_signal_end)(s.stream, epoch, end) },
            1,
        )
    }
}

impl Control {
    pub fn play(&self, playing: bool) -> Result<(), String> {
        let s = &self.inner;
        let _guard = s.gate.lock().unwrap();
        // SAFETY: the Arc owns the output; native controls serialize with consumption.
        s.error(
            unsafe {
                if playing {
                    (s.api.adm_scene_output_play)(s.output)
                } else {
                    (s.api.adm_scene_output_pause)(s.output)
                }
            },
            2,
        )
    }
    pub fn volume(&self, gain: f32) -> Result<(), String> {
        let s = &self.inner;
        let _guard = s.gate.lock().unwrap();
        // SAFETY: native code validates the scalar and serializes the control.
        s.error(
            unsafe { (s.api.adm_scene_output_set_volume)(s.output, gain) },
            2,
        )
    }
    pub fn orientation(&self, pose: [f32; 3]) -> Result<(), String> {
        let s = &self.inner;
        let _guard = s.gate.lock().unwrap();
        // SAFETY: pose is copied synchronously, with native finite-value validation.
        s.error(
            unsafe {
                (s.api.adm_scene_stream_set_listener_orientation)(
                    s.stream, pose[0], pose[1], pose[2],
                )
            },
            1,
        )
    }
    pub fn switch_renderer(&self, settings: &RendererSettings) -> Result<(), String> {
        let s = &self.inner;
        let mut error = std::ptr::null_mut();
        let code = settings.with_raw(|raw| {
            // SAFETY: the Arc retains the stream. This entrypoint owns its error
            // string, and native preparation runs concurrently with the producer.
            unsafe { (s.api.adm_scene_stream_switch_backend_ex)(s.stream, raw, &raw mut error) }
        })?;
        let message = copy_text(error);
        // SAFETY: ownership was transferred by the _ex entrypoint.
        unsafe {
            (s.api.adm_free_string)(error);
        }
        if code == 0 {
            Ok(())
        } else {
            Err(if message.is_empty() {
                format!("MacinRender backend error {code}")
            } else {
                message
            })
        }
    }
    pub fn status(&self) -> Result<Status, String> {
        let s = &self.inner;
        let _guard = s.gate.lock().unwrap();
        let mut raw = raw::OutputStatus {
            size: size::<raw::OutputStatus>(),
            ..Default::default()
        };
        // SAFETY: the status struct is initialized with its verified ABI size.
        s.error(
            unsafe { (s.api.adm_scene_output_get_status)(s.output, &raw mut raw) },
            2,
        )?;
        Ok(Status {
            phase: match raw.state {
                0 => Phase::Paused,
                1 => Phase::Playing,
                2 => Phase::Buffering,
                3 => Phase::Draining,
                4 => Phase::Ended,
                _ => Phase::Failed,
            },
            epoch: raw.epoch,
            consumed: raw.consumed,
            presented: raw.presented,
            queued: raw.queued,
            underruns: raw.underruns,
            media_clock: raw.clock == 1,
            recovering: raw.recovering != 0,
        })
    }
}

pub fn output_devices() -> Result<Vec<(String, String, bool)>, String> {
    let api = Api::load()?;
    // SAFETY: constructor has no arguments; the temporary context is released below.
    let context = unsafe { (api.adm_create_context)() };
    if context.is_null() {
        return Err("Cannot create device query context".into());
    }
    let mut json = std::ptr::null_mut();
    // SAFETY: out pointer is valid; JSON ownership is transferred to this caller.
    let code = unsafe { (api.adm_monitor_output_devices_json)(context, &raw mut json) };
    let text = copy_text(json);
    // SAFETY: these handles belong to this function, including a nullable string.
    unsafe {
        (api.adm_free_string)(json);
        (api.adm_destroy_context)(context);
    }
    if code != 0 {
        return Err("Cannot enumerate playback devices".into());
    }
    let data: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(data
        .as_array()
        .ok_or("Invalid device list")?
        .iter()
        .filter_map(|value| {
            Some((
                value["id"].as_str()?.into(),
                value["name"].as_str()?.into(),
                value["default"].as_bool().unwrap_or(false),
            ))
        })
        .collect())
}

#[cfg(all(test, native_macinrender))]
mod tests {
    use super::*;
    unsafe extern "C" {
        fn macinrender_abi_size(index: u32) -> usize;
        fn macinrender_abi_offset(index: u32) -> usize;
    }
    #[test]
    fn rust_layouts_match_the_actual_c_header() {
        let sizes = [
            size_of::<raw::RendererConfig>(),
            size_of::<raw::StreamConfig>(),
            size_of::<raw::Element>(),
            size_of::<raw::State>(),
            size_of::<raw::Plane>(),
            size_of::<raw::Initial>(),
            size_of::<raw::Update>(),
            size_of::<raw::Frame>(),
            size_of::<raw::OutputConfig>(),
            size_of::<raw::OutputStatus>(),
            size_of::<raw::HeadSample>(),
        ];
        for (index, size) in sizes.into_iter().enumerate() {
            // SAFETY: probe is built from the upstream header and returns a scalar.
            assert_eq!(
                size,
                unsafe { macinrender_abi_size(u32::try_from(index).unwrap()) },
                "POD {index}"
            );
        }
        let offsets = [
            std::mem::offset_of!(raw::RendererConfig, geometry),
            std::mem::offset_of!(raw::StreamConfig, input_rate),
            std::mem::offset_of!(raw::StreamConfig, input_bytes),
            std::mem::offset_of!(raw::Element, identity),
            std::mem::offset_of!(raw::State, valid),
            std::mem::offset_of!(raw::State, gain),
            std::mem::offset_of!(raw::State, head_locked),
            std::mem::offset_of!(raw::Plane, samples),
            std::mem::offset_of!(raw::Plane, has_signal),
            std::mem::offset_of!(raw::Initial, state),
            std::mem::offset_of!(raw::Update, changed),
            std::mem::offset_of!(raw::Update, state),
            std::mem::offset_of!(raw::Frame, start),
            std::mem::offset_of!(raw::Frame, planes),
            std::mem::offset_of!(raw::Frame, updates),
            std::mem::offset_of!(raw::OutputConfig, geometry),
            std::mem::offset_of!(raw::OutputStatus, presented),
            std::mem::offset_of!(raw::OutputStatus, clock),
            std::mem::offset_of!(raw::HeadSample, w),
        ];
        for (index, offset) in offsets.into_iter().enumerate() {
            // SAFETY: C's offsetof probe returns a scalar from the pinned headers.
            assert_eq!(
                offset,
                unsafe { macinrender_abi_offset(u32::try_from(index).unwrap()) },
                "field {index}"
            );
        }
    }

    #[test]
    #[ignore = "requires MACINDECODE_AC4_TEST_SOFA; validates concurrent HRTF preparation"]
    fn hrtf_preparation_keeps_the_producer_and_output_controls_live() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};
        let path = std::env::var("MACINDECODE_AC4_TEST_SOFA").expect("set SOFA path");
        let settings = RendererSettings {
            binaural: true,
            layout: "4+7+0".into(),
            sofa: String::new(),
            split_lfe: true,
        };
        let mut session = Session::new(&Config {
            renderer: settings.clone(),
            output: OutputKind::Null,
            device_id: String::new(),
            input_rate: 48_000,
        })
        .unwrap();
        session.reset(1, 0).unwrap();
        session.configure(1, 1, &[7], None).unwrap();
        let control = session.control();
        control.play(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let producer = std::thread::spawn(move || {
            let samples = [0.01; 480];
            let initial = [(
                7,
                ObjectState {
                    active: true,
                    gain: 1.0,
                    position: Some([0.0, 1.0, 0.0]),
                },
            )];
            let planes = [Plane {
                element: 7,
                samples: &samples,
            }];
            let mut start = 0;
            while !worker_stop.load(Ordering::Relaxed) {
                if session
                    .submit(&Frame {
                        epoch: 1,
                        generation: 1,
                        start,
                        duration: 480,
                        complete: true,
                        planes: &planes,
                        initial: &initial,
                        updates: &[],
                    })
                    .unwrap()
                {
                    start += 480;
                } else {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(40);
        while control.status().unwrap().presented < 4800 {
            assert!(Instant::now() < deadline, "startup timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        let invalid = RendererSettings {
            sofa: "/missing/listener.sofa".into(),
            ..settings.clone()
        };
        assert!(control.switch_renderer(&invalid).is_err());
        let before = control.status().unwrap().presented;
        let loader_control = control.clone();
        let loader = std::thread::spawn(move || {
            loader_control.switch_renderer(&RendererSettings {
                sofa: path,
                ..settings
            })
        });
        let mut advanced = false;
        while !loader.is_finished() {
            let call_start = Instant::now();
            let status = control.status().unwrap();
            assert!(
                call_start.elapsed() < Duration::from_secs(1),
                "status blocked behind HRTF preparation"
            );
            assert_ne!(status.phase, Phase::Failed);
            advanced |= status.presented > before;
            assert!(Instant::now() < deadline, "HRTF preparation timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        let outcome = loader.join().unwrap();
        stop.store(true, Ordering::Relaxed);
        producer.join().unwrap();
        assert!(outcome.is_ok(), "{outcome:?}");
        assert!(
            advanced,
            "media did not advance while preparing the SOFA dataset"
        );
    }
}
