use core::fmt;

use serde::{Deserialize, Serialize};

#[cfg(target_os = "windows")]
mod source;
#[cfg(target_os = "windows")]
mod windows;

use crate::decoder::{SceneQueueReader, SceneSignature};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpatialBackendKind {
    #[default]
    Automatic,
    WindowsSpatialAudio,
    AppleAuSpatialMixer,
}

impl SpatialBackendKind {
    pub const ALL: [Self; 3] = [
        Self::Automatic,
        Self::WindowsSpatialAudio,
        Self::AppleAuSpatialMixer,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::WindowsSpatialAudio => "Windows Spatial Audio",
            Self::AppleAuSpatialMixer => "macOS AU Spatial Mixer",
        }
    }

    pub const fn availability(self) -> &'static str {
        match self {
            Self::Automatic => "Uses the native spatial backend for the current platform",
            Self::WindowsSpatialAudio => "Dynamic objects plus one static LFE",
            Self::AppleAuSpatialMixer => "Planned: PointSource buses plus one LFE bus",
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSnapshot {
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
}

impl OutputSnapshot {
    fn idle() -> Self {
        Self {
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
        }
    }

    #[cfg(target_os = "windows")]
    fn initializing(reserved_dynamic_objects: u32, playhead_frames: u64) -> Self {
        Self {
            phase: OutputPhase::Initializing,
            reserved_dynamic_objects,
            playhead_frames,
            ..Self::idle()
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            phase: OutputPhase::Unavailable,
            error: Some(reason.into()),
            ..Self::idle()
        }
    }

    #[cfg(target_os = "windows")]
    fn failed(error: impl Into<String>) -> Self {
        Self {
            phase: OutputPhase::Failed,
            error: Some(error.into()),
            ..Self::idle()
        }
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

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    fn stream_compatible(&self, other: &Self) -> bool {
        self.request_id == other.request_id
            && self.sample_rate == other.sample_rate
            && self.dynamic_object_count == other.dynamic_object_count
            && self.has_lfe == other.has_lfe
            && self.scene_signature == other.scene_signature
            && self.output_device == other.output_device
    }
}

pub struct SpatialOutputController {
    #[cfg(target_os = "windows")]
    renderer: Option<macindecode_windows_spatial_audio::Renderer>,
    #[cfg(target_os = "windows")]
    device_catalog: windows::DeviceCatalogWorker,
    config: Option<OutputStreamConfig>,
    snapshot: OutputSnapshot,
    revision: u64,
    master_gain: f32,
    preferred_device: OutputDeviceSelection,
    devices: Vec<OutputDeviceInfo>,
    device_catalog_ready: bool,
    device_catalog_error: Option<String>,
}

impl SpatialOutputController {
    pub fn new() -> Self {
        #[cfg(target_os = "windows")]
        let snapshot = OutputSnapshot::idle();
        #[cfg(not(target_os = "windows"))]
        let snapshot = OutputSnapshot::unavailable(
            "Windows Spatial Audio is only available in the Windows build",
        );
        Self {
            #[cfg(target_os = "windows")]
            renderer: None,
            #[cfg(target_os = "windows")]
            device_catalog: windows::DeviceCatalogWorker::spawn(),
            config: None,
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
        if self.config.as_ref() == Some(config) {
            return;
        }
        #[cfg(target_os = "windows")]
        if let (Some(current), Some(renderer)) = (self.config.as_ref(), self.renderer.as_ref())
            && current.stream_compatible(config)
        {
            windows::replace_source(renderer, config, reader);
            self.config = Some(config.clone());
            return;
        }
        #[cfg(target_os = "windows")]
        let restore_phase = self.snapshot.phase;
        self.reset();
        self.config = Some(config.clone());
        #[cfg(target_os = "windows")]
        match windows::spawn(config, reader) {
            Ok(renderer) => {
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
        #[cfg(not(target_os = "windows"))]
        {
            drop(reader);
        }
    }

    pub fn reset(&mut self) {
        #[cfg(target_os = "windows")]
        {
            self.renderer = None;
        }
        self.config = None;
        #[cfg(target_os = "windows")]
        self.set_snapshot(OutputSnapshot::idle());
    }

    #[cfg_attr(
        not(target_os = "windows"),
        allow(
            clippy::unused_self,
            reason = "the native renderer exists only on Windows"
        )
    )]
    pub fn poll(&mut self) {
        #[cfg(target_os = "windows")]
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

    #[cfg_attr(
        not(target_os = "windows"),
        allow(
            clippy::unused_self,
            reason = "the native renderer exists only on Windows"
        )
    )]
    pub fn play(&self) {
        #[cfg(target_os = "windows")]
        if let Some(renderer) = self.renderer.as_ref() {
            renderer.play();
        }
    }

    #[cfg_attr(
        not(target_os = "windows"),
        allow(
            clippy::unused_self,
            reason = "the native renderer exists only on Windows"
        )
    )]
    pub fn pause(&self) {
        #[cfg(target_os = "windows")]
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
        #[cfg(target_os = "windows")]
        if let Some(renderer) = self.renderer.as_ref() {
            renderer.set_master_gain(self.master_gain);
        }
    }

    pub const fn snapshot(&self) -> &OutputSnapshot {
        &self.snapshot
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_configured_for_playback(&self, request_id: u64, playback_epoch: u64) -> bool {
        self.config.as_ref().is_some_and(|config| {
            config.request_id == request_id && config.playback_epoch == playback_epoch
        })
    }

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

    #[cfg(target_os = "windows")]
    fn set_snapshot(&mut self, snapshot: OutputSnapshot) {
        if self.snapshot != snapshot {
            self.snapshot = snapshot;
            self.revision = self.revision.saturating_add(1);
        }
    }
}

impl Default for SpatialOutputController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut output = SpatialOutputController::new();
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
        let mut output = SpatialOutputController::new();
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
    fn known_incompatible_default_endpoint_waits_for_recovery() {
        let mut output = SpatialOutputController::new();
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
