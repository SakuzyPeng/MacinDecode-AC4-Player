use core::fmt;

#[cfg(target_os = "windows")]
mod source;
#[cfg(target_os = "windows")]
mod windows;

use crate::decoder::SceneQueueReader;

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
            object_buffer_submissions: 0,
            position_updates: 0,
            underruns: 0,
            error: None,
        }
    }

    #[cfg(target_os = "windows")]
    fn initializing(reserved_dynamic_objects: u32) -> Self {
        Self {
            phase: OutputPhase::Initializing,
            reserved_dynamic_objects,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputStreamConfig {
    request_id: u64,
    sample_rate: u32,
    dynamic_object_count: u32,
    has_lfe: bool,
}

impl OutputStreamConfig {
    pub fn new(
        request_id: u64,
        sample_rate: u32,
        object_count: usize,
        has_lfe: bool,
    ) -> Result<Self, String> {
        let dynamic_object_count = u32::try_from(object_count)
            .map_err(|_| "Scene object count exceeds the Windows API range".to_owned())?;
        Ok(Self {
            request_id,
            sample_rate,
            dynamic_object_count,
            has_lfe,
        })
    }
}

pub struct SpatialOutputController {
    #[cfg(target_os = "windows")]
    renderer: Option<macindecode_windows_spatial_audio::Renderer>,
    config: Option<OutputStreamConfig>,
    snapshot: OutputSnapshot,
    revision: u64,
    master_gain: f32,
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
            config: None,
            snapshot,
            revision: 0,
            master_gain: 1.0,
        }
    }

    pub fn ensure_configured(&mut self, config: OutputStreamConfig, reader: SceneQueueReader) {
        if self.config == Some(config) {
            return;
        }
        self.reset();
        self.config = Some(config);
        #[cfg(target_os = "windows")]
        match windows::spawn(config, reader) {
            Ok(renderer) => {
                renderer.set_master_gain(self.master_gain);
                self.renderer = Some(renderer);
                self.set_snapshot(OutputSnapshot::initializing(config.dynamic_object_count));
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
        if let Some(renderer) = self.renderer.as_ref() {
            self.set_snapshot(windows::snapshot(renderer.snapshot()));
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

    pub fn is_configured_for_request(&self, request_id: u64) -> bool {
        self.config
            .is_some_and(|config| config.request_id == request_id)
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
}
