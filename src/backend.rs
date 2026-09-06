use core::fmt;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

#[cfg(all(target_os = "macos", macinrender_output))]
mod atmos;
mod controller;
#[cfg(macinrender_output)]
mod macinrender;
mod settings;
pub use controller::SpatialOutputController;
pub use settings::{OutputSettings, SpeakerLayout};

#[cfg(feature = "decode")]
#[cfg_attr(
    windows_spatial_output,
    allow(
        dead_code,
        reason = "the preview clock is built only where no renderer owns the FIFO"
    )
)]
mod preview;
#[cfg(windows_spatial_output)]
mod source;
// Deliberately not gated on Windows. It is arithmetic over cross-platform
// decoder types, and keeping it out of `source` is what lets its tests run
// everywhere instead of only where the render callback compiles.
#[cfg_attr(
    not(feature = "decode"),
    allow(
        dead_code,
        reason = "element state is resolved by the render callback and by the \
                  scene preview, and neither exists without a decoder"
    )
)]
mod state;
#[cfg(windows_spatial_output)]
mod windows;

use crate::decoder::{SceneQueueReader, SceneSignature};
use crate::scene_view::SceneViewMirror;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum OutputDeviceSelection {
    #[default]
    SystemDefault,
    EndpointId(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDeviceInfo {
    id: String,
    label: String,
    is_default: bool,
    max_dynamic_objects: Option<u32>,
    spatial_error: Option<String>,
}

impl OutputDeviceInfo {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub const fn is_default(&self) -> bool {
        self.is_default
    }

    pub const fn max_dynamic_objects(&self) -> Option<u32> {
        self.max_dynamic_objects
    }

    pub fn spatial_error(&self) -> Option<&str> {
        self.spatial_error.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SpatialBackendKind {
    #[default]
    Automatic,
    WindowsSpatialAudio,
    #[serde(alias = "AppleAuSpatialMixer")]
    SystemSpatial,
    SafBinaural,
}

impl SpatialBackendKind {
    pub const ALL: [Self; 4] = [
        Self::Automatic,
        Self::WindowsSpatialAudio,
        Self::SystemSpatial,
        Self::SafBinaural,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::WindowsSpatialAudio => "Windows object passthrough",
            Self::SystemSpatial => "System spatial audio",
            Self::SafBinaural => "SAF binaural",
        }
    }

    pub const fn availability(self) -> &'static str {
        match self {
            Self::Automatic => "Windows object passthrough / macOS system spatial audio",
            Self::WindowsSpatialAudio => "Dynamic objects plus one static LFE",
            Self::SystemSpatial => "Apple-geometry speaker bed through the system spatializer",
            Self::SafBinaural => "Software HRTF rendering with KEMAR or a SOFA dataset",
        }
    }

    pub const fn resolved(self) -> Self {
        match self {
            Self::Automatic if cfg!(windows_spatial_output) => Self::WindowsSpatialAudio,
            Self::Automatic if cfg!(macinrender_output) => Self::SystemSpatial,
            _ => self,
        }
    }

    pub const fn supported(self) -> bool {
        match self {
            Self::Automatic => true,
            Self::WindowsSpatialAudio => cfg!(windows_spatial_output),
            Self::SystemSpatial | Self::SafBinaural => cfg!(macinrender_output),
        }
    }
}

impl fmt::Display for SpatialBackendKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "one cross-platform phase is necessarily dormant on each target"
)]
pub enum OutputPhase {
    Unavailable,
    Idle,
    Initializing,
    Ready,
    Playing,
    Paused,
    Ended,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "clock variants depend on the compiled output backends"
)]
enum OutputClock {
    Unknown,
    Callback,
    SystemMedia,
    Preview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSnapshot {
    #[cfg(all(target_os = "macos", macinrender_output))]
    atmos_assist: Option<String>,
    queued_output_frames: Option<u64>,
    buffering: bool,
    clock: OutputClock,
    phase: OutputPhase,
    device_label: String,
    max_dynamic_objects: u32,
    reserved_dynamic_objects: u32,
    active_dynamic_objects: u32,
    render_updates: u64,
    submitted_frames: u64,
    playhead_frames: u64,
    object_buffer_submissions: u64,
    position_updates: u64,
    underruns: u64,
    error: Option<String>,
    /// Whether this is the scene preview rather than a real audio stream.
    ///
    /// A flag rather than an `OutputPhase` variant on purpose: the preview
    /// moves through Ready/Playing/Paused/Ended exactly like playback does, and
    /// every consumer that gates on phase — the transport, the timeline, the
    /// repaint cadence — should treat it the same. Only the words differ.
    preview: bool,
}

impl OutputSnapshot {
    #[cfg(all(target_os = "macos", macinrender_output))]
    pub fn atmos_assist_status(&self) -> Option<&str> {
        self.atmos_assist.as_deref()
    }

    fn idle() -> Self {
        Self {
            #[cfg(all(target_os = "macos", macinrender_output))]
            atmos_assist: None,
            queued_output_frames: None,
            buffering: false,
            clock: OutputClock::Unknown,
            phase: OutputPhase::Idle,
            device_label: "Default Windows audio endpoint".to_owned(),
            max_dynamic_objects: 0,
            reserved_dynamic_objects: 0,
            active_dynamic_objects: 0,
            render_updates: 0,
            submitted_frames: 0,
            playhead_frames: 0,
            object_buffer_submissions: 0,
            position_updates: 0,
            underruns: 0,
            error: None,
            preview: false,
        }
    }

    /// The scene view running without an audio device behind it.
    #[cfg(feature = "decode")]
    fn preview(phase: OutputPhase, reserved_dynamic_objects: u32, playhead_frames: u64) -> Self {
        Self {
            clock: OutputClock::Preview,
            phase,
            device_label: "Scene preview · no audio output".to_owned(),
            reserved_dynamic_objects,
            playhead_frames,
            preview: true,
            ..Self::idle()
        }
    }

    #[cfg(windows_spatial_output)]
    fn initializing(reserved_dynamic_objects: u32, playhead_frames: u64) -> Self {
        Self {
            phase: OutputPhase::Initializing,
            reserved_dynamic_objects,
            playhead_frames,
            ..Self::idle()
        }
    }

    #[cfg(not(windows_spatial_output))]
    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            phase: OutputPhase::Unavailable,
            error: Some(reason.into()),
            ..Self::idle()
        }
    }

    #[cfg(windows_spatial_output)]
    fn failed(error: impl Into<String>) -> Self {
        Self {
            phase: OutputPhase::Failed,
            error: Some(error.into()),
            ..Self::idle()
        }
    }

    /// Whether the scene is being previewed rather than played.
    pub const fn is_preview(&self) -> bool {
        self.preview
    }

    pub const fn phase(&self) -> OutputPhase {
        self.phase
    }

    pub fn device_label(&self) -> &str {
        &self.device_label
    }

    pub const fn max_dynamic_objects(&self) -> u32 {
        self.max_dynamic_objects
    }

    pub const fn reserved_dynamic_objects(&self) -> u32 {
        self.reserved_dynamic_objects
    }

    pub const fn active_dynamic_objects(&self) -> u32 {
        self.active_dynamic_objects
    }

    pub const fn render_updates(&self) -> u64 {
        self.render_updates
    }

    pub const fn submitted_frames(&self) -> u64 {
        self.submitted_frames
    }

    pub const fn playhead_frames(&self) -> u64 {
        self.playhead_frames
    }

    pub const fn object_buffer_submissions(&self) -> u64 {
        self.object_buffer_submissions
    }

    pub const fn position_updates(&self) -> u64 {
        self.position_updates
    }

    pub const fn underruns(&self) -> u64 {
        self.underruns
    }

    pub const fn queued_output_frames(&self) -> Option<u64> {
        self.queued_output_frames
    }
    pub const fn is_buffering(&self) -> bool {
        self.buffering
    }
    pub const fn clock_label(&self) -> &'static str {
        match self.clock {
            OutputClock::Unknown => "Unavailable",
            OutputClock::Callback => "Callback estimate",
            OutputClock::SystemMedia => "System media clock",
            OutputClock::Preview => "Scene preview clock",
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub const fn is_playing(&self) -> bool {
        matches!(self.phase, OutputPhase::Playing)
    }

    pub const fn can_play(&self) -> bool {
        matches!(
            self.phase,
            OutputPhase::Ready | OutputPhase::Paused | OutputPhase::Playing
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputStreamConfig {
    request_id: u64,
    playback_epoch: u64,
    start_frame: u64,
    sample_rate: u32,
    dynamic_object_count: u32,
    has_lfe: bool,
    scene_signature: SceneSignature,
    output_device: OutputDeviceSelection,
}

impl OutputStreamConfig {
    pub fn new(
        request_id: u64,
        playback_epoch: u64,
        start_frame: u64,
        sample_rate: u32,
        scene_signature: SceneSignature,
        output_device: OutputDeviceSelection,
    ) -> Result<Self, String> {
        let dynamic_object_count = u32::try_from(scene_signature.object_element_ids().len())
            .map_err(|_| "Scene object count exceeds the Windows API range".to_owned())?;
        let has_lfe = scene_signature.lfe_element_id().is_some();
        Ok(Self {
            request_id,
            playback_epoch,
            start_frame,
            sample_rate,
            dynamic_object_count,
            has_lfe,
            scene_signature,
            output_device,
        })
    }

    #[cfg_attr(not(windows_spatial_output), allow(dead_code))]
    fn stream_compatible(&self, other: &Self) -> bool {
        self.request_id == other.request_id
            && self.sample_rate == other.sample_rate
            && self.dynamic_object_count == other.dynamic_object_count
            && self.has_lfe == other.has_lfe
            && self.scene_signature == other.scene_signature
            && self.output_device == other.output_device
    }
}

pub struct NativeOutputController {
    pose: Arc<crate::head_tracking::PoseMirror>,
    #[cfg(windows_spatial_output)]
    renderer: Option<macindecode_windows_spatial_audio::Renderer>,
    #[cfg(windows_spatial_output)]
    device_catalog: windows::DeviceCatalogWorker,
    config: Option<OutputStreamConfig>,
    /// The render callback's object positions, for the scene view. Held here
    /// rather than per stream so a device recovery or a compatible seek — both
    /// of which build a fresh render source — keep publishing into the same
    /// mirror instead of leaving the view blank until the next quantum.
    scene_view: Arc<SceneViewMirror>,
    /// Drives the scene view where no renderer does. Present only when this
    /// build has no spatial output backend, which is what keeps exactly one
    /// consumer popping the Scene FIFO.
    #[cfg(feature = "decode")]
    preview: Option<preview::ScenePreview>,
    snapshot: OutputSnapshot,
    #[cfg_attr(
        not(feature = "decode"),
        allow(dead_code, reason = "inspection-only builds have no output revisions")
    )]
    revision: u64,
    master_gain: f32,
    preferred_device: OutputDeviceSelection,
    devices: Vec<OutputDeviceInfo>,
    device_catalog_ready: bool,
    device_catalog_error: Option<String>,
}

#[cfg(any(windows_spatial_output, test))]
const fn output_phase_allows_source_replacement(phase: OutputPhase) -> bool {
    matches!(
        phase,
        OutputPhase::Initializing
            | OutputPhase::Ready
            | OutputPhase::Playing
            | OutputPhase::Paused
            | OutputPhase::Ended
    )
}

impl NativeOutputController {
    pub fn new() -> Self {
        #[cfg(windows_spatial_output)]
        let snapshot = OutputSnapshot::idle();
        // Two ways to have no output, and saying which one it is saves the
        // Windows user staring at a message about the wrong platform.
        #[cfg(not(target_os = "windows"))]
        let snapshot = OutputSnapshot::unavailable(
            "Windows Spatial Audio is only available in the Windows build",
        );
        #[cfg(all(target_os = "windows", not(feature = "decode")))]
        let snapshot = OutputSnapshot::unavailable(
            "This build has the decode feature off, so there is nothing to play",
        );
        Self {
            pose: Arc::new(crate::head_tracking::PoseMirror::default()),
            #[cfg(windows_spatial_output)]
            renderer: None,
            #[cfg(windows_spatial_output)]
            device_catalog: windows::DeviceCatalogWorker::spawn(),
            config: None,
            scene_view: Arc::new(SceneViewMirror::new()),
            #[cfg(feature = "decode")]
            preview: None,
            snapshot,
            revision: 0,
            master_gain: 1.0,
            preferred_device: OutputDeviceSelection::SystemDefault,
            devices: Vec::new(),
            device_catalog_ready: false,
            device_catalog_error: None,
        }
    }

    pub fn ensure_configured(&mut self, config: &OutputStreamConfig, reader: SceneQueueReader) {
        #[cfg(windows_spatial_output)]
        if self.config.as_ref() == Some(config)
            && self.renderer.is_some()
            && output_phase_allows_source_replacement(self.snapshot.phase)
        {
            return;
        }
        #[cfg(not(windows_spatial_output))]
        if self.config.as_ref() == Some(config) {
            return;
        }
        #[cfg(windows_spatial_output)]
        if output_phase_allows_source_replacement(self.snapshot.phase)
            && let (Some(current), Some(renderer)) = (self.config.as_ref(), self.renderer.as_ref())
            && current.stream_compatible(config)
            && windows::replace_source(
                renderer,
                config,
                reader.clone(),
                Arc::clone(&self.scene_view),
                Arc::clone(&self.pose),
            )
            .is_ok()
        {
            self.config = Some(config.clone());
            return;
        }
        #[cfg(windows_spatial_output)]
        let restore_phase = self.snapshot.phase;
        self.reset();
        #[cfg(windows_spatial_output)]
        match windows::spawn(
            config,
            reader,
            Arc::clone(&self.scene_view),
            Arc::clone(&self.pose),
        ) {
            Ok(renderer) => {
                self.config = Some(config.clone());
                renderer.set_master_gain(self.master_gain);
                match restore_phase {
                    OutputPhase::Playing => renderer.play(),
                    OutputPhase::Paused => renderer.pause(),
                    _ => {}
                }
                self.renderer = Some(renderer);
                self.set_snapshot(OutputSnapshot::initializing(
                    config.dynamic_object_count,
                    config.start_frame,
                ));
            }
            Err(error) => self.set_snapshot(OutputSnapshot::failed(error)),
        }
        #[cfg(not(windows_spatial_output))]
        {
            self.config = Some(config.clone());
            #[cfg(feature = "decode")]
            {
                // Nothing else will drain this FIFO, so the preview both feeds
                // the stage and keeps the decoder from stalling at its bound.
                self.preview = Some(preview::ScenePreview::new(
                    reader,
                    Arc::clone(&self.scene_view),
                    config.scene_signature.clone(),
                    config.sample_rate,
                    config.start_frame,
                ));
                self.set_snapshot(OutputSnapshot::preview(
                    OutputPhase::Ready,
                    config.dynamic_object_count,
                    config.start_frame,
                ));
            }
            #[cfg(not(feature = "decode"))]
            drop(reader);
        }
    }

    /// The scene view's window onto the render callback.
    ///
    /// On builds without spatial output the preview writes this same mirror.
    #[must_use]
    pub fn scene_view(&self) -> &SceneViewMirror {
        &self.scene_view
    }

    pub fn reset(&mut self) {
        #[cfg(windows_spatial_output)]
        {
            self.renderer = None;
        }
        self.config = None;
        #[cfg(feature = "decode")]
        {
            self.preview = None;
        }
        #[cfg(feature = "decode")]
        self.set_snapshot(OutputSnapshot::idle());
    }

    #[cfg_attr(
        not(windows_spatial_output),
        allow(
            clippy::unused_self,
            reason = "the native renderer needs both Windows and a decoder"
        )
    )]
    pub fn poll(&mut self) {
        #[cfg(windows_spatial_output)]
        {
            if let Some(update) = self.device_catalog.poll() {
                match update {
                    Ok(devices) => {
                        self.devices = devices;
                        self.device_catalog_ready = true;
                        self.device_catalog_error = None;
                    }
                    Err(error) => self.device_catalog_error = Some(error),
                }
            }
            if let Some(renderer) = self.renderer.as_ref() {
                self.set_snapshot(windows::snapshot(renderer.snapshot()));
            }
        }
    }

    /// Move the scene preview forward by one UI frame.
    ///
    /// A no-op wherever a renderer owns the FIFO: there the render callback is
    /// the clock, and a second consumer would steal blocks from it.
    #[cfg_attr(
        not(feature = "decode"),
        allow(
            unused_variables,
            clippy::unused_self,
            reason = "the preview has no decoded blocks to walk without the decode feature"
        )
    )]
    pub fn advance_preview(&mut self, playing: bool, now: Instant) {
        #[cfg(feature = "decode")]
        {
            let reserved = self.snapshot.reserved_dynamic_objects;
            let was_ready = self.snapshot.phase == OutputPhase::Ready;
            let Some(preview) = self.preview.as_mut() else {
                return;
            };
            preview.tick(playing, now);
            let phase = if preview.error().is_some() {
                OutputPhase::Failed
            } else if preview.has_ended() {
                OutputPhase::Ended
            } else if playing {
                OutputPhase::Playing
            } else if was_ready {
                // Nothing has been asked of it yet, so it is still merely ready
                // rather than paused partway through.
                OutputPhase::Ready
            } else {
                OutputPhase::Paused
            };
            let mut snapshot = OutputSnapshot::preview(phase, reserved, preview.playhead_frames());
            snapshot.error = preview.error().map(str::to_owned);
            self.set_snapshot(snapshot);
        }
    }

    #[cfg_attr(
        not(windows_spatial_output),
        allow(
            clippy::unused_self,
            reason = "the native renderer needs both Windows and a decoder"
        )
    )]
    pub fn play(&self) {
        #[cfg(windows_spatial_output)]
        if let Some(renderer) = self.renderer.as_ref() {
            renderer.play();
        }
    }

    #[cfg_attr(
        not(windows_spatial_output),
        allow(
            clippy::unused_self,
            reason = "the native renderer needs both Windows and a decoder"
        )
    )]
    pub fn pause(&self) {
        #[cfg(windows_spatial_output)]
        if let Some(renderer) = self.renderer.as_ref() {
            renderer.pause();
        }
    }

    pub fn set_master_gain(&mut self, gain: f32) {
        let gain = if gain.is_finite() {
            gain.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if (self.master_gain - gain).abs() <= f32::EPSILON {
            return;
        }
        self.master_gain = gain;
        #[cfg(windows_spatial_output)]
        if let Some(renderer) = self.renderer.as_ref() {
            renderer.set_master_gain(self.master_gain);
        }
    }

    pub const fn snapshot(&self) -> &OutputSnapshot {
        &self.snapshot
    }

    #[cfg(all(test, feature = "decode", not(windows_spatial_output)))]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_configured_for_playback(&self, request_id: u64, playback_epoch: u64) -> bool {
        self.config.as_ref().is_some_and(|config| {
            config.request_id == request_id && config.playback_epoch == playback_epoch
        })
    }

    #[cfg(test)]
    pub fn preferred_device(&self) -> &OutputDeviceSelection {
        &self.preferred_device
    }

    pub fn set_preferred_device(&mut self, selection: OutputDeviceSelection) -> bool {
        if self.preferred_device == selection {
            return false;
        }
        self.preferred_device = selection;
        true
    }

    pub fn devices(&self) -> &[OutputDeviceInfo] {
        &self.devices
    }

    pub fn device_catalog_error(&self) -> Option<&str> {
        self.device_catalog_error.as_deref()
    }

    pub const fn device_catalog_ready(&self) -> bool {
        self.device_catalog_ready
    }

    pub fn resolved_device(&self, dynamic_object_count: usize) -> Option<OutputDeviceSelection> {
        let required = u32::try_from(dynamic_object_count).unwrap_or(u32::MAX);
        if let OutputDeviceSelection::EndpointId(id) = &self.preferred_device
            && self.devices.iter().any(|device| {
                device.id == *id
                    && device
                        .max_dynamic_objects
                        .is_some_and(|capacity| capacity >= required)
            })
        {
            return Some(self.preferred_device.clone());
        }
        let default = self
            .devices
            .iter()
            .find(|device| {
                device.is_default
                    && device
                        .max_dynamic_objects
                        .is_some_and(|capacity| capacity >= required)
            })
            .map(|device| OutputDeviceSelection::EndpointId(device.id.clone()));
        if default.is_some() {
            return default;
        }
        (!self.device_catalog_ready).then_some(OutputDeviceSelection::SystemDefault)
    }

    pub fn configured_device(&self) -> Option<&OutputDeviceSelection> {
        self.config.as_ref().map(|config| &config.output_device)
    }

    #[cfg(feature = "decode")]
    fn set_snapshot(&mut self, snapshot: OutputSnapshot) {
        if self.snapshot != snapshot {
            self.snapshot = snapshot;
            self.revision = self.revision.saturating_add(1);
        }
    }
}

impl Default for NativeOutputController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, feature = "decode", not(windows_spatial_output)))]
mod preview_tests {
    use super::*;
    use crate::decoder::{
        DecodedSceneBlock, PlaybackKey, SceneObjectPcm, SharedSceneQueue, SpatialObjectState,
        SpatialPosition, scene_queue_pair,
    };
    use std::time::Duration;

    fn block(start: i64, id: u64) -> DecodedSceneBlock {
        DecodedSceneBlock::new(
            48_000,
            start,
            2048,
            1,
            0,
            None,
            true,
            vec![SceneObjectPcm::new(
                id,
                Some(SpatialObjectState::new(
                    true,
                    Some(SpatialPosition::new(0.0, 0.0, 0.0)),
                    Some(1.0),
                    true,
                )),
                vec![0.0; 2048],
            )],
            None,
            Vec::new(),
        )
    }

    fn configure(
        output: &mut NativeOutputController,
        epoch: u64,
        start: u64,
        blocks: Vec<DecodedSceneBlock>,
    ) -> SharedSceneQueue {
        let key = PlaybackKey::new(1, epoch);
        let config = OutputStreamConfig::new(
            1,
            epoch,
            start,
            48_000,
            SceneSignature::from_block(blocks.first().expect("initial block")),
            OutputDeviceSelection::SystemDefault,
        )
        .expect("preview config");
        let (queue, reader) = scene_queue_pair(key);
        for block in blocks {
            queue.try_push(key, block).expect("queue block");
        }
        output.ensure_configured(&config, reader);
        queue
    }

    #[test]
    fn resetting_a_preview_disables_transport_and_updates_revision() {
        let mut output = NativeOutputController::new();
        let _queue = configure(&mut output, 0, 0, vec![block(0, 7)]);
        let now = Instant::now();
        output.advance_preview(true, now);
        output.advance_preview(true, now + Duration::from_millis(10));
        assert!(output.snapshot().is_playing());
        let revision = output.revision();
        output.reset();
        assert!(!output.snapshot().can_play());
        assert!(!output.snapshot().is_preview());
        assert_eq!(output.snapshot().playhead_frames(), 0);
        assert!(output.revision() > revision);
        output.advance_preview(true, now + Duration::from_secs(10));
        assert_eq!(output.snapshot().phase(), OutputPhase::Idle);
    }

    #[test]
    fn a_topology_failure_can_be_reconfigured_into_a_fresh_preview() {
        let mut output = NativeOutputController::new();
        let _old_queue = configure(&mut output, 0, 0, vec![block(0, 7), block(2048, 8)]);
        let now = Instant::now();
        output.advance_preview(true, now);
        output.advance_preview(true, now + Duration::from_millis(100));
        assert_eq!(output.snapshot().phase(), OutputPhase::Failed);
        assert_eq!(output.snapshot().playhead_frames(), 2048);
        assert!(output.snapshot().is_preview());
        assert!(
            output
                .snapshot()
                .error()
                .is_some_and(|error| error.starts_with("Scene dynamic-object element IDs changed"))
        );

        let _new_queue = configure(&mut output, 1, 2048, vec![block(2048, 8)]);
        let resumed = now + Duration::from_secs(10);
        output.advance_preview(true, resumed);
        assert_eq!(
            output.snapshot().playhead_frames(),
            2048,
            "reconfiguration resets the clock"
        );
        output.advance_preview(true, resumed + Duration::from_millis(10));
        assert_eq!(output.snapshot().phase(), OutputPhase::Playing);
        assert!(output.snapshot().error().is_none());
        let frame = output
            .scene_view()
            .read(PlaybackKey::new(1, 1))
            .expect("new epoch scene");
        assert_eq!(frame.objects()[0].element_id, 8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `windows_spatial_output` is emitted by `build.rs`, so nothing in the compiler
    /// checks that it still means what it is documented to mean. A typo in the
    /// env var it reads would not fail any build: every `#[cfg(windows_spatial_output)]`
    /// item would simply vanish, and the Windows build would compile cleanly
    /// with no audio path at all. This test is the only thing standing between
    /// that mistake and a silent release.
    #[test]
    fn the_derived_gate_is_exactly_windows_and_a_decoder() {
        assert_eq!(
            cfg!(windows_spatial_output),
            cfg!(target_os = "windows") && cfg!(feature = "decode"),
        );
    }

    fn signature(generation: u32, ids: &[u64]) -> SceneSignature {
        SceneSignature::new(generation, 0, Some(7), ids.to_vec(), None)
    }

    fn device(id: &str, is_default: bool, capacity: Option<u32>) -> OutputDeviceInfo {
        OutputDeviceInfo {
            id: id.to_owned(),
            label: id.to_owned(),
            is_default,
            max_dynamic_objects: capacity,
            spatial_error: capacity.is_none().then(|| "unsupported".to_owned()),
        }
    }

    #[test]
    fn backend_labels_are_unique_and_non_empty() {
        for (index, backend) in SpatialBackendKind::ALL.iter().enumerate() {
            assert!(!backend.label().is_empty());
            assert!(
                SpatialBackendKind::ALL[..index]
                    .iter()
                    .all(|previous| previous.label() != backend.label())
            );
        }
    }

    #[test]
    fn unavailable_preferred_device_temporarily_resolves_to_default() {
        let mut output = NativeOutputController::new();
        output.devices = vec![device("default", true, Some(16))];
        output.preferred_device = OutputDeviceSelection::EndpointId("headphones".to_owned());
        assert_eq!(
            output.resolved_device(8),
            Some(OutputDeviceSelection::EndpointId("default".to_owned()))
        );

        output.devices.push(device("headphones", false, Some(16)));
        assert_eq!(
            output.resolved_device(8),
            Some(OutputDeviceSelection::EndpointId("headphones".to_owned()))
        );
    }

    #[test]
    fn insufficient_endpoint_capacity_falls_back_without_forgetting_preference() {
        let mut output = NativeOutputController::new();
        output.devices = vec![
            device("default", true, Some(32)),
            device("small", false, Some(4)),
        ];
        output.preferred_device = OutputDeviceSelection::EndpointId("small".to_owned());
        assert_eq!(
            output.resolved_device(8),
            Some(OutputDeviceSelection::EndpointId("default".to_owned()))
        );
        assert_eq!(
            output.preferred_device(),
            &OutputDeviceSelection::EndpointId("small".to_owned())
        );
    }

    #[test]
    fn preferred_device_round_trips_through_persistent_json() {
        let selection = OutputDeviceSelection::EndpointId("endpoint-id".to_owned());
        let encoded = serde_json::to_string(&selection).expect("serialize device selection");
        let decoded: OutputDeviceSelection =
            serde_json::from_str(&encoded).expect("deserialize device selection");
        assert_eq!(decoded, selection);
    }

    #[test]
    fn source_replacement_requires_the_complete_scene_signature() {
        let first = OutputStreamConfig::new(
            1,
            1,
            0,
            48_000,
            signature(3, &[10, 20]),
            OutputDeviceSelection::SystemDefault,
        )
        .expect("first config");
        let same_scene_new_epoch = OutputStreamConfig::new(
            1,
            2,
            96_000,
            48_000,
            signature(3, &[20, 10]),
            OutputDeviceSelection::SystemDefault,
        )
        .expect("replacement config");
        let changed_ids = OutputStreamConfig::new(
            1,
            2,
            96_000,
            48_000,
            signature(3, &[10, 30]),
            OutputDeviceSelection::SystemDefault,
        )
        .expect("changed config");
        assert!(first.stream_compatible(&same_scene_new_epoch));
        assert!(!first.stream_compatible(&changed_ids));
    }

    #[test]
    fn failed_output_is_not_eligible_for_source_replacement() {
        assert!(!output_phase_allows_source_replacement(OutputPhase::Failed));
        assert!(output_phase_allows_source_replacement(OutputPhase::Ended));
    }

    #[test]
    fn known_incompatible_default_endpoint_waits_for_recovery() {
        let mut output = NativeOutputController::new();
        output.device_catalog_ready = true;
        output.devices = vec![device("default", true, Some(2))];
        assert_eq!(output.resolved_device(8), None);

        output.devices = vec![device("default", true, Some(16))];
        assert_eq!(
            output.resolved_device(8),
            Some(OutputDeviceSelection::EndpointId("default".to_owned()))
        );
    }
}
