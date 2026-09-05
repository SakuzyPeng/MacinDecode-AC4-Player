#[cfg(macinrender_output)]
use std::sync::Arc;
use std::time::Instant;

#[cfg(macinrender_output)]
use super::OutputPhase;
use super::{
    NativeOutputController, OutputDeviceInfo, OutputDeviceSelection, OutputSettings,
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
    pub fn install_settings(&mut self, settings: OutputSettings) {
        self.settings = settings.validated();
        self.legacy
            .set_preferred_device(self.settings.native_device.clone());
        self.configure_head();
    }
    /// A same-format renderer change is committed only after native preparation succeeds.
    pub fn hot_settings(&mut self, settings: OutputSettings) {
        if self.settings_pending() {
            self.update_result = Some(Err("A renderer update is already being prepared".into()));
            return;
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
            self.pending_hot.is_some() || self.preparation.is_some()
        }
        #[cfg(not(macinrender_output))]
        {
            false
        }
    }
    fn configure_head(&self) {
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
            self.legacy.reset();
            self.runtime = None;
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
            self.revision += 1;
            self.configure_head();
            return;
        }
        self.legacy.ensure_configured(config, reader);
        self.configure_head();
    }
    pub fn reset(&mut self) {
        #[cfg(macinrender_output)]
        {
            self.head.set_target(None);
            self.runtime = None;
            self.config = None;
            self.pending_hot = None;
            self.preparation = None;
            self.prepared_session = None;
        }
        self.legacy.reset();
        self.snapshot = OutputSnapshot::idle();
        self.revision += 1;
    }
    pub fn poll(&mut self) {
        self.legacy.poll();
        #[cfg(macinrender_output)]
        {
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
        if snapshot != self.snapshot {
            self.snapshot = snapshot;
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
