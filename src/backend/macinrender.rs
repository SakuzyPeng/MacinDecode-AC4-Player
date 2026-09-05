use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::state::{element_state_at, listener_render_state, state_at_updates, validate_block};
use super::{
    OutputDeviceInfo, OutputDeviceSelection, OutputPhase, OutputSettings, OutputSnapshot,
    OutputStreamConfig, SpatialBackendKind,
};
use crate::decoder::{
    DecodedSceneBlock, SceneMetadataUpdate, SceneQueueReader, SceneSignature, SpatialObjectState,
};
use crate::head_tracking::NativeTarget;
use crate::scene_view::{ObjectView, SceneViewMirror};
use macindecode_macinrender as native;

const NATIVE_EPOCH: u64 = 1;
const HISTORY_BYTES: usize = 16 * 1024 * 1024;

struct Shared {
    stop: AtomicBool,
    playing: AtomicBool,
    volume: AtomicU32,
    snapshot: Mutex<OutputSnapshot>,
    switch: Mutex<Option<native::RendererSettings>>,
    switch_result: Mutex<Option<Result<(), String>>>,
    control: NativeTarget,
}
pub(super) struct Runtime {
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
}
impl Runtime {
    pub fn spawn(
        config: OutputStreamConfig,
        settings: OutputSettings,
        reader: SceneQueueReader,
        mirror: Arc<SceneViewMirror>,
        playing: bool,
        gain: f32,
    ) -> Result<Self, String> {
        let mut snapshot = OutputSnapshot::idle();
        snapshot.phase = OutputPhase::Initializing;
        snapshot.device_label = settings.mode.resolved().label().into();
        snapshot.playhead_frames = config.start_frame;
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            playing: AtomicBool::new(playing),
            volume: AtomicU32::new(gain.to_bits()),
            snapshot: Mutex::new(snapshot),
            switch: Mutex::new(None),
            switch_result: Mutex::new(None),
            control: Arc::new(Mutex::new(None)),
        });
        let worker = Arc::clone(&shared);
        let join = thread::Builder::new()
            .name("macinrender-scene-producer".into())
            .spawn(move || {
                if let Err(error) = run(&worker, &config, &settings, &reader, &mirror) {
                    let mut snapshot = worker
                        .snapshot
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    snapshot.phase = OutputPhase::Failed;
                    snapshot.error = Some(error);
                }
                *worker
                    .control
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            shared,
            join: Some(join),
        })
    }
    pub fn control_slot(&self) -> NativeTarget {
        Arc::clone(&self.shared.control)
    }
    pub fn snapshot(&self) -> OutputSnapshot {
        self.shared
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub fn play(&self, playing: bool) {
        self.shared.playing.store(playing, Ordering::Relaxed);
    }
    pub fn volume(&self, gain: f32) {
        self.shared.volume.store(gain.to_bits(), Ordering::Relaxed);
    }
    pub fn switch(&self, settings: native::RendererSettings) {
        *self
            .shared
            .switch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(settings);
    }
    pub fn take_switch_result(&self) -> Option<Result<(), String>> {
        self.shared
            .switch_result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct MetadataFrame {
    start: i64,
    duration: u32,
    complete: bool,
    objects: Vec<(u64, Option<SpatialObjectState>)>,
    updates: Vec<SceneMetadataUpdate>,
}
impl MetadataFrame {
    fn new(block: &DecodedSceneBlock) -> Self {
        let mut objects: Vec<_> = block
            .objects()
            .iter()
            .map(|object| (object.element_id(), object.initial_state()))
            .collect();
        objects.sort_unstable_by_key(|(id, _)| *id);
        Self {
            start: block.start_frame(),
            duration: block.duration_frames(),
            complete: block.state_complete(),
            objects,
            updates: block.metadata_updates().to_vec(),
        }
    }
    fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.objects.len() * size_of::<(u64, Option<SpatialObjectState>)>()
            + self.updates.len() * size_of::<SceneMetadataUpdate>()
    }
    fn end(&self) -> i64 {
        self.start.saturating_add(i64::from(self.duration))
    }
}

fn own_state(state: Option<SpatialObjectState>) -> native::ObjectState {
    state.map_or(
        native::ObjectState {
            active: false,
            gain: 0.0,
            position: None,
        },
        |state| native::ObjectState {
            active: state.metadata_active() && state.semantic_complete(),
            gain: state.linear_gain().unwrap_or(1.0),
            position: state.position().map(|p| [p.x(), p.y(), p.z()]),
        },
    )
}

fn renderer_fields(core: u32) -> u64 {
    u64::from(core & 1 != 0) | (u64::from(core & 2 != 0) << 1) | (u64::from(core & 8 != 0) << 2)
}

fn submit_block(
    session: &mut native::Session,
    block: &DecodedSceneBlock,
    offset: u32,
) -> Result<bool, String> {
    let from = usize::try_from(offset).map_err(|_| "Scene trim offset overflow")?;
    let mut planes: Vec<_> = block
        .objects()
        .iter()
        .map(|object| native::Plane {
            element: object.element_id(),
            samples: &object.samples()[from..],
        })
        .collect();
    let mut initial: Vec<_> = block
        .objects()
        .iter()
        .map(|object| {
            (
                object.element_id(),
                own_state(element_state_at(
                    block,
                    object.element_id(),
                    object.initial_state(),
                    offset,
                )),
            )
        })
        .collect();
    if let Some(lfe) = block.lfe() {
        planes.push(native::Plane {
            element: lfe.element_id(),
            samples: &lfe.samples()[from..],
        });
        initial.push((
            lfe.element_id(),
            own_state(element_state_at(
                block,
                lfe.element_id(),
                lfe.initial_state(),
                offset,
            )),
        ));
    }
    let mut updates = Vec::new();
    // A trimmed block can begin inside a ramp: synthesize its remaining target
    // at offset zero instead of freezing at the interpolated initial state.
    for (id, _) in &initial {
        if let Some(update) = block
            .metadata_updates()
            .iter()
            .rev()
            .find(|u| u.element_id() == *id && u.offset_frames() < offset)
            && update.offset_frames().saturating_add(update.ramp_frames()) > offset
        {
            updates.push(native::Update {
                element: *id,
                offset: 0,
                ramp: update.offset_frames().saturating_add(update.ramp_frames()) - offset,
                changed: renderer_fields(update.changed_fields()),
                state: own_state(Some(update.state())),
            });
        }
    }
    updates.extend(
        block
            .metadata_updates()
            .iter()
            .filter(|u| u.offset_frames() >= offset)
            .map(|u| native::Update {
                element: u.element_id(),
                offset: u.offset_frames() - offset,
                ramp: u.ramp_frames(),
                changed: renderer_fields(u.changed_fields()),
                state: own_state(Some(u.state())),
            }),
    );
    // Core can emit importance/zone-only changes, and LFE has no Cartesian
    // position. The renderer ABI requires a nonempty mask of fields actually
    // present in the target. End-boundary state is carried by the next block.
    for update in &mut updates {
        update.changed &= if update.state.position.is_some() {
            7
        } else {
            3
        };
    }
    updates
        .retain(|update| update.changed != 0 && update.offset < block.duration_frames() - offset);
    session.submit(&native::Frame {
        epoch: NATIVE_EPOCH,
        generation: u64::from(block.configuration_generation()),
        start: block.start_frame().saturating_add(i64::from(offset)),
        duration: block.duration_frames() - offset,
        complete: block.state_complete(),
        planes: &planes,
        initial: &initial,
        updates: &updates,
    })
}

fn submit_gap(
    session: &mut native::Session,
    signature: &SceneSignature,
    start: i64,
    duration: u32,
) -> Result<bool, String> {
    let ids: Vec<_> = signature
        .object_element_ids()
        .iter()
        .copied()
        .chain(signature.lfe_element_id())
        .collect();
    let planes: Vec<_> = ids
        .iter()
        .map(|&element| native::Plane {
            element,
            samples: &[],
        })
        .collect();
    let initial: Vec<_> = ids
        .iter()
        .map(|&id| {
            (
                id,
                native::ObjectState {
                    active: false,
                    gain: 0.0,
                    position: Some([0.0, 1.0, 0.0]),
                },
            )
        })
        .collect();
    session.submit(&native::Frame {
        epoch: NATIVE_EPOCH,
        generation: u64::from(signature.configuration_generation()),
        start,
        duration,
        complete: true,
        planes: &planes,
        initial: &initial,
        updates: &[],
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "one producer owns generation, frame retries, and presented metadata history"
)]
fn run(
    shared: &Arc<Shared>,
    config: &OutputStreamConfig,
    settings: &OutputSettings,
    reader: &SceneQueueReader,
    mirror: &SceneViewMirror,
) -> Result<(), String> {
    let kind = if settings.mode.resolved() == SpatialBackendKind::SafBinaural {
        native::OutputKind::Stereo
    } else {
        native::OutputKind::SystemSpatial
    };
    #[cfg(test)]
    let kind = if settings.null_output {
        native::OutputKind::Null
    } else {
        kind
    };
    let device_id = if kind == native::OutputKind::Stereo {
        match &config.output_device {
            OutputDeviceSelection::EndpointId(id) => id.clone(),
            OutputDeviceSelection::SystemDefault => String::new(),
        }
    } else {
        String::new()
    };
    let mut session = native::Session::new(&native::Config {
        renderer: settings.renderer(),
        output: kind,
        device_id,
        input_rate: config.sample_rate,
    })?;
    let target =
        i64::try_from(config.start_frame).map_err(|_| "Scene target exceeds signed time")?;
    session.reset(NATIVE_EPOCH, target)?;
    let control = session.control();
    *shared
        .control
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(control.clone());
    let mut signature = config.scene_signature.clone();
    session.configure(
        NATIVE_EPOCH,
        u64::from(signature.configuration_generation()),
        signature.object_element_ids(),
        signature.lfe_element_id(),
    )?;
    let mut pending = None::<DecodedSceneBlock>;
    let mut history = VecDeque::<MetadataFrame>::new();
    let mut history_bytes = 0;
    let mut next_sample = target;
    let mut first = true;
    let mut ended = false;
    let mut playing = false;
    let mut gain = f32::NAN;
    let mut last_view_time = target;
    let mut iterations = 0;
    while !shared.stop.load(Ordering::Relaxed) {
        let wanted_playing = shared.playing.load(Ordering::Relaxed);
        if wanted_playing != playing {
            control.play(wanted_playing)?;
            playing = wanted_playing;
        }
        let wanted_gain = f32::from_bits(shared.volume.load(Ordering::Relaxed));
        if wanted_gain.to_bits() != gain.to_bits() {
            control.volume(wanted_gain)?;
            gain = wanted_gain;
        }
        if let Some(settings) = shared
            .switch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let loader = control.clone();
            let reply = Arc::clone(shared);
            let spawned = thread::Builder::new()
                .name("hrtf-preparation".into())
                .spawn(move || {
                    let result = loader.switch_renderer(&settings);
                    *reply
                        .switch_result
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
                });
            if let Err(error) = spawned {
                *shared
                    .switch_result
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(Err(error.to_string()));
            }
        }
        let status = control.status()?;
        if status.phase == native::Phase::Failed {
            return Err("MacinRender rendering or output device failed".into());
        }
        let media_offset =
            u64::try_from(u128::from(status.presented) * u128::from(config.sample_rate) / 48_000)
                .unwrap_or(u64::MAX);
        let playhead = config.start_frame.saturating_add(media_offset);
        let playhead = if ended {
            playhead.min(u64::try_from(next_sample).unwrap_or(config.start_frame))
        } else {
            playhead
        };
        let view_time = i64::try_from(playhead).unwrap_or(i64::MAX);
        while history
            .front()
            .is_some_and(|frame| frame.end() <= view_time)
            && history.len() > 1
        {
            if let Some(old) = history.pop_front() {
                history_bytes -= old.bytes();
            }
        }
        if let Some(frame) = history.front().filter(|frame| frame.start <= view_time)
            && view_time != last_view_time
        {
            let offset = u32::try_from(view_time - frame.start)
                .unwrap_or(u32::MAX)
                .min(frame.duration.saturating_sub(1));
            let previous = u32::try_from(last_view_time.saturating_sub(frame.start).max(0))
                .unwrap_or(u32::MAX);
            mirror.write(
                reader.playback_key(),
                frame.objects.iter().map(|(id, initial)| {
                    let (active, position, gain) = listener_render_state(state_at_updates(
                        &frame.updates,
                        *id,
                        *initial,
                        offset,
                    ));
                    ObjectView {
                        element_id: *id,
                        active: active && frame.complete,
                        position,
                        gain,
                        jumped: frame.updates.iter().any(|u| {
                            u.element_id() == *id
                                && u.ramp_frames() == 0
                                && u.offset_frames() >= previous
                                && u.offset_frames() <= offset
                        }),
                    }
                }),
                view_time,
                config.sample_rate,
            );
            last_view_time = view_time;
        }
        {
            let mut snapshot = shared
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            snapshot.phase = if ended && next_sample <= target {
                OutputPhase::Ended
            } else {
                match status.phase {
                    native::Phase::Ended => OutputPhase::Ended,
                    native::Phase::Paused if first => OutputPhase::Ready,
                    native::Phase::Paused => OutputPhase::Paused,
                    _ if playing => OutputPhase::Playing,
                    _ => OutputPhase::Ready,
                }
            };
            snapshot.playhead_frames = playhead;
            snapshot.queued_output_frames = Some(status.queued);
            snapshot.clock = if status.media_clock {
                super::OutputClock::SystemMedia
            } else {
                super::OutputClock::Callback
            };
            snapshot.submitted_frames = status.consumed;
            snapshot.render_updates = iterations;
            if let Some(frame) = history.front().filter(|frame| frame.start <= view_time) {
                snapshot.reserved_dynamic_objects =
                    u32::try_from(frame.objects.len()).unwrap_or(u32::MAX);
                let offset = u32::try_from(view_time - frame.start)
                    .unwrap_or(u32::MAX)
                    .min(frame.duration.saturating_sub(1));
                snapshot.active_dynamic_objects = u32::try_from(
                    frame
                        .objects
                        .iter()
                        .filter(|(id, initial)| {
                            frame.complete
                                && listener_render_state(state_at_updates(
                                    &frame.updates,
                                    *id,
                                    *initial,
                                    offset,
                                ))
                                .0
                        })
                        .count(),
                )
                .unwrap_or(u32::MAX);
            }
            snapshot.underruns = status.underruns;
            snapshot.error = status
                .recovering
                .then(|| "System spatializer is reconnecting".into());
        }
        iterations += 1;
        if !ended && history_bytes < HISTORY_BYTES {
            if pending.is_none() {
                pending = reader.try_pop();
            }
            if let Some(block) = pending.as_ref() {
                let actual = SceneSignature::from_block(block);
                if actual.configuration_generation() != signature.configuration_generation() {
                    if block.sample_rate() != config.sample_rate {
                        return Err("Scene sample rate changed during playback".into());
                    }
                    session.configure(
                        NATIVE_EPOCH,
                        u64::from(actual.configuration_generation()),
                        actual.object_element_ids(),
                        actual.lfe_element_id(),
                    )?;
                    signature = actual;
                }
                validate_block(
                    block,
                    config.sample_rate,
                    u32::try_from(signature.object_element_ids().len()).unwrap_or(u32::MAX),
                    signature.lfe_element_id().is_some(),
                    &signature,
                )?;
                if first && block.start_frame() < target {
                    next_sample = block.start_frame();
                }
                if block.start_frame() > next_sample {
                    let duration = u32::try_from((block.start_frame() - next_sample).min(4096))
                        .unwrap_or(4096);
                    if submit_gap(&mut session, &signature, next_sample, duration)? {
                        next_sample += i64::from(duration);
                        first = false;
                    }
                } else {
                    let offset = u32::try_from(next_sample.saturating_sub(block.start_frame()))
                        .unwrap_or(u32::MAX);
                    if offset >= block.duration_frames() {
                        pending = None;
                    } else if submit_block(&mut session, block, offset)? {
                        let metadata = MetadataFrame::new(block);
                        history_bytes += metadata.bytes();
                        history.push_back(metadata);
                        next_sample = block
                            .start_frame()
                            .checked_add(i64::from(block.duration_frames()))
                            .ok_or("Scene time overflow")?;
                        pending = None;
                        first = false;
                    }
                }
            } else if reader.is_end_of_stream() {
                session.end(NATIVE_EPOCH, next_sample)?;
                ended = true;
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
    control.play(false)?;
    Ok(())
}

pub(super) struct DeviceCatalog {
    receive: std::sync::mpsc::Receiver<Result<Vec<OutputDeviceInfo>, String>>,
    stop: std::sync::mpsc::Sender<()>,
    join: Option<JoinHandle<()>>,
}
impl DeviceCatalog {
    pub fn spawn() -> Self {
        let (send, receive) = std::sync::mpsc::channel();
        let (stop, wait) = std::sync::mpsc::channel();
        let join = thread::Builder::new()
            .name("pcm-device-catalog".into())
            .spawn(move || {
                loop {
                    let result = native::output_devices().map(|devices| {
                        devices
                            .into_iter()
                            .map(|(id, label, is_default)| OutputDeviceInfo {
                                id,
                                label,
                                is_default,
                                max_dynamic_objects: None,
                                spatial_error: None,
                            })
                            .collect()
                    });
                    if send.send(result).is_err() {
                        break;
                    }
                    if !matches!(
                        wait.recv_timeout(Duration::from_secs(2)),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                    ) {
                        break;
                    }
                }
            })
            .ok();
        Self {
            receive,
            stop,
            join,
        }
    }
    pub fn poll(&self) -> Option<Result<Vec<OutputDeviceInfo>, String>> {
        self.receive.try_iter().last()
    }
}
impl Drop for DeviceCatalog {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::{PlaybackKey, SceneObjectPcm, SpatialPosition, scene_queue_pair};
    use std::time::Instant;

    fn block(start: i64, frames: u32) -> DecodedSceneBlock {
        DecodedSceneBlock::new(
            48_000,
            start,
            frames,
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
                vec![0.1; frames as usize],
            )],
            None,
            Vec::new(),
        )
    }

    #[test]
    fn native_output_preserves_preroll_overlap_gap_and_short_tail() {
        let key = PlaybackKey::new(1, 1);
        let (queue, reader) = scene_queue_pair(key);
        let first = block(-8, 28);
        let signature = SceneSignature::from_block(&first);
        for block in [first, block(16, 20), block(48, 33)] {
            queue.try_push(key, block).unwrap();
        }
        queue.mark_end_of_stream(key);
        let config = OutputStreamConfig::new(
            1,
            1,
            0,
            48_000,
            signature,
            OutputDeviceSelection::SystemDefault,
        )
        .unwrap();
        let settings = OutputSettings {
            null_output: true,
            mode: SpatialBackendKind::SystemSpatial,
            ..Default::default()
        };
        let mirror = Arc::new(SceneViewMirror::new());
        let runtime =
            Runtime::spawn(config, settings, reader, Arc::clone(&mirror), true, 1.0).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = runtime.snapshot();
            assert_ne!(status.phase, OutputPhase::Failed, "{:?}", status.error);
            if status.phase == OutputPhase::Ended {
                assert_eq!(status.playhead_frames, 81);
                assert_eq!(status.submitted_frames, 81);
                assert_eq!(mirror.read(key).unwrap().objects()[0].element_id, 7);
                break;
            }
            assert!(Instant::now() < deadline, "timed out: {status:?}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn reconfiguration_at_eos_remains_ended_even_when_paused() {
        let key = PlaybackKey::new(4, 2);
        let (queue, reader) = scene_queue_pair(key);
        queue.mark_end_of_stream(key);
        let signature = SceneSignature::new(1, 0, None, vec![7], None);
        let config = OutputStreamConfig::new(
            4,
            2,
            100,
            48_000,
            signature,
            OutputDeviceSelection::SystemDefault,
        )
        .unwrap();
        let settings = OutputSettings {
            null_output: true,
            mode: SpatialBackendKind::SystemSpatial,
            ..Default::default()
        };
        let runtime = Runtime::spawn(
            config,
            settings,
            reader,
            Arc::new(SceneViewMirror::new()),
            false,
            0.0,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = runtime.snapshot();
            assert_ne!(status.phase, OutputPhase::Failed, "{:?}", status.error);
            if status.phase == OutputPhase::Ended {
                assert_eq!(status.playhead_frames, 100);
                break;
            }
            assert!(Instant::now() < deadline, "empty epoch did not end");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "requires MACINDECODE_AC4_TEST_MEDIA; renders to a silent timed device"]
    fn decodes_real_media_through_all_render_modes() {
        use crate::decoder::{DecodePhase, DecoderController};
        let path = std::env::var_os("MACINDECODE_AC4_TEST_MEDIA").expect("set media path");
        let seconds = std::env::var("MACINDECODE_AC4_TEST_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| (1..=600).contains(value))
            .unwrap_or(1);
        for (mode, layout) in [
            (
                SpatialBackendKind::SystemSpatial,
                super::super::SpeakerLayout::SevenOneFour,
            ),
            (
                SpatialBackendKind::SystemSpatial,
                super::super::SpeakerLayout::NineOneSix,
            ),
            (
                SpatialBackendKind::SystemSpatial,
                super::super::SpeakerLayout::TwentyTwoTwo,
            ),
            (
                SpatialBackendKind::SafBinaural,
                super::super::SpeakerLayout::SevenOneFour,
            ),
        ] {
            let mut decoder = DecoderController::new();
            decoder.ensure_open(std::path::Path::new(&path));
            let deadline = Instant::now() + Duration::from_secs(seconds + 60);
            while !matches!(
                decoder.snapshot().phase(),
                DecodePhase::Ready | DecodePhase::EndOfStream
            ) {
                decoder.poll();
                assert_ne!(
                    decoder.snapshot().phase(),
                    DecodePhase::Failed,
                    "{:?}",
                    decoder.snapshot()
                );
                assert!(Instant::now() < deadline, "decode readiness timed out");
                thread::sleep(Duration::from_millis(10));
            }
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
            let settings = OutputSettings {
                null_output: std::env::var_os("MACINDECODE_AC4_TEST_SYSTEM_OUTPUT").is_none(),
                sofa: std::env::var("MACINDECODE_AC4_TEST_SOFA").unwrap_or_default(),
                mode,
                layout,
                ..Default::default()
            };
            let runtime = Runtime::spawn(
                config,
                settings,
                decoder.scene_reader(),
                Arc::new(SceneViewMirror::new()),
                true,
                0.0,
            )
            .unwrap();
            loop {
                decoder.poll();
                let status = runtime.snapshot();
                assert_ne!(status.phase, OutputPhase::Failed, "{:?}", status.error);
                if status.playhead_frames >= seconds * 48_000 || status.phase == OutputPhase::Ended
                {
                    println!(
                        "{} {}: {} media frames; {} underruns",
                        mode.label(),
                        layout.label(),
                        status.playhead_frames,
                        status.underruns
                    );
                    break;
                }
                assert!(Instant::now() < deadline, "render timed out: {status:?}");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[test]
    #[ignore = "requires MACINDECODE_AC4_TEST_MEDIA; checks MP4 padding and end-position reconfiguration"]
    fn real_media_ends_at_the_container_boundary_and_can_seek_there() {
        use crate::decoder::{DecodePhase, DecoderController};
        let path = std::env::var_os("MACINDECODE_AC4_TEST_MEDIA").expect("set media path");
        let mut decoder = DecoderController::new();
        decoder.ensure_open(std::path::Path::new(&path));
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            decoder.poll();
            assert_ne!(
                decoder.snapshot().phase(),
                DecodePhase::Failed,
                "{:?}",
                decoder.snapshot()
            );
            if decoder.snapshot().metrics().is_some_and(|metrics| {
                !metrics.is_indexing() && metrics.scene_signature().is_some()
            }) {
                break;
            }
            assert!(Instant::now() < deadline, "index timed out");
            thread::sleep(Duration::from_millis(10));
        }
        let duration = decoder
            .snapshot()
            .metrics()
            .unwrap()
            .duration_frames()
            .unwrap();
        let target = duration.saturating_sub(48_000);
        decoder.seek(target).unwrap();
        while !matches!(
            decoder.snapshot().phase(),
            DecodePhase::Ready | DecodePhase::EndOfStream
        ) {
            decoder.poll();
            assert_ne!(
                decoder.snapshot().phase(),
                DecodePhase::Failed,
                "{:?}",
                decoder.snapshot()
            );
            assert!(Instant::now() < deadline, "seek timed out");
            thread::sleep(Duration::from_millis(10));
        }
        let metrics = decoder.snapshot().metrics().unwrap();
        let config = OutputStreamConfig::new(
            decoder.request_id(),
            decoder.playback_epoch(),
            target,
            metrics.sample_rate(),
            metrics.scene_signature().unwrap().clone(),
            OutputDeviceSelection::SystemDefault,
        )
        .unwrap();
        let runtime = Runtime::spawn(
            config,
            OutputSettings {
                null_output: true,
                mode: SpatialBackendKind::SystemSpatial,
                ..Default::default()
            },
            decoder.scene_reader(),
            Arc::new(SceneViewMirror::new()),
            true,
            0.0,
        )
        .unwrap();
        loop {
            decoder.poll();
            let status = runtime.snapshot();
            assert_ne!(status.phase, OutputPhase::Failed, "{:?}", status.error);
            if status.phase == OutputPhase::Ended {
                assert_eq!(status.playhead_frames, duration);
                decoder.seek(status.playhead_frames).unwrap();
                println!("container and output end agree at {duration}; EOS seek accepted");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "tail playback timed out: {status:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn metadata_bits_are_translated_instead_of_copied() {
        assert_eq!(renderer_fields(8), 4);
        assert_eq!(renderer_fields(1 | 2 | 8), 7);
        assert_eq!(renderer_fields(4), 0);
    }

    #[test]
    fn importance_only_and_positionless_lfe_updates_are_accepted() {
        use crate::decoder::SceneLfePcm;
        let key = PlaybackKey::new(2, 1);
        let (queue, reader) = scene_queue_pair(key);
        let object = SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(0.0, 1.0, 0.0)),
            Some(1.0),
            true,
        );
        let lfe = SpatialObjectState::new(true, None, Some(1.0), true);
        let block = DecodedSceneBlock::new(
            48_000,
            0,
            33,
            1,
            0,
            None,
            true,
            vec![SceneObjectPcm::new(7, Some(object), vec![0.1; 33])],
            Some(SceneLfePcm::new(9, Some(lfe), vec![0.1; 33])),
            vec![
                SceneMetadataUpdate::new(7, 3, 0, 4, object),
                SceneMetadataUpdate::new(9, 5, 0, 1 | 2 | 8, lfe),
                SceneMetadataUpdate::new(7, 33, 0, 8, object),
            ],
        );
        let signature = SceneSignature::from_block(&block);
        queue.try_push(key, block).unwrap();
        queue.mark_end_of_stream(key);
        let config = OutputStreamConfig::new(
            2,
            1,
            0,
            48_000,
            signature,
            OutputDeviceSelection::SystemDefault,
        )
        .unwrap();
        let settings = OutputSettings {
            null_output: true,
            mode: SpatialBackendKind::SystemSpatial,
            ..Default::default()
        };
        let runtime = Runtime::spawn(
            config,
            settings,
            reader,
            Arc::new(SceneViewMirror::new()),
            true,
            0.0,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = runtime.snapshot();
            assert_ne!(status.phase, OutputPhase::Failed, "{:?}", status.error);
            if status.phase == OutputPhase::Ended {
                assert_eq!(status.playhead_frames, 33);
                break;
            }
            assert!(Instant::now() < deadline, "metadata update test timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
