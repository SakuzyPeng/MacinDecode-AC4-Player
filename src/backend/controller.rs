#[cfg(macinrender_output)]
use std::sync::Arc;
use std::time::Instant;

#[cfg(macinrender_output)]
use super::OutputPhase;
use super::{
    HptfReadout, NativeOutputController, OutputDeviceInfo, OutputDeviceSelection, OutputSettings,
    OutputSnapshot, OutputStreamConfig, SpatialBackendKind,
};
use crate::decoder::SceneQueueReader;
use crate::head_tracking::{HeadSnapshot, HeadTracker};
use crate::scene_view::SceneViewMirror;
#[cfg(macinrender_output)]
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[cfg(macinrender_output)]
pub struct PreparedSettings {
    settings: OutputSettings,
    session: super::macinrender::PreparedSession,
}
#[cfg(macinrender_output)]
struct Preparation {
    key: (u64, u64),
    result: Receiver<Result<PreparedSettings, String>>,
}

pub struct SpatialOutputController {
    #[cfg(all(target_os = "macos", macinrender_output))]
    atmos: super::atmos::AtmosController,
    legacy: NativeOutputController,
    settings: OutputSettings,
    snapshot: OutputSnapshot,
    revision: u64,
    head: HeadTracker,
    #[cfg(macinrender_output)]
    runtime: Option<super::macinrender::Runtime>,
    #[cfg(macinrender_output)]
    config: Option<OutputStreamConfig>,
    #[cfg(macinrender_output)]
    catalog: super::macinrender::DeviceCatalog,
    #[cfg(macinrender_output)]
    pcm_devices: Vec<OutputDeviceInfo>,
    #[cfg(macinrender_output)]
    pcm_ready: bool,
    #[cfg(macinrender_output)]
    pcm_error: Option<String>,
    #[cfg(macinrender_output)]
    pending_hot: Option<OutputSettings>,
    #[cfg(macinrender_output)]
    preparation: Option<Preparation>,
    #[cfg(macinrender_output)]
    prepared_session: Option<super::macinrender::PreparedSession>,
    /// Separate the last accepted profile from the request still being prepared.
    /// Both belong to the current native output and reset with it.
    #[cfg(macinrender_output)]
    hptf_accepted: Option<(super::HptfRequest, u64)>,
    #[cfg(macinrender_output)]
    hptf_pending: Option<(super::HptfRequest, u64)>,
    #[cfg(macinrender_output)]
    hptf_revision: u64,
    hptf_error: Option<String>,
    update_result: Option<Result<(), String>>,
    playing: bool,
    gain: f32,
    default_device: OutputDeviceSelection,
}
impl SpatialOutputController {
    pub fn new() -> Self {
        let head = HeadTracker::new();
        let mut legacy = NativeOutputController::new();
        legacy.pose = head.mirror();
        let snapshot = if cfg!(macinrender_output) {
            OutputSnapshot::idle()
        } else {
            legacy.snapshot().clone()
        };
        Self {
            #[cfg(all(target_os = "macos", macinrender_output))]
            atmos: super::atmos::AtmosController::default(),
            legacy,
            settings: OutputSettings::default(),
            snapshot,
            revision: 0,
            head,
            #[cfg(macinrender_output)]
            runtime: None,
            #[cfg(macinrender_output)]
            config: None,
            #[cfg(macinrender_output)]
            catalog: super::macinrender::DeviceCatalog::spawn(),
            #[cfg(macinrender_output)]
            pcm_devices: Vec::new(),
            #[cfg(macinrender_output)]
            pcm_ready: false,
            #[cfg(macinrender_output)]
            pcm_error: None,
            #[cfg(macinrender_output)]
            pending_hot: None,
            #[cfg(macinrender_output)]
            preparation: None,
            #[cfg(macinrender_output)]
            prepared_session: None,
            #[cfg(macinrender_output)]
            hptf_accepted: None,
            #[cfg(macinrender_output)]
            hptf_pending: None,
            #[cfg(macinrender_output)]
            hptf_revision: 0,
            hptf_error: None,
            update_result: None,
            playing: false,
            gain: 1.0,
            default_device: OutputDeviceSelection::SystemDefault,
        }
    }
    fn uses_macinrender(&self) -> bool {
        cfg!(macinrender_output)
            && matches!(
                self.settings.mode.resolved(),
                SpatialBackendKind::SystemSpatial | SpatialBackendKind::SafBinaural
            )
    }
    pub fn settings(&self) -> &OutputSettings {
        &self.settings
    }
    /// The SOFA accepted by the current native binaural output. Preferences
    /// alone are not evidence: they can be selected before any output exists.
    #[cfg_attr(
        not(macinrender_output),
        allow(
            clippy::unused_self,
            reason = "no native HRTF can be active in inspection builds"
        )
    )]
    pub fn active_sofa(&self) -> Option<&str> {
        #[cfg(macinrender_output)]
        if self.runtime.is_some()
            && self.settings.mode.resolved() == SpatialBackendKind::SafBinaural
            && !self.settings.sofa.is_empty()
            && matches!(
                self.snapshot.phase,
                OutputPhase::Ready
                    | OutputPhase::Playing
                    | OutputPhase::Paused
                    | OutputPhase::Ended
            )
        {
            // Hot switches replace settings only after native acknowledgement;
            // on failure the previous SOFA remains the active one.
            return Some(&self.settings.sofa);
        }
        None
    }
    /// Hand the current output the compensation the settings ask for, once.
    ///
    /// Called wherever settings or the output itself change, because a freshly
    /// built output starts with none: the renderer carries a profile across its
    /// own backend and device switches, but not across an output we replaced.
    #[cfg(macinrender_output)]
    fn sync_hptf(&mut self) {
        let Some(runtime) = &self.runtime else {
            return;
        };
        let wanted = super::HptfRequest {
            settings: self.settings.hptf(),
            adjustment: self.settings.hptf_adjustment(),
        };
        if self.hptf_pending.is_some()
            || self
                .hptf_accepted
                .as_ref()
                .is_some_and(|(accepted, _)| accepted == &wanted)
            || (self.hptf_accepted.is_none() && wanted.settings.profile.is_empty())
        {
            return;
        }
        self.hptf_revision += 1;
        runtime.set_hptf(wanted.clone(), self.hptf_revision);
        self.hptf_pending = Some((wanted, self.hptf_revision));
    }
    /// The profile the current output is actually running, on the same terms as
    /// [`Self::active_sofa`]: selected is not applied, and a swap only counts
    /// once the audio callback has taken up that revision.
    #[cfg_attr(
        not(macinrender_output),
        allow(
            clippy::unused_self,
            reason = "no native headphone feed exists in inspection builds"
        )
    )]
    pub fn active_hptf(&self) -> Option<&str> {
        #[cfg(macinrender_output)]
        if let Some(runtime) = &self.runtime
            && self.settings.hptf_applicable()
            && let Some((accepted, revision)) = &self.hptf_accepted
        {
            let status = runtime.hptf_status();
            if status.enabled && status.applied_revision == *revision {
                return Some(&accepted.settings.profile);
            }
        }
        None
    }
    /// The adjustment the running cascade was built with, on the same terms as
    /// [`Self::active_hptf`].
    ///
    /// A knob is a setting like any other, so it takes a round trip to reach the
    /// audio. Without this the panel would call a curve applied the instant a
    /// slider moved, and compare it against a readout still describing the
    /// cascade before it.
    #[cfg_attr(
        not(macinrender_output),
        allow(
            clippy::unused_self,
            reason = "no native headphone feed exists in inspection builds"
        )
    )]
    pub fn active_hptf_adjustment(&self) -> crate::hptf_profile::Adjustment {
        #[cfg(macinrender_output)]
        if let Some(runtime) = &self.runtime
            && let Some((accepted, revision)) = &self.hptf_accepted
        {
            let status = runtime.hptf_status();
            if status.enabled && status.applied_revision == *revision {
                return accepted.adjustment;
            }
        }
        crate::hptf_profile::Adjustment::default()
    }
    #[cfg_attr(
        not(macinrender_output),
        allow(
            clippy::unused_self,
            reason = "no native headphone feed exists in inspection builds"
        )
    )]
    pub fn hptf_readout(&self) -> HptfReadout {
        #[cfg(macinrender_output)]
        if let Some(runtime) = &self.runtime {
            let status = runtime.hptf_status();
            return HptfReadout {
                enabled: status.enabled,
                bands: status.bands,
                rate: status.rate,
                preamp_db: status.preamp_db,
                auto_trim_db: status.auto_trim_db,
                max_response_db: status.max_response_db,
            };
        }
        HptfReadout::default()
    }
    pub fn take_hptf_error(&mut self) -> Option<String> {
        self.hptf_error.take()
    }
    pub fn install_settings(&mut self, settings: OutputSettings) {
        self.settings = settings.validated();
        self.legacy
            .set_preferred_device(self.settings.native_device.clone());
        #[cfg(macinrender_output)]
        self.sync_hptf();
        self.configure_head();
        #[cfg(all(target_os = "macos", macinrender_output))]
        self.poll_atmos();
    }
    /// A same-format renderer change is committed only after native preparation succeeds.
    pub fn hot_settings(&mut self, settings: OutputSettings) {
        if self.settings_pending() {
            self.update_result = Some(Err("A renderer update is already being prepared".into()));
            return;
        }
        #[cfg(macinrender_output)]
        if self.uses_macinrender()
            && self.config.is_none()
            && self.settings.needs_rebuild(&settings)
        {
            // A retained, paused session is not a same-format target once its
            // output mode/layout/device changes between tracks.
            self.reset();
        }
        #[cfg(macinrender_output)]
        if self.uses_macinrender()
            && self.settings.renderer() != settings.renderer()
            && let Some(runtime) = &self.runtime
        {
            runtime.switch(settings.renderer());
            self.pending_hot = Some(settings);
            return;
        }
        self.install_settings(settings);
        self.update_result = Some(Ok(()));
    }
    pub fn take_settings_result(&mut self) -> Option<Result<(), String>> {
        self.update_result.take()
    }
    /// Prepare a paused output without borrowing or consuming the Scene FIFO.
    #[cfg(macinrender_output)]
    pub fn prepare_settings(
        &mut self,
        settings: OutputSettings,
        sample_rate: u32,
        request: u64,
        epoch: u64,
    ) -> Result<(), String> {
        if self.settings_pending() {
            return Err("An audio settings change is already being prepared".into());
        }
        let device = match settings.mode.resolved() {
            SpatialBackendKind::SafBinaural => match &settings.stereo_device {
                OutputDeviceSelection::EndpointId(id)
                    if self.pcm_devices.iter().any(|device| &device.id == id) =>
                {
                    settings.stereo_device.clone()
                }
                _ => OutputDeviceSelection::SystemDefault,
            },
            SpatialBackendKind::SystemSpatial => {
                if cfg!(target_os = "windows")
                    && self.legacy.device_catalog_ready
                    && !self.legacy.devices.iter().any(|device| {
                        device.is_default
                            && device
                                .max_dynamic_objects
                                .is_some_and(|count| count >= settings.layout.dynamic_budget())
                    })
                {
                    return Err(
                        "The default endpoint cannot host the selected speaker layout".into(),
                    );
                }
                OutputDeviceSelection::SystemDefault
            }
            _ => return Err("This output mode does not support renderer preparation".into()),
        };
        let (send, result) = mpsc::channel();
        std::thread::Builder::new()
            .name("prepare-audio-output".into())
            .spawn(move || {
                let prepared =
                    super::macinrender::PreparedSession::new(&settings, sample_rate, &device)
                        .map(|session| PreparedSettings { settings, session });
                let _ = send.send(prepared);
            })
            .map_err(|error| error.to_string())?;
        self.preparation = Some(Preparation {
            key: (request, epoch),
            result,
        });
        Ok(())
    }
    #[cfg(macinrender_output)]
    pub fn take_prepared_settings(
        &mut self,
        request: u64,
        epoch: u64,
    ) -> Option<Result<PreparedSettings, String>> {
        let preparation = self.preparation.as_ref()?;
        if preparation.key != (request, epoch) {
            self.preparation = None;
            return None;
        }
        let result = match preparation.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err("Audio output preparation stopped unexpectedly".into())
            }
        };
        self.preparation = None;
        Some(result)
    }
    #[cfg(macinrender_output)]
    pub fn install_prepared_settings(&mut self, prepared: PreparedSettings) {
        self.reset();
        self.install_settings(prepared.settings);
        self.prepared_session = Some(prepared.session);
    }
    #[cfg_attr(
        not(macinrender_output),
        allow(
            clippy::unused_self,
            reason = "same controller API with the optional renderer disabled"
        )
    )]
    pub fn settings_pending(&self) -> bool {
        #[cfg(macinrender_output)]
        {
            self.pending_hot.is_some() || self.preparation.is_some() || self.hptf_pending.is_some()
        }
        #[cfg(not(macinrender_output))]
        {
            false
        }
    }
    fn configure_head(&self) {
        #[cfg(posebridge_input)]
        self.head.configure_bridge(&self.settings.posebridge);
        let mode = self.settings.mode.resolved();
        self.head.configure(
            self.settings.head_source,
            matches!(
                mode,
                SpatialBackendKind::SafBinaural | SpatialBackendKind::WindowsSpatialAudio
            ),
            mode == SpatialBackendKind::SystemSpatial,
        );
    }
    #[cfg(posebridge_input)]
    pub fn posebridge(&self) -> &crate::posebridge::service::Service {
        &self.head.bridge
    }
    pub fn head_snapshot(&self) -> HeadSnapshot {
        self.head.snapshot()
    }
    pub fn manual_head(&mut self, pose: [f32; 3]) {
        self.settings.head_source = crate::head_tracking::HeadSource::Manual;
        self.head.manual(pose);
        self.configure_head();
    }
    pub fn recenter_head(&self) {
        self.head.recenter();
    }
    pub fn ensure_configured(&mut self, config: &OutputStreamConfig, reader: SceneQueueReader) {
        #[cfg(macinrender_output)]
        if self.uses_macinrender() {
            if self.config.as_ref() == Some(config) {
                return;
            }
            if self.prepared_session.is_none()
                && let Some(runtime) = &self.runtime
                && runtime.replace_source(config, &self.settings, reader.clone())
            {
                self.config = Some(config.clone());
                self.snapshot = runtime.snapshot();
                self.revision += 1;
                return;
            }
            #[cfg(target_os = "macos")]
            self.atmos.reset();
            self.legacy.reset();
            self.runtime = None;
            self.hptf_accepted = None;
            self.hptf_pending = None;
            self.hptf_error = None;
            self.head.set_target(None);
            match super::macinrender::Runtime::spawn_prepared(
                config.clone(),
                self.settings.clone(),
                reader,
                Arc::clone(&self.legacy.scene_view),
                self.playing,
                self.gain,
                self.prepared_session.take(),
            ) {
                Ok(runtime) => {
                    if self.settings.mode.resolved() == SpatialBackendKind::SafBinaural {
                        self.head.set_target(Some(runtime.control_slot()));
                    }
                    self.config = Some(config.clone());
                    self.snapshot = runtime.snapshot();
                    self.runtime = Some(runtime);
                }
                Err(error) => {
                    self.snapshot.phase = OutputPhase::Failed;
                    self.snapshot.error = Some(error);
                }
            }
            self.sync_hptf();
            self.revision += 1;
            self.configure_head();
            return;
        }
        self.legacy.ensure_configured(config, reader);
        self.configure_head();
    }
    /// Pause an outgoing track without discarding an expensive prepared HRTF.
    /// Ready metadata will decide whether the next source can reuse the device.
    pub fn suspend_for_source_change(&mut self) {
        self.pause();
        #[cfg(macinrender_output)]
        if self.uses_macinrender() {
            self.preparation = None;
            self.prepared_session = None;
            if let Some(runtime) = &self.runtime {
                runtime.hold_source();
            }
            self.config = None;
            self.snapshot = OutputSnapshot::idle();
            self.revision += 1;
            return;
        }
        self.reset();
    }

    pub fn reset(&mut self) {
        #[cfg(all(target_os = "macos", macinrender_output))]
        self.atmos.reset();
        #[cfg(macinrender_output)]
        {
            self.head.set_target(None);
            self.runtime = None;
            self.hptf_accepted = None;
            self.hptf_pending = None;
            self.hptf_error = None;
            self.config = None;
            self.pending_hot = None;
            self.preparation = None;
            self.prepared_session = None;
        }
        self.legacy.reset();
        self.snapshot = OutputSnapshot::idle();
        self.revision += 1;
    }
    #[cfg(macinrender_output)]
    fn recover_source_reset(&mut self) {
        // A native reset failure gets one fresh session. Failure while
        // creating that session follows the ordinary error path, so this
        // cannot turn an unsupported source into a rebuild loop.
        if let Some((config, reader)) = self
            .runtime
            .as_ref()
            .and_then(super::macinrender::Runtime::take_reset_failure)
            && self.config.as_ref() == Some(&config)
        {
            self.runtime = None;
            self.config = None;
            self.ensure_configured(&config, reader);
        }
    }

    pub fn poll(&mut self) {
        self.legacy.poll();
        #[cfg(macinrender_output)]
        {
            self.recover_source_reset();
            if let Some(result) = self.catalog.poll() {
                self.pcm_ready = true;
                match result {
                    Ok(devices) => {
                        self.pcm_devices = devices;
                        self.pcm_error = None;
                    }
                    Err(error) => self.pcm_error = Some(error),
                }
            }
            if let Some(result) = self
                .runtime
                .as_ref()
                .and_then(super::macinrender::Runtime::take_switch_result)
            {
                if let Some(settings) = self.pending_hot.take()
                    && result.is_ok()
                {
                    self.install_settings(settings);
                }
                self.update_result = Some(result);
            }
            if let Some((revision, result)) = self
                .runtime
                .as_ref()
                .and_then(super::macinrender::Runtime::take_hptf_result)
                && self
                    .hptf_pending
                    .as_ref()
                    .is_some_and(|(_, pending)| *pending == revision)
            {
                let (wanted, _) = self.hptf_pending.take().expect("matched request");
                self.hptf_error = match result {
                    Ok(true) => {
                        self.hptf_accepted = Some((wanted.clone(), revision));
                        None
                    }
                    Ok(false) => Some(
                        "This output has no headphone feed for a profile to apply to".to_owned(),
                    ),
                    Err(error) => Some(error),
                };
                if self.hptf_error.is_some()
                    && self.settings.hptf() == wanted.settings
                    && self.settings.hptf_adjustment() == wanted.adjustment
                {
                    // Put the knobs back with the profile: a rejected cascade is
                    // rejected as a whole, and leaving a knob where it caused the
                    // refusal would make the next sync try the same thing again.
                    let previous = self.hptf_accepted.as_ref().map(|(request, _)| request);
                    self.settings.hptf = previous
                        .map_or_else(String::new, |request| request.settings.profile.clone());
                    self.settings.hptf_auto_trim =
                        previous.is_some_and(|request| request.settings.auto_trim);
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "the knobs are stored as the widget's f32 and \
                                  only widened to design the bands"
                    )]
                    {
                        self.settings.hptf_bass_db =
                            previous.map_or(0.0, |request| request.adjustment.bass_db as f32);
                        self.settings.hptf_tilt_db = previous
                            .map_or(0.0, |request| request.adjustment.tilt_db_per_octave as f32);
                    }
                }
                // A newer desired setting may have arrived while this request ran.
                self.sync_hptf();
                self.revision += 1;
            }
        }
        let snapshot = if self.uses_macinrender() {
            #[cfg(macinrender_output)]
            {
                self.runtime.as_ref().map_or_else(
                    || self.snapshot.clone(),
                    super::macinrender::Runtime::snapshot,
                )
            }
            #[cfg(not(macinrender_output))]
            {
                self.snapshot.clone()
            }
        } else {
            self.legacy.snapshot().clone()
        };
        #[cfg(all(target_os = "macos", macinrender_output))]
        let snapshot = {
            let mut snapshot = snapshot;
            snapshot
                .atmos_assist
                .clone_from(&self.snapshot.atmos_assist);
            snapshot
        };
        if snapshot != self.snapshot {
            self.snapshot = snapshot;
            self.revision += 1;
        }
        #[cfg(all(target_os = "macos", macinrender_output))]
        self.poll_atmos();
    }
    #[cfg(all(target_os = "macos", macinrender_output))]
    fn poll_atmos(&mut self) {
        let eligible = self.settings.atmos_label_assist
            && self.settings.atmos_label_applicable()
            && !self.snapshot.preview;
        let status = self.atmos.update(
            eligible,
            self.playing,
            self.snapshot.phase,
            self.snapshot.playhead_frames,
        );
        if self.snapshot.atmos_assist.as_ref() != Some(&status) {
            self.snapshot.atmos_assist = Some(status);
            self.revision += 1;
        }
    }
    pub fn advance_preview(&mut self, playing: bool, now: Instant) {
        if !self.uses_macinrender() {
            self.legacy.advance_preview(playing, now);
            self.poll();
        }
    }
    pub fn play(&mut self) {
        self.playing = true;
        #[cfg(macinrender_output)]
        if let Some(runtime) = &self.runtime {
            runtime.play(true);
            return;
        }
        self.legacy.play();
    }
    pub fn pause(&mut self) {
        self.playing = false;
        #[cfg(all(target_os = "macos", macinrender_output))]
        self.atmos.pause();
        #[cfg(macinrender_output)]
        if let Some(runtime) = &self.runtime {
            runtime.play(false);
            return;
        }
        self.legacy.pause();
    }
    pub fn set_master_gain(&mut self, gain: f32) {
        self.gain = if gain.is_finite() {
            gain.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.legacy.set_master_gain(self.gain);
        #[cfg(macinrender_output)]
        if let Some(runtime) = &self.runtime {
            runtime.volume(self.gain);
        }
    }
    pub const fn snapshot(&self) -> &OutputSnapshot {
        &self.snapshot
    }
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub fn scene_view(&self) -> &SceneViewMirror {
        self.legacy.scene_view()
    }
    pub fn is_configured_for_playback(&self, request: u64, epoch: u64) -> bool {
        #[cfg(macinrender_output)]
        if self.uses_macinrender() {
            return self
                .config
                .as_ref()
                .is_some_and(|c| c.request_id == request && c.playback_epoch == epoch);
        }
        self.legacy.is_configured_for_playback(request, epoch)
    }
    pub fn preferred_device(&self) -> &OutputDeviceSelection {
        match self.settings.mode.resolved() {
            SpatialBackendKind::SafBinaural => &self.settings.stereo_device,
            SpatialBackendKind::SystemSpatial => &self.default_device,
            _ => &self.settings.native_device,
        }
    }
    pub fn devices(&self) -> &[OutputDeviceInfo] {
        #[cfg(macinrender_output)]
        if self.settings.mode.resolved() == SpatialBackendKind::SafBinaural {
            return &self.pcm_devices;
        }
        self.legacy.devices()
    }
    pub fn device_catalog_error(&self) -> Option<&str> {
        #[cfg(macinrender_output)]
        if self.settings.mode.resolved() == SpatialBackendKind::SafBinaural {
            return self.pcm_error.as_deref();
        }
        self.legacy.device_catalog_error()
    }
    pub fn device_catalog_ready(&self) -> bool {
        #[cfg(macinrender_output)]
        if self.settings.mode.resolved() == SpatialBackendKind::SafBinaural {
            return self.pcm_ready;
        }
        self.legacy.device_catalog_ready()
    }
    pub fn required_dynamic_objects(&self, source_count: usize) -> Option<u32> {
        match self.settings.mode.resolved() {
            SpatialBackendKind::SafBinaural => None,
            SpatialBackendKind::SystemSpatial => {
                cfg!(target_os = "windows").then(|| self.settings.layout.dynamic_budget())
            }
            _ => Some(u32::try_from(source_count).unwrap_or(u32::MAX)),
        }
    }
    pub fn resolved_device(&self, source_count: usize) -> Option<OutputDeviceSelection> {
        match self.settings.mode.resolved() {
            SpatialBackendKind::SystemSpatial if cfg!(macinrender_output) => {
                if cfg!(target_os = "windows")
                    && self.legacy.device_catalog_ready
                    && !self.legacy.devices.iter().any(|d| {
                        d.is_default
                            && d.max_dynamic_objects
                                .is_some_and(|n| n >= self.settings.layout.dynamic_budget())
                    })
                {
                    None
                } else {
                    Some(OutputDeviceSelection::SystemDefault)
                }
            }
            SpatialBackendKind::SafBinaural if cfg!(macinrender_output) => {
                if let OutputDeviceSelection::EndpointId(id) = &self.settings.stereo_device
                    && self.devices().iter().any(|d| &d.id == id)
                {
                    Some(self.settings.stereo_device.clone())
                } else {
                    Some(OutputDeviceSelection::SystemDefault)
                }
            }
            _ => self.legacy.resolved_device(source_count),
        }
    }
    pub fn configured_device(&self) -> Option<&OutputDeviceSelection> {
        #[cfg(macinrender_output)]
        if self.uses_macinrender() {
            return self.config.as_ref().map(|c| &c.output_device);
        }
        self.legacy.configured_device()
    }
}
impl Default for SpatialOutputController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, macinrender_output))]
mod tests {
    use super::*;
    #[test]
    fn failed_hptf_switch_preserves_the_active_profile_and_allows_retry() {
        let directory = tempfile::tempdir().unwrap();
        let good = directory.path().join("good.txt");
        let bad = directory.path().join("bad.txt");
        std::fs::write(&good, b"Preamp: -3 dB\n").unwrap();
        std::fs::write(&bad, b"not a profile\n").unwrap();
        let key = PlaybackKey::new(41, 1);
        let (queue, reader) = scene_queue_pair(key);
        let signature = SceneSignature::from_block(&tone(0));
        for i in 0..90 {
            queue.try_push(key, tone(i * 1024)).unwrap();
        }
        queue.mark_end_of_stream(key);
        let mut output = SpatialOutputController::new();
        output.install_settings(OutputSettings {
            null_output: true,
            mode: SpatialBackendKind::SafBinaural,
            ..Default::default()
        });
        output.playing = true;
        output.ensure_configured(&output_config(signature), reader);
        let mut settings = output.settings().clone();
        settings.hptf = good.to_str().unwrap().to_owned();
        output.hot_settings(settings.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        while output.active_hptf().is_none() {
            output.poll();
            assert!(
                Instant::now() < deadline,
                "good profile did not activate: {:?}",
                output.take_hptf_error()
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(output.active_hptf(), good.to_str());
        settings.hptf = bad.to_str().unwrap().to_owned();
        output.hot_settings(settings);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            output.poll();
            if let Some(error) = output.take_hptf_error() {
                assert!(error.contains("HpTF"));
                break;
            }
            assert!(Instant::now() < deadline, "bad profile did not fail");
            thread::sleep(Duration::from_millis(5));
        }
        let readout = output.hptf_readout();
        assert!(readout.enabled);
        assert!((readout.preamp_db + 3.0).abs() < f32::EPSILON);
        assert_eq!(
            output.active_hptf(),
            good.to_str(),
            "old EQ is still audible but has lost its active identity"
        );
        assert_eq!(output.settings().hptf, good.to_str().unwrap());
        assert!(!output.settings_pending());
        // Correct the rejected file and choose it again: failure must not
        // suppress a retry through the idempotence guard.
        std::fs::write(&bad, b"Preamp: -6 dB\n").unwrap();
        let mut settings = output.settings().clone();
        settings.hptf = bad.to_str().unwrap().to_owned();
        output.hot_settings(settings);
        let deadline = Instant::now() + Duration::from_secs(5);
        while output.active_hptf() != bad.to_str() {
            output.poll();
            assert!(
                Instant::now() < deadline,
                "corrected profile did not activate"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!((output.hptf_readout().preamp_db + 6.0).abs() < f32::EPSILON);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn auxiliary_failure_does_not_fail_or_rewind_primary_playback() {
        let mut output = SpatialOutputController::new();
        output.settings.mode = SpatialBackendKind::SystemSpatial;
        output.playing = true;
        output.snapshot.phase = OutputPhase::Playing;
        output.snapshot.playhead_frames = 48_000;
        output.atmos =
            super::super::atmos::AtmosController::failed_for_test("injected tap failure");
        output.poll_atmos();
        assert_eq!(output.snapshot.phase, OutputPhase::Playing);
        assert_eq!(output.snapshot.playhead_frames, 48_000);
        assert!(output.snapshot.error.is_none());
        assert!(
            output
                .snapshot
                .atmos_assist_status()
                .unwrap()
                .contains("injected tap failure")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn leaving_714_disables_assist_and_returning_waits_for_fresh_pcm() {
        use super::super::SpeakerLayout;

        for mode in [
            SpatialBackendKind::Automatic,
            SpatialBackendKind::SystemSpatial,
        ] {
            for layout in [SpeakerLayout::NineOneSix, SpeakerLayout::TwentyTwoTwo] {
                let mut output = SpatialOutputController::new();
                output.settings.mode = mode;
                output.playing = true;
                output.snapshot.phase = OutputPhase::Playing;
                output.snapshot.playhead_frames = 48_000;
                output.atmos =
                    super::super::atmos::AtmosController::failed_for_test("injected tap failure");
                output.poll_atmos();
                assert!(
                    output
                        .snapshot
                        .atmos_assist_status()
                        .unwrap()
                        .contains("injected tap failure")
                );

                let mut settings = output.settings().clone();
                settings.layout = layout;
                output.install_settings(settings.clone());
                assert_eq!(
                    output.snapshot.atmos_assist_status(),
                    Some("Inactive for current settings")
                );
                assert!(output.settings().atmos_label_assist);

                settings.layout = SpeakerLayout::SevenOneFour;
                output.install_settings(settings);
                assert_eq!(
                    output.snapshot.atmos_assist_status(),
                    Some("Waiting for PCM presentation")
                );
                assert!(output.settings().atmos_label_assist);
                assert_eq!(output.snapshot.phase, OutputPhase::Playing);
                assert_eq!(output.snapshot.playhead_frames, 48_000);
                assert!(output.snapshot.error.is_none());
            }
        }
    }
    use crate::decoder::{
        DecodedSceneBlock, PlaybackKey, SceneObjectPcm, SceneSignature, SpatialObjectState,
        SpatialPosition, scene_queue_pair,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    struct Feeder {
        stop: Arc<AtomicBool>,
        join: Option<thread::JoinHandle<()>>,
    }
    impl Drop for Feeder {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(join) = self.join.take() {
                join.join().unwrap();
            }
        }
    }

    fn tone(start: i64) -> DecodedSceneBlock {
        DecodedSceneBlock::new(
            48_000,
            start,
            1024,
            1,
            0,
            None,
            true,
            vec![SceneObjectPcm::new(
                7,
                Some(SpatialObjectState::new(
                    true,
                    Some(SpatialPosition::new(0.0, 1.0, 0.0)),
                    Some(1.0),
                    true,
                )),
                vec![0.01; 1024],
            )],
            None,
            Vec::new(),
        )
    }

    fn output_config(signature: SceneSignature) -> OutputStreamConfig {
        OutputStreamConfig::new(
            41,
            1,
            0,
            48_000,
            signature,
            OutputDeviceSelection::SystemDefault,
        )
        .unwrap()
    }

    #[test]
    fn selected_sofa_is_not_active_without_a_native_runtime() {
        let mut output = SpatialOutputController::new();
        output.hot_settings(OutputSettings {
            mode: SpatialBackendKind::SafBinaural,
            sofa: "selected-before-playback.sofa".into(),
            ..Default::default()
        });
        assert_eq!(output.take_settings_result(), Some(Ok(())));
        assert!(output.active_sofa().is_none());
        // A ready legacy/preview snapshot cannot prove a native HRTF was loaded.
        output.snapshot.phase = OutputPhase::Ready;
        assert!(output.active_sofa().is_none());
    }

    #[test]
    #[ignore = "requires MACINDECODE_AC4_TEST_SOFA; checks real native HRTF activation"]
    fn active_sofa_follows_successful_native_loading_and_preserves_failed_switches() {
        let path = std::env::var("MACINDECODE_AC4_TEST_SOFA").expect("set SOFA path");
        let key = PlaybackKey::new(41, 1);
        let (queue, reader) = scene_queue_pair(key);
        let signature = SceneSignature::from_block(&tone(0));
        queue.try_push(key, tone(0)).unwrap();
        queue.mark_end_of_stream(key);
        let settings = OutputSettings {
            null_output: true,
            mode: SpatialBackendKind::SafBinaural,
            sofa: path.clone(),
            ..Default::default()
        };
        let mut output = SpatialOutputController::new();
        output.hot_settings(settings.clone());
        assert_eq!(output.take_settings_result(), Some(Ok(())));
        assert!(output.active_sofa().is_none());
        output.ensure_configured(&output_config(signature), reader);
        let deadline = Instant::now() + Duration::from_secs(30);
        while output.active_sofa().is_none() {
            output.poll();
            assert_ne!(
                output.snapshot().phase(),
                OutputPhase::Failed,
                "{:?}",
                output.snapshot().error()
            );
            assert!(
                Instant::now() < deadline,
                "native HRTF did not become active"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(output.active_sofa(), Some(path.as_str()));

        let wait_for_switch = |output: &mut SpatialOutputController| {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                output.poll();
                if let Some(result) = output.take_settings_result() {
                    break result;
                }
                assert!(Instant::now() < deadline, "native HRTF switch timed out");
                thread::sleep(Duration::from_millis(5));
            }
        };
        let directory = tempfile::tempdir().unwrap();
        let invalid = directory.path().join("invalid.sofa");
        std::fs::write(&invalid, b"not a SOFA file").unwrap();
        output.hot_settings(OutputSettings {
            sofa: invalid.to_str().unwrap().into(),
            ..settings.clone()
        });
        assert!(output.settings_pending());
        assert_eq!(output.active_sofa(), Some(path.as_str()));
        assert!(wait_for_switch(&mut output).is_err());
        assert_eq!(output.active_sofa(), Some(path.as_str()));
        assert_eq!(output.settings(), &settings);

        // Switching to the built-in HRTF must remove the file's active status.
        output.hot_settings(OutputSettings {
            sofa: String::new(),
            ..settings.clone()
        });
        wait_for_switch(&mut output).unwrap();
        assert!(output.active_sofa().is_none());
        output.hot_settings(settings);
        wait_for_switch(&mut output).unwrap();
        assert_eq!(output.active_sofa(), Some(path.as_str()));
        output.reset();
        assert!(output.active_sofa().is_none());
    }

    fn advance_real_media(
        decoder: &mut crate::decoder::DecoderController,
        output: &mut SpatialOutputController,
    ) -> Duration {
        use crate::decoder::DecodePhase;

        let start = Instant::now();
        loop {
            decoder.poll();
            assert_ne!(
                decoder.snapshot().phase(),
                DecodePhase::Failed,
                "{:?}",
                decoder.snapshot()
            );
            if matches!(
                decoder.snapshot().phase(),
                DecodePhase::Ready | DecodePhase::EndOfStream
            ) {
                let metrics = decoder.snapshot().metrics().unwrap();
                let config = OutputStreamConfig::new(
                    decoder.request_id(),
                    decoder.playback_epoch(),
                    metrics.target_frame(),
                    metrics.sample_rate(),
                    metrics.scene_signature().unwrap().clone(),
                    OutputDeviceSelection::SystemDefault,
                )
                .unwrap();
                output.ensure_configured(&config, decoder.scene_reader());
                output.play();
                output.poll();
                assert_ne!(
                    output.snapshot().phase(),
                    OutputPhase::Failed,
                    "{:?}",
                    output.snapshot().error()
                );
                if output.snapshot().playhead_frames() > metrics.target_frame() {
                    return start.elapsed();
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(90),
                "real media startup timed out"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    #[ignore = "requires MACINDECODE_AC4_TEST_MEDIA and MACINDECODE_AC4_TEST_SOFA; silent real-media latency check"]
    fn real_media_seek_replay_and_track_change_keep_the_prepared_hrtf() {
        use crate::decoder::DecoderController;
        let media = std::env::var("MACINDECODE_AC4_TEST_MEDIA").unwrap();
        let sofa = std::env::var("MACINDECODE_AC4_TEST_SOFA").unwrap();
        for profile in [String::new(), sofa] {
            let mut decoder = DecoderController::new();
            let mut output = SpatialOutputController::new();
            output.install_settings(OutputSettings {
                null_output: true,
                mode: SpatialBackendKind::SafBinaural,
                sofa: profile.clone(),
                head_source: crate::head_tracking::HeadSource::Manual,
                ..Default::default()
            });
            decoder.ensure_open(std::path::Path::new(&media));
            let first = advance_real_media(&mut decoder, &mut output);
            let control = output
                .runtime
                .as_ref()
                .unwrap()
                .control_slot()
                .lock()
                .unwrap()
                .clone()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(90);
            while decoder.snapshot().metrics().unwrap().is_indexing() {
                decoder.poll();
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(2));
            }
            let duration = decoder
                .snapshot()
                .metrics()
                .unwrap()
                .duration_frames()
                .unwrap();
            let mut times = Vec::new();
            for target in [duration / 4, 0, duration / 2, 0] {
                let epoch = control.status().unwrap().epoch;
                output.pause();
                decoder.seek(target).unwrap();
                times.push(advance_real_media(&mut decoder, &mut output));
                assert!(
                    control.status().unwrap().epoch > epoch,
                    "seek replaced native output"
                );
            }
            let epoch = control.status().unwrap().epoch;
            output.suspend_for_source_change();
            decoder.close();
            decoder.ensure_open(std::path::Path::new(&media));
            let track = advance_real_media(&mut decoder, &mut output);
            assert!(
                control.status().unwrap().epoch > epoch,
                "track change replaced native output"
            );
            eprintln!(
                "{}: first {first:?}; seeks/replays {times:?}; track change {track:?}",
                if profile.is_empty() { "KEMAR" } else { "SOFA" }
            );
        }
    }

    #[test]
    fn a_new_track_keeps_the_output_while_its_decoder_opens() {
        let mut output = SpatialOutputController::new();
        output.install_settings(OutputSettings {
            null_output: true,
            mode: SpatialBackendKind::SafBinaural,
            ..Default::default()
        });
        let key = PlaybackKey::new(41, 1);
        let (queue, reader) = scene_queue_pair(key);
        let frame = tone(0);
        let mut config = output_config(SceneSignature::from_block(&frame));
        queue.try_push(key, frame).unwrap();
        queue.mark_end_of_stream(key);
        output.ensure_configured(&config, reader);
        output.play();
        let deadline = Instant::now() + Duration::from_secs(5);
        while output.snapshot().phase() != OutputPhase::Ended {
            output.poll();
            assert_ne!(output.snapshot().phase(), OutputPhase::Failed);
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        let slot = output.runtime.as_ref().unwrap().control_slot();
        let control = slot.lock().unwrap().clone().unwrap();
        let old_epoch = control.status().unwrap().epoch;
        // Both activate_entry and the Opening ticks suspend the same output.
        for _ in 0..3 {
            output.suspend_for_source_change();
        }
        assert!(!output.is_configured_for_playback(41, 1));
        assert!(Arc::ptr_eq(
            &slot,
            &output.runtime.as_ref().unwrap().control_slot()
        ));
        let next = PlaybackKey::new(42, 1);
        let (queue, reader) = scene_queue_pair(next);
        queue.try_push(next, tone(0)).unwrap();
        queue.mark_end_of_stream(next);
        config.request_id = 42;
        output.ensure_configured(&config, reader);
        output.play();
        while output.snapshot().phase() != OutputPhase::Ended {
            output.poll();
            assert_ne!(output.snapshot().phase(), OutputPhase::Failed);
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(control.status().unwrap().epoch > old_epoch);
        assert!(output.is_configured_for_playback(42, 1));
        // Changing format while the next decoder opens must release the held
        // output, rather than requesting an incompatible same-format hot swap.
        output.suspend_for_source_change();
        let desired = OutputSettings {
            mode: SpatialBackendKind::SystemSpatial,
            ..output.settings().clone()
        };
        output.hot_settings(desired.clone());
        assert_eq!(output.take_settings_result(), Some(Ok(())));
        assert_eq!(output.settings(), &desired);
        assert!(output.runtime.is_none());
    }

    #[test]
    fn output_preparation_preserves_old_playback_and_hands_off_a_ready_session() {
        let key = PlaybackKey::new(41, 1);
        let (queue, reader) = scene_queue_pair(key);
        let signature = SceneSignature::from_block(&tone(0));
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&stop);
        let feed = thread::spawn(move || {
            let mut start = 0;
            while !done.load(Ordering::Relaxed) {
                if queue.try_push(key, tone(start)).is_ok() {
                    start += 1024;
                } else {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        });
        let feeder = Feeder {
            stop,
            join: Some(feed),
        };
        let mut output = SpatialOutputController::new();
        let original = OutputSettings {
            null_output: true,
            mode: SpatialBackendKind::SystemSpatial,
            ..Default::default()
        };
        output.install_settings(original.clone());
        let mut config = output_config(signature);
        output.ensure_configured(&config, reader);
        output.play();
        let deadline = Instant::now() + Duration::from_secs(15);
        while output.snapshot().playhead_frames() < 4800 {
            output.poll();
            assert_ne!(output.snapshot().phase(), OutputPhase::Failed);
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        let before = output.snapshot().playhead_frames();
        let desired = OutputSettings {
            mode: SpatialBackendKind::SafBinaural,
            ..original.clone()
        };
        let call = Instant::now();
        output
            .prepare_settings(desired.clone(), 48_000, 41, 1)
            .unwrap();
        assert!(call.elapsed() < Duration::from_millis(100));
        let hold_until = Instant::now() + Duration::from_millis(200);
        while Instant::now() < hold_until {
            output.poll();
            assert_eq!(output.settings(), &original);
            assert!(output.is_configured_for_playback(41, 1));
            thread::sleep(Duration::from_millis(5));
        }
        assert!(output.snapshot().playhead_frames() >= before + 4800);
        let prepared = loop {
            output.poll();
            if let Some(result) = output.take_prepared_settings(41, 1) {
                break result.unwrap();
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        };
        let target = output.snapshot().playhead_frames();
        drop(feeder);
        output.pause();
        output.install_prepared_settings(prepared);
        assert_eq!(output.settings(), &desired);
        let key = PlaybackKey::new(41, 2);
        let (queue, reader) = scene_queue_pair(key);
        for offset in (0..16_384).step_by(1024) {
            queue
                .try_push(key, tone(i64::try_from(target).unwrap() + offset))
                .unwrap();
        }
        queue.mark_end_of_stream(key);
        config.playback_epoch = 2;
        config.start_frame = target;
        let handoff = Instant::now();
        output.ensure_configured(&config, reader);
        output.play();
        while output.snapshot().playhead_frames() <= target {
            output.poll();
            assert_ne!(
                output.snapshot().phase(),
                OutputPhase::Failed,
                "{:?}",
                output.snapshot().error()
            );
            assert!(
                handoff.elapsed() < Duration::from_secs(1),
                "prepared output repeated HRTF initialization"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(output.is_configured_for_playback(41, 2));
    }

    #[test]
    fn failed_or_stale_preparation_cannot_replace_current_settings() {
        let mut output = SpatialOutputController::new();
        let original = output.settings().clone();
        let invalid = OutputSettings {
            null_output: true,
            mode: SpatialBackendKind::SafBinaural,
            sofa: "/missing/test-listener.sofa".into(),
            ..original.clone()
        };
        output
            .prepare_settings(invalid.clone(), 48_000, 9, 1)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = output.take_prepared_settings(9, 1) {
                assert!(result.is_err());
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(output.settings(), &original);
        output.prepare_settings(invalid, 48_000, 9, 1).unwrap();
        assert!(output.take_prepared_settings(9, 2).is_none());
        assert!(!output.settings_pending());
        assert_eq!(output.settings(), &original);
    }
}
