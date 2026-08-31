use std::fs;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use macindecode_ac4_bitstream::{Ac4Toc, SyncFrameIter};
use macindecode_ac4_mp4::Ac4Mp4;
use macindecode_ac4_scene::{
    Ac4DecoderConfig, Ac4DecoderSession, Ac4SceneFrame, AccessUnit, AccessUnitContext, DecodeMode,
    DecodeStatus, PresentationSelection, SceneObjectState,
};

use super::{
    DecodeContainer, DecodeMetrics, DecodePhase, DecodedSceneBlock, DecoderSnapshot,
    PREBUFFER_MILLISECONDS, QueuePushError, SceneLfePcm, SceneMetadataUpdate, SceneObjectPcm,
    SharedSceneQueue, SpatialObjectState, SpatialPosition, WorkerCommand, WorkerEvent,
    WorkerHandle,
};

const MAX_MP4_EDIT_ENTRIES: usize = 8;
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(20);

pub(super) fn spawn(queue: SharedSceneQueue) -> Result<WorkerHandle, String> {
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let join_handle = thread::Builder::new()
        .name("ac4-core-decode".to_owned())
        .spawn(move || decoder_worker(&command_receiver, &event_sender, &queue))
        .map_err(|error| format!("Failed to start MacinDecode Core worker: {error}"))?;
    Ok(WorkerHandle {
        command_sender,
        event_receiver,
        join_handle,
    })
}

#[derive(Debug)]
enum RunControl {
    Complete,
    Command(WorkerCommand),
    Shutdown,
}

fn decoder_worker(
    command_receiver: &Receiver<WorkerCommand>,
    event_sender: &Sender<WorkerEvent>,
    queue: &SharedSceneQueue,
) {
    let mut pending = None;
    loop {
        let command = match pending.take() {
            Some(command) => command,
            None => match command_receiver.recv() {
                Ok(command) => command,
                Err(_) => break,
            },
        };
        match command {
            WorkerCommand::Open { request_id, path } => {
                match decode_path(request_id, &path, command_receiver, event_sender, queue) {
                    RunControl::Complete => {}
                    RunControl::Command(command) => pending = Some(command),
                    RunControl::Shutdown => break,
                }
            }
            WorkerCommand::Close => {}
            WorkerCommand::Shutdown => break,
        }
    }
}

fn decode_path(
    request_id: u64,
    path: &Path,
    command_receiver: &Receiver<WorkerCommand>,
    event_sender: &Sender<WorkerEvent>,
    queue: &SharedSceneQueue,
) -> RunControl {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            send_failure(
                request_id,
                path,
                format!("Failed to read AC-4 media: {error}"),
                event_sender,
            );
            return RunControl::Complete;
        }
    };
    if let Some(control) = pending_command(command_receiver) {
        return control;
    }

    let result = if is_raw_ac4(&bytes) {
        decode_raw(
            request_id,
            path,
            &bytes,
            command_receiver,
            event_sender,
            queue,
        )
    } else {
        decode_mp4(
            request_id,
            path,
            &bytes,
            command_receiver,
            event_sender,
            queue,
        )
    };
    match result {
        Ok(control) => control,
        Err(error) => {
            send_failure(request_id, path, error, event_sender);
            RunControl::Complete
        }
    }
}

fn is_raw_ac4(bytes: &[u8]) -> bool {
    matches!(bytes.get(..2), Some([0xAC, 0x40 | 0x41]))
}

fn decode_mp4(
    request_id: u64,
    path: &Path,
    bytes: &[u8],
    command_receiver: &Receiver<WorkerCommand>,
    event_sender: &Sender<WorkerEvent>,
    queue: &SharedSceneQueue,
) -> Result<RunControl, String> {
    let source = Ac4Mp4::parse(bytes).map_err(|error| error.to_string())?;
    let timeline = source
        .presentation_timeline::<MAX_MP4_EDIT_ENTRIES>()
        .map_err(|error| error.to_string())?;
    if timeline.media_edit_count() > 1 {
        return Err("Multiple discontiguous MP4 media edits are not supported yet".to_owned());
    }
    let sample_rate = source.dsi().base_sampling_frequency.hz();
    let priming = timeline
        .priming_samples(sample_rate)
        .map_err(|error| error.to_string())?;
    let presentation_shift = timeline
        .presentation_sample_shift(sample_rate)
        .map_err(|error| error.to_string())?;
    let duration = timeline
        .presentation_duration_samples(sample_rate)
        .map_err(|error| error.to_string())?;
    let mut metrics = initial_metrics(DecodeContainer::IsoBmff, sample_rate, Some(duration));
    let mut session = new_session();

    for item in source.access_units() {
        if let Some(control) = pending_command(command_receiver) {
            return Ok(control);
        }
        let access_unit = item.map_err(|error| error.to_string())?;
        let info = access_unit.info;
        let source_start = timeline
            .media_time_samples(info.composition_time, sample_rate)
            .map_err(|error| error.to_string())?;
        let mut context = AccessUnitContext::new(u64::from(info.index))
            .with_source_sample_start(source_start)
            .with_priming_samples(priming)
            .with_random_access_hint(info.is_sync);
        if let Some(shift) = presentation_shift {
            let presentation_start = source_start.checked_add(shift).ok_or_else(|| {
                "MP4 presentation position overflow after applying edits".to_owned()
            })?;
            context = context.with_presentation_sample_start(presentation_start);
        }
        let control = decode_access_unit(
            request_id,
            path,
            &mut session,
            access_unit.payload,
            context,
            &mut metrics,
            command_receiver,
            event_sender,
            queue,
        )?;
        if !matches!(control, RunControl::Complete) {
            return Ok(control);
        }
    }
    finish(request_id, path, &metrics, event_sender)
}

fn decode_raw(
    request_id: u64,
    path: &Path,
    bytes: &[u8],
    command_receiver: &Receiver<WorkerCommand>,
    event_sender: &Sender<WorkerEvent>,
    queue: &SharedSceneQueue,
) -> Result<RunControl, String> {
    let mut session = new_session();
    let mut metrics = None;
    let mut frame_start = 0i64;
    let mut access_unit_index = 0u64;

    for item in SyncFrameIter::new(bytes) {
        if let Some(control) = pending_command(command_receiver) {
            return Ok(control);
        }
        let sync_frame = item.map_err(|error| error.to_string())?;
        let toc = Ac4Toc::parse(sync_frame.raw_frame).map_err(|error| error.to_string())?;
        let sample_rate = toc
            .base_sampling_frequency_hz()
            .ok_or_else(|| "Raw AC-4 declares no supported sample rate".to_owned())?;
        let current_metrics = metrics
            .get_or_insert_with(|| initial_metrics(DecodeContainer::RawAc4, sample_rate, None));
        if current_metrics.sample_rate != sample_rate {
            return Err("Raw AC-4 changes sample rate midstream".to_owned());
        }
        let context = AccessUnitContext::new(access_unit_index)
            .with_source_sample_start(frame_start)
            .with_presentation_sample_start(frame_start);
        let control = decode_access_unit(
            request_id,
            path,
            &mut session,
            sync_frame.raw_frame,
            context,
            current_metrics,
            command_receiver,
            event_sender,
            queue,
        )?;
        if !matches!(control, RunControl::Complete) {
            return Ok(control);
        }
        let frame_len = toc
            .codec_frame_len_base(1)
            .ok_or_else(|| "Cannot derive raw AC-4 frame length".to_owned())?;
        frame_start = frame_start
            .checked_add(i64::from(frame_len))
            .ok_or_else(|| "Raw AC-4 timeline overflow".to_owned())?;
        access_unit_index = access_unit_index
            .checked_add(1)
            .ok_or_else(|| "Raw AC-4 access-unit index overflow".to_owned())?;
    }

    let mut metrics = metrics.ok_or_else(|| "Input contains no AC-4 sync frame".to_owned())?;
    metrics.duration_frames = u64::try_from(frame_start).ok();
    finish(request_id, path, &metrics, event_sender)
}

fn new_session() -> Ac4DecoderSession {
    Ac4DecoderSession::new(
        Ac4DecoderConfig::new(PresentationSelection::AutoUnique).with_decode_mode(DecodeMode::Full),
    )
}

#[allow(clippy::too_many_arguments)]
fn decode_access_unit(
    request_id: u64,
    path: &Path,
    session: &mut Ac4DecoderSession,
    raw_frame: &[u8],
    context: AccessUnitContext,
    metrics: &mut DecodeMetrics,
    command_receiver: &Receiver<WorkerCommand>,
    event_sender: &Sender<WorkerEvent>,
    queue: &SharedSceneQueue,
) -> Result<RunControl, String> {
    let decoded = session
        .decode_access_unit(AccessUnit::new(raw_frame, context))
        .map_err(|error| error.to_string())?;
    metrics.decoded_access_units = metrics.decoded_access_units.saturating_add(1);
    match decoded.status() {
        DecodeStatus::Decoded => {}
        DecodeStatus::WaitingForRandomAccess { .. } => return Ok(RunControl::Complete),
        _ => return Err("MacinDecode Core returned an unknown decode status".to_owned()),
    }

    for frame in decoded.frames() {
        let block = own_scene_frame(frame)?;
        metrics.presentation_index = block.presentation_index;
        metrics.presentation_id = block.presentation_id;
        metrics.object_count = block.objects.len();
        metrics.has_lfe = block.lfe.is_some();
        metrics.state_complete = block.state_complete;
        metrics.decoded_scene_frames = metrics.decoded_scene_frames.saturating_add(1);
        metrics.decoded_frames = metrics
            .decoded_frames
            .saturating_add(u64::from(block.duration_frames));
        metrics.metadata_updates = metrics
            .metadata_updates
            .saturating_add(u64::try_from(block.metadata_updates.len()).unwrap_or(u64::MAX));

        let control = enqueue_block(
            request_id,
            path,
            block,
            metrics,
            command_receiver,
            event_sender,
            queue,
        )?;
        if !matches!(control, RunControl::Complete) {
            return Ok(control);
        }
    }
    Ok(RunControl::Complete)
}

#[allow(clippy::too_many_arguments)]
fn enqueue_block(
    request_id: u64,
    path: &Path,
    mut block: DecodedSceneBlock,
    metrics: &mut DecodeMetrics,
    command_receiver: &Receiver<WorkerCommand>,
    event_sender: &Sender<WorkerEvent>,
    queue: &SharedSceneQueue,
) -> Result<RunControl, String> {
    loop {
        match queue.try_push(request_id, block) {
            Ok(snapshot) => {
                metrics.buffered_frames = snapshot.buffered_frames;
                metrics.buffer_capacity_frames = snapshot.capacity_frames;
                let phase = if is_prebuffered(metrics) {
                    DecodePhase::Ready
                } else {
                    DecodePhase::Buffering
                };
                send_progress(request_id, path, phase, metrics, event_sender);
                return Ok(RunControl::Complete);
            }
            Err(QueuePushError::Full(returned)) => {
                block = returned;
                send_progress(request_id, path, DecodePhase::Ready, metrics, event_sender);
                match command_receiver.recv_timeout(COMMAND_POLL_INTERVAL) {
                    Ok(WorkerCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                        return Ok(RunControl::Shutdown);
                    }
                    Ok(command) => return Ok(RunControl::Command(command)),
                    Err(RecvTimeoutError::Timeout) => queue.wait_for_change(COMMAND_POLL_INTERVAL),
                }
            }
            Err(QueuePushError::Stale) => {
                return Ok(pending_command(command_receiver).unwrap_or(RunControl::Complete));
            }
            Err(QueuePushError::Format(error)) => return Err(error),
        }
    }
}

fn finish(
    request_id: u64,
    path: &Path,
    metrics: &DecodeMetrics,
    event_sender: &Sender<WorkerEvent>,
) -> Result<RunControl, String> {
    if metrics.decoded_scene_frames == 0 {
        return Err("MacinDecode Core produced no scene PCM".to_owned());
    }
    send_progress(
        request_id,
        path,
        DecodePhase::EndOfStream,
        metrics,
        event_sender,
    );
    Ok(RunControl::Complete)
}

fn initial_metrics(
    container: DecodeContainer,
    sample_rate: u32,
    duration_frames: Option<u64>,
) -> DecodeMetrics {
    DecodeMetrics {
        container,
        sample_rate,
        presentation_index: 0,
        presentation_id: None,
        object_count: 0,
        has_lfe: false,
        state_complete: false,
        decoded_access_units: 0,
        decoded_scene_frames: 0,
        decoded_frames: 0,
        buffered_frames: 0,
        buffer_capacity_frames: u64::from(sample_rate).saturating_mul(super::MAX_BUFFER_SECONDS),
        metadata_updates: 0,
        duration_frames,
    }
}

fn is_prebuffered(metrics: &DecodeMetrics) -> bool {
    metrics.buffered_frames.saturating_mul(1_000)
        >= u64::from(metrics.sample_rate).saturating_mul(PREBUFFER_MILLISECONDS)
}

fn own_scene_frame(frame: Ac4SceneFrame<'_>) -> Result<DecodedSceneBlock, String> {
    let timeline = frame.timeline();
    let expected_samples = usize::try_from(timeline.duration_samples())
        .map_err(|_| "Scene frame duration exceeds usize".to_owned())?;
    if expected_samples == 0 || timeline.sample_rate() == 0 {
        return Err("Scene frame has an invalid PCM duration or sample rate".to_owned());
    }

    let mut objects = Vec::with_capacity(frame.objects().len());
    for object in frame.objects() {
        let pcm = object.pcm();
        if pcm.planes().len() != 1 || pcm.samples_per_plane() != expected_samples {
            return Err("Scene object is not mono planar PCM".to_owned());
        }
        let plane = pcm
            .planes()
            .first()
            .ok_or_else(|| "Scene object PCM has no plane".to_owned())?;
        validate_samples(plane.samples(), expected_samples)?;
        objects.push(SceneObjectPcm::new(
            object.element_id().get(),
            object.initial_state().map(own_state),
            plane.samples().to_vec(),
        ));
    }

    let mut lfe = None;
    for bed in frame.beds() {
        for component in bed.components() {
            if lfe.is_some() {
                return Err("Scene contains more than one native LFE component".to_owned());
            }
            validate_samples(component.plane().samples(), expected_samples)?;
            lfe = Some(SceneLfePcm::new(
                bed.element_id().get(),
                component.plane().samples().to_vec(),
            ));
        }
    }

    if objects.is_empty() && lfe.is_none() {
        return Err("Scene frame contains no renderable PCM elements".to_owned());
    }
    let metadata_updates = frame
        .metadata_updates()
        .iter()
        .map(|update| {
            SceneMetadataUpdate::new(
                update.element_id().get(),
                update.offset_samples(),
                update.ramp_duration_samples(),
                update.changed_fields().bits(),
                own_state(update.state()),
            )
        })
        .collect();
    let presentation = frame.presentation();
    Ok(DecodedSceneBlock::new(
        timeline.sample_rate(),
        timeline
            .presentation_sample_start()
            .unwrap_or_else(|| timeline.codec_sample_start()),
        timeline.duration_samples(),
        timeline.configuration_generation(),
        presentation.index(),
        presentation.id(),
        frame.diagnostics().state_complete(),
        objects,
        lfe,
        metadata_updates,
    ))
}

fn validate_samples(samples: &[f32], expected_samples: usize) -> Result<(), String> {
    if samples.len() != expected_samples || samples.iter().any(|sample| !sample.is_finite()) {
        return Err("Scene PCM has an invalid length or non-finite sample".to_owned());
    }
    Ok(())
}

fn own_state(state: SceneObjectState) -> SpatialObjectState {
    let position = state
        .position()
        .map(|value| SpatialPosition::new(value.x(), value.y(), value.z()));
    SpatialObjectState::new(
        state.metadata_active(),
        position,
        state.linear_gain(),
        state.semantic_complete(),
    )
}

fn pending_command(receiver: &Receiver<WorkerCommand>) -> Option<RunControl> {
    match receiver.try_recv() {
        Ok(WorkerCommand::Shutdown) | Err(TryRecvError::Disconnected) => Some(RunControl::Shutdown),
        Ok(command) => Some(RunControl::Command(command)),
        Err(TryRecvError::Empty) => None,
    }
}

fn send_progress(
    request_id: u64,
    path: &Path,
    phase: DecodePhase,
    metrics: &DecodeMetrics,
    sender: &Sender<WorkerEvent>,
) {
    let _ = sender.send(WorkerEvent {
        request_id,
        snapshot: DecoderSnapshot::progress(phase, path.to_path_buf(), metrics.clone()),
    });
}

fn send_failure(request_id: u64, path: &Path, error: String, sender: &Sender<WorkerEvent>) {
    let _ = sender.send(WorkerEvent {
        request_id,
        snapshot: DecoderSnapshot::failed(path.to_path_buf(), error),
    });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::super::{DecodePhase, DecoderController};

    #[test]
    #[ignore = "requires MACINDECODE_AC4_TEST_MEDIA with local AC-4 media"]
    fn decodes_local_media_into_a_bounded_scene_buffer() {
        let path = std::env::var_os("MACINDECODE_AC4_TEST_MEDIA")
            .map(PathBuf::from)
            .expect("set MACINDECODE_AC4_TEST_MEDIA");
        let mut controller = DecoderController::new();
        controller.ensure_open(&path);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            controller.poll();
            match controller.snapshot().phase() {
                DecodePhase::Ready | DecodePhase::EndOfStream => break,
                DecodePhase::Failed => panic!(
                    "decode failed: {}",
                    controller.snapshot().detail().unwrap_or("unknown error")
                ),
                _ if Instant::now() >= deadline => panic!("decode did not prebuffer in time"),
                _ => thread::sleep(Duration::from_millis(10)),
            }
        }
        let metrics = controller.snapshot().metrics().expect("decode metrics");
        assert!(metrics.decoded_access_units() > 0);
        assert!(metrics.decoded_frames() > 0);
        assert!(metrics.object_count() > 0 || metrics.has_lfe());
        assert!(metrics.buffered_frames() <= metrics.buffer_capacity_frames());
        assert!(controller.try_pop_scene_block().is_some());
    }
}
