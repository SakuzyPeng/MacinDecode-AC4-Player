#![cfg(target_os = "windows")]

use std::collections::{BTreeMap, HashSet};
use std::mem::{ManuallyDrop, size_of};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    AudioCategory_Media, AudioObjectType_Dynamic, AudioObjectType_LowFrequency,
    AudioObjectType_None, DEVICE_STATE_ACTIVE, IMMDevice, IMMDeviceEnumerator, ISpatialAudioClient,
    ISpatialAudioObject, ISpatialAudioObjectRenderStream, MMDeviceEnumerator,
    SpatialAudioObjectRenderStreamActivationParams, WAVEFORMATEX, eConsole, eRender,
};
use windows::Win32::Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0_0};
use windows::Win32::System::Com::{
    CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemAlloc, CoTaskMemFree, CoUninitialize, STGM_READ,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{Error as WindowsError, HSTRING};

const COMMAND_WAIT_MILLISECONDS: u32 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderPhase {
    Initializing,
    Ready,
    Playing,
    Paused,
    Ended,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderSnapshot {
    pub phase: RenderPhase,
    pub device_id: Option<String>,
    pub device_label: String,
    pub max_dynamic_objects: u32,
    pub reserved_dynamic_objects: u32,
    pub active_dynamic_objects: u32,
    pub render_updates: u64,
    pub submitted_frames: u64,
    pub playhead_frames: u64,
    pub object_buffer_submissions: u64,
    pub position_updates: u64,
    pub underruns: u64,
    pub error: Option<String>,
}

impl RenderSnapshot {
    fn initializing(reserved_dynamic_objects: u32) -> Self {
        Self {
            phase: RenderPhase::Initializing,
            device_id: None,
            device_label: "Default Windows audio endpoint".to_owned(),
            max_dynamic_objects: 0,
            reserved_dynamic_objects,
            active_dynamic_objects: 0,
            render_updates: 0,
            submitted_frames: 0,
            playhead_frames: 0,
            object_buffer_submissions: 0,
            position_updates: 0,
            underruns: 0,
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputDeviceSelection {
    SystemDefault,
    EndpointId(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceInfo {
    pub id: String,
    pub label: String,
    pub is_default: bool,
    pub max_dynamic_objects: Option<u32>,
    pub spatial_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamConfig {
    pub sample_rate: u32,
    pub dynamic_object_count: u32,
    pub has_lfe: bool,
    pub start_frame: u64,
    pub output_device: OutputDeviceSelection,
}

#[derive(Debug)]
pub struct DynamicObjectRender {
    pub element_id: u64,
    pub active: bool,
    pub position: [f32; 3],
    pub gain: f32,
    pub samples: Vec<f32>,
}

#[derive(Debug)]
pub struct LfeObjectRender {
    pub active: bool,
    pub gain: f32,
    pub samples: Vec<f32>,
}

#[derive(Debug)]
pub struct RenderQuantum {
    pub objects: Vec<DynamicObjectRender>,
    pub lfe: Option<LfeObjectRender>,
    pub frames_written: u32,
    pub end_of_stream: bool,
    pub underrun: bool,
}

pub trait SpatialSource: Send + 'static {
    /// Produces one mono-planar render quantum for the frame count requested by Windows.
    ///
    /// # Errors
    ///
    /// Returns an error when the source cannot provide a valid scene quantum.
    fn render(&mut self, frame_count: u32) -> Result<RenderQuantum, String>;
}

enum Command {
    Play,
    Pause,
    SetMasterGain(f32),
    ReplaceSource {
        source: Box<dyn SpatialSource>,
        start_frame: u64,
    },
    Shutdown,
}

pub struct Renderer {
    command_sender: Sender<Command>,
    snapshot: Arc<Mutex<RenderSnapshot>>,
    join_handle: Option<JoinHandle<()>>,
}

impl Renderer {
    /// Starts the dedicated COM render worker for a validated scene source.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream configuration is empty or the worker thread cannot start.
    pub fn spawn(config: StreamConfig, source: Box<dyn SpatialSource>) -> Result<Self, String> {
        validate_config(&config)?;
        let (command_sender, command_receiver) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(RenderSnapshot::initializing(
            config.dynamic_object_count,
        )));
        let worker_snapshot = Arc::clone(&snapshot);
        let join_handle = thread::Builder::new()
            .name("windows-spatial-audio".to_owned())
            .spawn(move || render_worker(&config, source, &command_receiver, &worker_snapshot))
            .map_err(|error| format!("Failed to start Windows Spatial Audio worker: {error}"))?;
        Ok(Self {
            command_sender,
            snapshot,
            join_handle: Some(join_handle),
        })
    }

    pub fn play(&self) {
        let _ = self.command_sender.send(Command::Play);
    }

    pub fn pause(&self) {
        let _ = self.command_sender.send(Command::Pause);
    }

    pub fn set_master_gain(&self, gain: f32) {
        let _ = self
            .command_sender
            .send(Command::SetMasterGain(sanitize_gain(gain)));
    }

    pub fn replace_source(&self, source: Box<dyn SpatialSource>, start_frame: u64) {
        let _ = self.command_sender.send(Command::ReplaceSource {
            source,
            start_frame,
        });
    }

    #[must_use]
    pub fn snapshot(&self) -> RenderSnapshot {
        lock_recover(&self.snapshot).clone()
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        let _ = self.command_sender.send(Command::Shutdown);
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

fn validate_config(config: &StreamConfig) -> Result<(), String> {
    if config.sample_rate == 0 {
        return Err("Windows Spatial Audio requires a nonzero sample rate".to_owned());
    }
    if config.dynamic_object_count == 0 && !config.has_lfe {
        return Err("Windows Spatial Audio stream has no renderable object".to_owned());
    }
    Ok(())
}

/// Enumerates active Windows render endpoints and probes their Spatial Audio capacity.
///
/// # Errors
///
/// Returns an error when COM or the Windows endpoint enumerator cannot be initialized.
pub fn enumerate_output_devices() -> Result<Vec<AudioDeviceInfo>, String> {
    let _apartment = ComApartment::initialize()?;
    let enumerator = create_device_enumerator()?;
    let default_id = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
        .ok()
        .and_then(|endpoint| endpoint_id(&endpoint).ok());
    let collection = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }
        .map_err(|error| format_windows_error("Enumerating active audio endpoints", &error))?;
    let count = unsafe { collection.GetCount() }
        .map_err(|error| format_windows_error("Reading the audio endpoint count", &error))?;
    let mut devices = Vec::with_capacity(usize::try_from(count).unwrap_or(usize::MAX));
    for index in 0..count {
        let endpoint = unsafe { collection.Item(index) }.map_err(|error| {
            format_windows_error("Opening an enumerated audio endpoint", &error)
        })?;
        let id = endpoint_id(&endpoint)?;
        let label = endpoint_label(&endpoint).unwrap_or_else(|| id.clone());
        let capacity =
            unsafe { endpoint.Activate::<ISpatialAudioClient>(CLSCTX_INPROC_SERVER, None) }
                .and_then(|client| unsafe { client.GetMaxDynamicObjectCount() });
        let (max_dynamic_objects, spatial_error) = match capacity {
            Ok(value) => (Some(value), None),
            Err(error) => (
                None,
                Some(format_windows_error(
                    "Probing Spatial Audio support",
                    &error,
                )),
            ),
        };
        devices.push(AudioDeviceInfo {
            is_default: default_id.as_deref() == Some(id.as_str()),
            id,
            label,
            max_dynamic_objects,
            spatial_error,
        });
    }
    devices.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(devices)
}

fn create_device_enumerator() -> Result<IMMDeviceEnumerator, String> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(|error| format_windows_error("Creating the audio device enumerator", &error))
}

fn selected_endpoint(
    enumerator: &IMMDeviceEnumerator,
    selection: &OutputDeviceSelection,
) -> Result<IMMDevice, String> {
    match selection {
        OutputDeviceSelection::SystemDefault => {
            unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
                .map_err(|error| format_windows_error("Opening the default audio endpoint", &error))
        }
        OutputDeviceSelection::EndpointId(id) => {
            let id = HSTRING::from(id.as_str());
            unsafe { enumerator.GetDevice(&id) }.map_err(|error| {
                format_windows_error("Opening the selected audio endpoint", &error)
            })
        }
    }
}

fn endpoint_id(endpoint: &IMMDevice) -> Result<String, String> {
    let value = unsafe { endpoint.GetId() }
        .map_err(|error| format_windows_error("Reading the audio endpoint ID", &error))?;
    let result = unsafe { value.to_string() }
        .map_err(|error| format!("Audio endpoint ID is not valid UTF-16: {error}"));
    unsafe { CoTaskMemFree(Some(value.as_ptr().cast())) };
    result
}

fn endpoint_label(endpoint: &IMMDevice) -> Option<String> {
    let store = unsafe { endpoint.OpenPropertyStore(STGM_READ) }.ok()?;
    let friendly_name = PKEY_Device_FriendlyName;
    let value = unsafe { store.GetValue(&raw const friendly_name) }.ok()?;
    let label = value.to_string();
    (!label.trim().is_empty()).then_some(label)
}

fn render_worker(
    config: &StreamConfig,
    source: Box<dyn SpatialSource>,
    commands: &Receiver<Command>,
    snapshot: &Arc<Mutex<RenderSnapshot>>,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_render_loop(config, source, commands, snapshot)
    }));
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => fail_snapshot(snapshot, error),
        Err(_) => fail_snapshot(snapshot, "Windows Spatial Audio worker panicked".to_owned()),
    }
}

fn run_render_loop(
    config: &StreamConfig,
    mut source: Box<dyn SpatialSource>,
    commands: &Receiver<Command>,
    snapshot: &Arc<Mutex<RenderSnapshot>>,
) -> Result<(), String> {
    let _apartment = ComApartment::initialize()?;
    let mut context = SpatialContext::open(config)?;
    {
        let mut state = lock_recover(snapshot);
        state.phase = RenderPhase::Ready;
        state.device_id = Some(context.device_id.clone());
        state.device_label.clone_from(&context.device_label);
        state.max_dynamic_objects = context.max_dynamic_objects;
        state.playhead_frames = config.start_frame;
    }

    let mut playing = false;
    let mut master_gain = 1.0f32;
    loop {
        if process_commands(
            commands,
            &mut playing,
            &mut master_gain,
            &mut source,
            snapshot,
        ) {
            return Ok(());
        }

        let wait = unsafe { WaitForSingleObject(context.event.0, COMMAND_WAIT_MILLISECONDS) };
        if wait == WAIT_TIMEOUT {
            continue;
        }
        if wait != WAIT_OBJECT_0 {
            return Err("Waiting for the Windows Spatial Audio buffer event failed".to_owned());
        }

        let (available_dynamic_objects, frame_count) = context.begin_update()?;
        let submit_result = if frame_count == 0 {
            Err("Windows Spatial Audio requested a zero-length buffer".to_owned())
        } else {
            let quantum = if playing {
                source.render(frame_count)
            } else {
                Ok(silent_quantum())
            };
            quantum.and_then(|quantum| {
                context.submit_quantum(quantum, frame_count, available_dynamic_objects, master_gain)
            })
        };
        let end_result = context.end_update();
        let outcome = submit_result?;
        end_result?;
        if outcome.end_of_stream {
            context.release_ended_objects();
        }

        {
            let mut state = lock_recover(snapshot);
            state.render_updates = state.render_updates.saturating_add(1);
            state.submitted_frames = state
                .submitted_frames
                .saturating_add(u64::from(outcome.frames_written));
            state.playhead_frames = state
                .playhead_frames
                .saturating_add(u64::from(outcome.frames_written));
            state.object_buffer_submissions = state
                .object_buffer_submissions
                .saturating_add(u64::from(outcome.object_buffer_submissions));
            state.position_updates = state
                .position_updates
                .saturating_add(u64::from(outcome.position_updates));
            state.active_dynamic_objects = if outcome.end_of_stream {
                0
            } else {
                outcome.active_dynamic_objects
            };
            if outcome.underrun {
                state.underruns = state.underruns.saturating_add(1);
            }
            if outcome.end_of_stream {
                playing = false;
                state.phase = RenderPhase::Ended;
            }
        }
    }
}

fn silent_quantum() -> RenderQuantum {
    RenderQuantum {
        objects: Vec::new(),
        lfe: None,
        frames_written: 0,
        end_of_stream: false,
        underrun: false,
    }
}

fn process_commands(
    commands: &Receiver<Command>,
    playing: &mut bool,
    master_gain: &mut f32,
    source: &mut Box<dyn SpatialSource>,
    snapshot: &Arc<Mutex<RenderSnapshot>>,
) -> bool {
    loop {
        match commands.try_recv() {
            Ok(Command::Play) => {
                *playing = true;
                lock_recover(snapshot).phase = RenderPhase::Playing;
            }
            Ok(Command::Pause) => {
                *playing = false;
                lock_recover(snapshot).phase = RenderPhase::Paused;
            }
            Ok(Command::SetMasterGain(gain)) => *master_gain = sanitize_gain(gain),
            Ok(Command::ReplaceSource {
                source: replacement,
                start_frame,
            }) => {
                *source = replacement;
                let mut state = lock_recover(snapshot);
                state.playhead_frames = start_frame;
                state.active_dynamic_objects = 0;
                state.error = None;
                state.phase = if *playing {
                    RenderPhase::Playing
                } else {
                    RenderPhase::Paused
                };
            }
            Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => return true,
            Err(TryRecvError::Empty) => return false,
        }
    }
}

fn fail_snapshot(snapshot: &Arc<Mutex<RenderSnapshot>>, error: String) {
    let mut state = lock_recover(snapshot);
    state.phase = RenderPhase::Failed;
    state.error = Some(error);
}

fn sanitize_gain(gain: f32) -> f32 {
    if gain.is_finite() {
        gain.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct ComApartment;

impl ComApartment {
    fn initialize() -> Result<Self, String> {
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result.is_err() {
            return Err(format_hresult(
                "Initializing COM for Windows Spatial Audio",
                result,
            ));
        }
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct EventHandle(HANDLE);

impl EventHandle {
    fn create() -> Result<Self, String> {
        let handle = unsafe { CreateEventW(None, false, false, None) }
            .map_err(|error| format_windows_error("Creating the Spatial Audio event", &error))?;
        Ok(Self(handle))
    }
}

impl Drop for EventHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct SpatialContext {
    stream: ISpatialAudioObjectRenderStream,
    dynamic_objects: BTreeMap<u64, ISpatialAudioObject>,
    lfe_object: Option<ISpatialAudioObject>,
    _client: ISpatialAudioClient,
    _endpoint: IMMDevice,
    _enumerator: IMMDeviceEnumerator,
    event: EventHandle,
    has_lfe: bool,
    reserved_dynamic_objects: u32,
    max_dynamic_objects: u32,
    device_id: String,
    device_label: String,
    _activation: PROPVARIANT,
    _params: Box<SpatialAudioObjectRenderStreamActivationParams>,
    _format: Box<WAVEFORMATEX>,
}

impl SpatialContext {
    fn open(config: &StreamConfig) -> Result<Self, String> {
        let enumerator = create_device_enumerator()?;
        let endpoint = selected_endpoint(&enumerator, &config.output_device)?;
        let device_id = endpoint_id(&endpoint)?;
        let device_label = endpoint_label(&endpoint).unwrap_or_else(|| device_id.clone());
        let client: ISpatialAudioClient = unsafe { endpoint.Activate(CLSCTX_INPROC_SERVER, None) }
            .map_err(|error| {
                format_windows_error(
                    "Activating ISpatialAudioClient on the selected endpoint",
                    &error,
                )
            })?;
        let max_dynamic_objects = unsafe { client.GetMaxDynamicObjectCount() }
            .map_err(|error| format_windows_error("Reading dynamic-object capacity", &error))?;
        if max_dynamic_objects < config.dynamic_object_count {
            return Err(format!(
                "Selected endpoint provides {max_dynamic_objects} dynamic objects, but the scene requires {}",
                config.dynamic_object_count
            ));
        }

        let format = Box::new(object_format(config.sample_rate)?);
        unsafe { client.IsAudioObjectFormatSupported(std::ptr::from_ref(format.as_ref())) }
            .map_err(|error| {
                format_windows_error("Checking mono f32 Spatial Audio format support", &error)
            })?;
        let event = EventHandle::create()?;
        let static_mask = if config.has_lfe {
            AudioObjectType_LowFrequency
        } else {
            AudioObjectType_None
        };
        let params = Box::new(SpatialAudioObjectRenderStreamActivationParams {
            ObjectFormat: std::ptr::from_ref(format.as_ref()),
            StaticObjectTypeMask: static_mask,
            MinDynamicObjectCount: config.dynamic_object_count,
            MaxDynamicObjectCount: config.dynamic_object_count,
            Category: AudioCategory_Media,
            EventHandle: event.0,
            NotifyObject: ManuallyDrop::new(None),
        });
        let activation = activation_variant(params.as_ref())?;
        let stream: ISpatialAudioObjectRenderStream =
            unsafe { client.ActivateSpatialAudioStream(std::ptr::from_ref(&activation)) }.map_err(
                |error| format_windows_error("Activating the Spatial Audio object stream", &error),
            )?;
        unsafe { stream.Start() }.map_err(|error| {
            format_windows_error("Starting the Spatial Audio object stream", &error)
        })?;

        Ok(Self {
            stream,
            dynamic_objects: BTreeMap::new(),
            lfe_object: None,
            _client: client,
            _endpoint: endpoint,
            _enumerator: enumerator,
            event,
            has_lfe: config.has_lfe,
            reserved_dynamic_objects: config.dynamic_object_count,
            max_dynamic_objects,
            device_id,
            device_label,
            _activation: activation,
            _params: params,
            _format: format,
        })
    }

    fn begin_update(&self) -> Result<(u32, u32), String> {
        let mut available = 0;
        let mut frame_count = 0;
        unsafe {
            self.stream
                .BeginUpdatingAudioObjects(&raw mut available, &raw mut frame_count)
        }
        .map_err(|error| format_windows_error("Beginning a Spatial Audio update", &error))?;
        Ok((available, frame_count))
    }

    fn end_update(&self) -> Result<(), String> {
        unsafe { self.stream.EndUpdatingAudioObjects() }
            .map_err(|error| format_windows_error("Ending a Spatial Audio update", &error))
    }

    fn submit_quantum(
        &mut self,
        quantum: RenderQuantum,
        frame_count: u32,
        available_dynamic_objects: u32,
        master_gain: f32,
    ) -> Result<SubmitOutcome, String> {
        validate_quantum(&quantum, frame_count, self.has_lfe)?;
        self.activate_objects_for_quantum(&quantum, available_dynamic_objects)?;

        let mut renders = quantum
            .objects
            .into_iter()
            .map(|object| (object.element_id, object))
            .collect::<BTreeMap<_, _>>();
        let mut buffer_submissions = 0u32;
        let mut position_updates = 0u32;
        for (&element_id, object) in &self.dynamic_objects {
            let render = renders.remove(&element_id);
            let (position, gain, samples) =
                render
                    .as_ref()
                    .map_or(([0.0, 0.0, 0.0], 0.0, None), |render| {
                        let gain = if render.active {
                            sanitize_gain(render.gain) * master_gain
                        } else {
                            0.0
                        };
                        (render.position, gain, Some(render.samples.as_slice()))
                    });
            unsafe { object.SetPosition(position[0], position[1], position[2]) }.map_err(
                |error| format_windows_error("Setting a dynamic-object position", &error),
            )?;
            unsafe { object.SetVolume(gain) }
                .map_err(|error| format_windows_error("Setting a dynamic-object volume", &error))?;
            write_object_buffer(object, samples, frame_count)?;
            buffer_submissions = buffer_submissions.saturating_add(1);
            position_updates = position_updates.saturating_add(1);
        }
        if !renders.is_empty() {
            return Err("Scene returned duplicate or unbound dynamic object IDs".to_owned());
        }

        if let Some(object) = self.lfe_object.as_ref() {
            let (gain, samples) = quantum.lfe.as_ref().map_or((0.0, None), |render| {
                let gain = if render.active {
                    sanitize_gain(render.gain) * master_gain
                } else {
                    0.0
                };
                (gain, Some(render.samples.as_slice()))
            });
            unsafe { object.SetVolume(gain) }
                .map_err(|error| format_windows_error("Setting the LFE object volume", &error))?;
            write_object_buffer(object, samples, frame_count)?;
            buffer_submissions = buffer_submissions.saturating_add(1);
        }

        if quantum.end_of_stream {
            for object in self.dynamic_objects.values() {
                unsafe { object.SetEndOfStream(quantum.frames_written) }.map_err(|error| {
                    format_windows_error("Ending a dynamic Spatial Audio object", &error)
                })?;
            }
            if let Some(object) = self.lfe_object.as_ref() {
                unsafe { object.SetEndOfStream(quantum.frames_written) }.map_err(|error| {
                    format_windows_error("Ending the Spatial Audio LFE object", &error)
                })?;
            }
        }

        Ok(SubmitOutcome {
            frames_written: quantum.frames_written,
            object_buffer_submissions: buffer_submissions,
            position_updates,
            active_dynamic_objects: u32::try_from(self.dynamic_objects.len()).unwrap_or(u32::MAX),
            underrun: quantum.underrun,
            end_of_stream: quantum.end_of_stream,
        })
    }

    fn release_ended_objects(&mut self) {
        self.dynamic_objects.clear();
        self.lfe_object = None;
    }

    fn activate_objects_for_quantum(
        &mut self,
        quantum: &RenderQuantum,
        available_dynamic_objects: u32,
    ) -> Result<(), String> {
        let new_ids = quantum
            .objects
            .iter()
            .filter(|object| !self.dynamic_objects.contains_key(&object.element_id))
            .map(|object| object.element_id)
            .collect::<Vec<_>>();
        let total_after_activation = self.dynamic_objects.len().saturating_add(new_ids.len());
        if total_after_activation
            > usize::try_from(self.reserved_dynamic_objects).unwrap_or(usize::MAX)
        {
            return Err(format!(
                "Scene exposed {total_after_activation} object IDs, exceeding the {} reserved Windows slots",
                self.reserved_dynamic_objects
            ));
        }
        if new_ids.len() > usize::try_from(available_dynamic_objects).unwrap_or(usize::MAX) {
            return Err(format!(
                "Windows reported {available_dynamic_objects} available dynamic objects, but {} new scene objects are required",
                new_ids.len()
            ));
        }
        for element_id in new_ids {
            let object = unsafe {
                self.stream
                    .ActivateSpatialAudioObject(AudioObjectType_Dynamic)
            }
            .map_err(|error| {
                format_windows_error("Activating a dynamic Spatial Audio object", &error)
            })?;
            self.dynamic_objects.insert(element_id, object);
        }
        if self.has_lfe && self.lfe_object.is_none() && quantum.lfe.is_some() {
            self.lfe_object = Some(
                unsafe {
                    self.stream
                        .ActivateSpatialAudioObject(AudioObjectType_LowFrequency)
                }
                .map_err(|error| {
                    format_windows_error("Activating the static Spatial Audio LFE object", &error)
                })?,
            );
        }
        Ok(())
    }
}

impl Drop for SpatialContext {
    fn drop(&mut self) {
        let _ = unsafe { self.stream.Stop() };
        let _ = unsafe { self.stream.Reset() };
        self.dynamic_objects.clear();
        self.lfe_object = None;
    }
}

#[derive(Debug, Clone, Copy)]
struct SubmitOutcome {
    frames_written: u32,
    object_buffer_submissions: u32,
    position_updates: u32,
    active_dynamic_objects: u32,
    underrun: bool,
    end_of_stream: bool,
}

fn object_format(sample_rate: u32) -> Result<WAVEFORMATEX, String> {
    let format_tag = u16::try_from(WAVE_FORMAT_IEEE_FLOAT)
        .map_err(|_| "WAVE_FORMAT_IEEE_FLOAT does not fit WAVEFORMATEX".to_owned())?;
    let block_align = u16::try_from(size_of::<f32>())
        .map_err(|_| "f32 size does not fit WAVEFORMATEX".to_owned())?;
    let average_bytes = sample_rate
        .checked_mul(u32::from(block_align))
        .ok_or_else(|| "Spatial Audio byte rate overflow".to_owned())?;
    Ok(WAVEFORMATEX {
        wFormatTag: format_tag,
        nChannels: 1,
        nSamplesPerSec: sample_rate,
        nAvgBytesPerSec: average_bytes,
        nBlockAlign: block_align,
        wBitsPerSample: 32,
        cbSize: 0,
    })
}

fn activation_variant(
    params: &SpatialAudioObjectRenderStreamActivationParams,
) -> Result<PROPVARIANT, String> {
    let size = u32::try_from(size_of::<SpatialAudioObjectRenderStreamActivationParams>())
        .map_err(|_| "Spatial Audio activation parameters exceed a PROPVARIANT blob".to_owned())?;
    // The windows crate clears PROPVARIANT automatically on Drop, so VT_BLOB must point at
    // CoTaskMemAlloc memory. A borrowed pointer would make PropVariantClear free Rust memory.
    let data = unsafe { CoTaskMemAlloc(size as usize) }.cast::<u8>();
    if data.is_null() {
        return Err("Allocating Spatial Audio activation parameters failed".to_owned());
    }
    unsafe {
        std::ptr::copy_nonoverlapping(std::ptr::from_ref(params).cast::<u8>(), data, size as usize);
    }

    let mut activation = PROPVARIANT::default();
    unsafe {
        let inner =
            std::ptr::addr_of_mut!(activation.Anonymous.Anonymous).cast::<PROPVARIANT_0_0>();
        (*inner).vt = VT_BLOB;
        let blob = std::ptr::addr_of_mut!((*inner).Anonymous.blob);
        (*blob).cbSize = size;
        (*blob).pBlobData = data;
    }
    Ok(activation)
}

fn validate_quantum(
    quantum: &RenderQuantum,
    frame_count: u32,
    stream_has_lfe: bool,
) -> Result<(), String> {
    if quantum.frames_written > frame_count {
        return Err("Scene source wrote more frames than Windows requested".to_owned());
    }
    let expected =
        usize::try_from(frame_count).map_err(|_| "Windows frame count exceeds usize".to_owned())?;
    let mut ids = HashSet::with_capacity(quantum.objects.len());
    for object in &quantum.objects {
        if !ids.insert(object.element_id) {
            return Err(format!(
                "Scene source returned object {} more than once",
                object.element_id
            ));
        }
        if object.samples.len() != expected
            || object.samples.iter().any(|sample| !sample.is_finite())
        {
            return Err(format!(
                "Scene object {} returned invalid PCM",
                object.element_id
            ));
        }
        if object
            .position
            .iter()
            .any(|coordinate| !coordinate.is_finite())
        {
            return Err(format!(
                "Scene object {} returned a non-finite position",
                object.element_id
            ));
        }
    }
    match quantum.lfe.as_ref() {
        Some(render)
            if !stream_has_lfe
                || render.samples.len() != expected
                || render.samples.iter().any(|sample| !sample.is_finite()) =>
        {
            Err("Scene source returned invalid or unexpected LFE PCM".to_owned())
        }
        _ => Ok(()),
    }
}

fn write_object_buffer(
    object: &ISpatialAudioObject,
    samples: Option<&[f32]>,
    frame_count: u32,
) -> Result<(), String> {
    let mut raw_buffer = std::ptr::null_mut();
    let mut byte_count = 0u32;
    unsafe { object.GetBuffer(&raw mut raw_buffer, &raw mut byte_count) }
        .map_err(|error| format_windows_error("Getting a Spatial Audio object buffer", &error))?;
    let sample_size = u32::try_from(size_of::<f32>()).unwrap_or(4);
    if raw_buffer.is_null() || !byte_count.is_multiple_of(sample_size) {
        return Err("Windows returned an invalid Spatial Audio object buffer".to_owned());
    }
    let buffer_bytes = usize::try_from(byte_count)
        .map_err(|_| "Spatial Audio object buffer length exceeds usize".to_owned())?;
    let expected =
        usize::try_from(frame_count).map_err(|_| "Windows frame count exceeds usize".to_owned())?;
    let expected_bytes = expected
        .checked_mul(size_of::<f32>())
        .ok_or_else(|| "Spatial Audio object buffer length overflow".to_owned())?;
    if buffer_bytes < expected_bytes {
        return Err(format!(
            "Windows object buffer exposes {} frames, fewer than the requested {expected}",
            buffer_bytes / size_of::<f32>()
        ));
    }
    if samples.is_some_and(|samples| samples.len() != expected) {
        return Err("Scene PCM length does not match the Windows render quantum".to_owned());
    }
    unsafe { std::ptr::write_bytes(raw_buffer, 0, buffer_bytes) };
    if let Some(samples) = samples {
        unsafe {
            std::ptr::copy_nonoverlapping(
                samples.as_ptr().cast::<u8>(),
                raw_buffer,
                expected_bytes,
            );
        }
    }
    Ok(())
}

fn format_windows_error(operation: &str, error: &WindowsError) -> String {
    format!(
        "{operation} (0x{:08X}): {error}",
        error.code().0.cast_unsigned()
    )
}

fn format_hresult(operation: &str, result: windows::core::HRESULT) -> String {
    format!("{operation} (0x{:08X})", result.0.cast_unsigned())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    struct OneQuantumSource;

    struct EmptySource;

    impl SpatialSource for EmptySource {
        fn render(&mut self, _frame_count: u32) -> Result<RenderQuantum, String> {
            Err("old source must have been replaced".to_owned())
        }
    }

    impl SpatialSource for OneQuantumSource {
        fn render(&mut self, frame_count: u32) -> Result<RenderQuantum, String> {
            let samples = usize::try_from(frame_count)
                .map_err(|_| "test frame count exceeds usize".to_owned())?;
            Ok(RenderQuantum {
                objects: vec![DynamicObjectRender {
                    element_id: 1,
                    active: true,
                    position: [0.0, 0.0, -1.0],
                    gain: 1.0,
                    samples: vec![0.0; samples],
                }],
                lfe: None,
                frames_written: frame_count,
                end_of_stream: true,
                underrun: false,
            })
        }
    }

    #[test]
    fn activation_blob_has_com_owned_drop_storage() {
        let params = SpatialAudioObjectRenderStreamActivationParams {
            ObjectFormat: std::ptr::null(),
            StaticObjectTypeMask: AudioObjectType_None,
            MinDynamicObjectCount: 0,
            MaxDynamicObjectCount: 0,
            Category: AudioCategory_Media,
            EventHandle: HANDLE::default(),
            NotifyObject: ManuallyDrop::new(None),
        };
        let activation = activation_variant(&params).expect("owned activation blob");
        assert_eq!(activation.vt(), VT_BLOB);
        drop(activation);
    }

    #[test]
    fn replacing_a_source_resets_the_absolute_playhead_and_preserves_play_state() {
        let (sender, receiver) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(RenderSnapshot::initializing(1)));
        let mut source: Box<dyn SpatialSource> = Box::new(EmptySource);
        let mut playing = true;
        let mut gain = 0.5;
        sender
            .send(Command::ReplaceSource {
                source: Box::new(OneQuantumSource),
                start_frame: 96_000,
            })
            .expect("queue source replacement");

        assert!(!process_commands(
            &receiver,
            &mut playing,
            &mut gain,
            &mut source,
            &snapshot,
        ));
        let state = lock_recover(&snapshot).clone();
        assert_eq!(state.playhead_frames, 96_000);
        assert_eq!(state.phase, RenderPhase::Playing);
        assert_eq!(
            source
                .render(32)
                .expect("replacement source")
                .frames_written,
            32
        );
    }

    #[test]
    #[ignore = "requires one or more Spatial Audio-capable render endpoints"]
    fn opens_enumerated_endpoints_by_stable_id() {
        let devices = enumerate_output_devices().expect("enumerate active render endpoints");
        let mut ids = HashSet::new();
        let mut opened = 0usize;
        for device in &devices {
            assert!(!device.id.is_empty());
            assert!(!device.label.is_empty());
            assert!(ids.insert(device.id.clone()), "duplicate endpoint ID");
            if device.max_dynamic_objects.unwrap_or(0) < 1 {
                continue;
            }
            let renderer = Renderer::spawn(
                StreamConfig {
                    sample_rate: 48_000,
                    dynamic_object_count: 1,
                    has_lfe: false,
                    start_frame: 0,
                    output_device: OutputDeviceSelection::EndpointId(device.id.clone()),
                },
                Box::new(OneQuantumSource),
            )
            .unwrap_or_else(|error| panic!("open endpoint {}: {error}", device.label));
            wait_for_phase(&renderer, RenderPhase::Ready);
            let state = renderer.snapshot();
            assert_eq!(state.device_id.as_deref(), Some(device.id.as_str()));
            renderer.play();
            wait_for_phase(&renderer, RenderPhase::Ended);
            opened += 1;
        }
        eprintln!("enumerated={} spatial_opened={opened}", devices.len());
        assert!(opened > 0, "no active endpoint supports one dynamic object");
    }

    #[test]
    #[ignore = "requires a Spatial Audio-capable default endpoint"]
    fn ended_renderer_releases_objects_without_entering_failed_state() {
        let renderer = Renderer::spawn(
            StreamConfig {
                sample_rate: 48_000,
                dynamic_object_count: 1,
                has_lfe: false,
                start_frame: 0,
                output_device: OutputDeviceSelection::SystemDefault,
            },
            Box::new(OneQuantumSource),
        )
        .expect("spawn renderer");
        wait_for_phase(&renderer, RenderPhase::Ready);
        renderer.play();
        wait_for_phase(&renderer, RenderPhase::Ended);
        thread::sleep(Duration::from_millis(150));
        let state = renderer.snapshot();
        assert_eq!(state.phase, RenderPhase::Ended, "{state:?}");
        assert_eq!(state.active_dynamic_objects, 0);
        assert!(state.error.is_none());
    }

    fn wait_for_phase(renderer: &Renderer, expected: RenderPhase) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = renderer.snapshot();
            assert_ne!(state.phase, RenderPhase::Failed, "{state:?}");
            if state.phase == expected {
                return;
            }
            assert!(Instant::now() < deadline, "{state:?}");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
