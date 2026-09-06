use std::fs;
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use macindecode_ac4_bitstream::topology::{Ac4Topology, RandomAccess};
use macindecode_ac4_bitstream::{Ac4Toc, SyncFrameIter};
use macindecode_ac4_mp4::{Ac4Mp4, Ac4Mp4Timeline};
use macindecode_ac4_scene::{
    Ac4DecoderConfig, Ac4DecoderSession, Ac4SceneFrame, AccessUnit, AccessUnitContext, DecodeMode,
    DecodeStatus, PresentationSelection, SceneObjectState,
};

use super::{
    DecodeContainer, DecodeMetrics, DecodePhase, DecodedSceneBlock, DecoderSnapshot,
    PREBUFFER_MILLISECONDS, PlaybackKey, QueuePushError, SceneLfePcm, SceneMetadataUpdate,
    SceneObjectPcm, SharedSceneQueue, SpatialObjectState, SpatialPosition, WorkerCommand,
    WorkerEvent, WorkerEventKind, WorkerHandle,
};

/// Stack for the decode thread.
///
/// Measured against one 20-object L4 A-JOC stream decoded end to end: a release
/// build overflows at 512 KiB and survives 1 MiB; a debug build overflows at
/// 1 MiB and survives 2 MiB. So std's 2 MiB default for a spawned thread does
/// carry that stream -- with under 2x headroom in the profile developers run,
/// on one piece of content, for work whose stack use varies with the tool
/// configuration in the bitstream. Core reserves 16 MiB for the same
/// reconstruction in its own tests; matching that is the cheap side of the
/// trade, since a thread stack is reserved address space rather than committed
/// memory.
const DECODE_STACK_BYTES: usize = 16 * 1024 * 1024;
const MAX_MP4_EDIT_ENTRIES: usize = 8;
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(20);

pub(super) fn spawn(queue: SharedSceneQueue) -> Result<WorkerHandle, String> {
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let join_handle = thread::Builder::new()
        .name("ac4-core-decode".to_owned())
        // Core's A-JOC reconstruction wants a large workspace on the stack.
        // On Windows this thread used to inherit the 8 MB that
        // `.cargo/config.toml` asks the linker for -- invisible, and gone the
        // moment the same code runs anywhere else. Ask for it here so the
        // requirement travels with the code rather than with the target.
        .stack_size(DECODE_STACK_BYTES)
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

#[derive(Default)]
struct SeekPreroll {
    blocks: Vec<DecodedSceneBlock>,
    frames: u64,
    published: bool,
}

impl SeekPreroll {
    fn push(&mut self, block: DecodedSceneBlock) {
        self.frames = self
            .frames
            .saturating_add(u64::from(block.duration_frames()));
        self.blocks.push(block);
    }

    fn ready(&self, sample_rate: u32) -> bool {
        has_prebuffer(self.frames, sample_rate)
    }

    fn publish(
        &mut self,
        key: PlaybackKey,
        path: &Path,
        metrics: &mut DecodeMetrics,
        commands: &Receiver<WorkerCommand>,
        events: &Sender<WorkerEvent>,
        queue: &SharedSceneQueue,
    ) -> Result<RunControl, String> {
        self.published |= !self.blocks.is_empty();
        for block in std::mem::take(&mut self.blocks) {
            let control = enqueue_block(key, path, block, metrics, commands, events, queue)?;
            if !matches!(control, RunControl::Complete) {
                return Ok(control);
            }
        }
        Ok(RunControl::Complete)
    }
}

fn decoder_worker(
    command_receiver: &Receiver<WorkerCommand>,
    event_sender: &Sender<WorkerEvent>,
    queue: &SharedSceneQueue,
) {
    let mut pending = None;
    let mut loaded: Option<LoadedMedia> = None;
    loop {
        let command = match pending.take() {
            Some(command) => command,
            None => match command_receiver.recv() {
                Ok(command) => command,
                Err(_) => break,
            },
        };
        match command {
            WorkerCommand::Open {
                key,
                path,
                reuse_cached,
            } => {
                let failure_path = path.clone();
                loaded = loaded.filter(|media| reuse_cached && media.path == path);
                if let Some(media) = loaded.as_mut() {
                    media.session = new_session();
                } else {
                    loaded = match LoadedMedia::open(key, path, event_sender) {
                        Ok(media) => Some(media),
                        Err(error) => {
                            send_failure(key, &failure_path, error, event_sender);
                            None
                        }
                    };
                }
                let Some(media) = loaded.as_mut() else {
                    continue;
                };
                match media.decode_from(key, 0, false, command_receiver, event_sender, queue) {
                    RunControl::Complete => {}
                    RunControl::Command(command) => pending = Some(command),
                    RunControl::Shutdown => break,
                }
            }
            WorkerCommand::Seek { key, target_frame } => {
                let Some(media) = loaded.as_mut() else {
                    send_failure(
                        key,
                        Path::new(""),
                        "Seek requested without a loaded source".to_owned(),
                        event_sender,
                    );
                    continue;
                };
                match media.decode_from(
                    key,
                    target_frame,
                    true,
                    command_receiver,
                    event_sender,
                    queue,
                ) {
                    RunControl::Complete => {}
                    RunControl::Command(command) => pending = Some(command),
                    RunControl::Shutdown => break,
                }
            }
            WorkerCommand::Close => loaded = None,
            WorkerCommand::Shutdown => break,
        }
    }
}

fn is_raw_ac4(bytes: &[u8]) -> bool {
    matches!(bytes.get(..2), Some([0xAC, 0x40 | 0x41]))
}

struct IndexedAccessUnit {
    range: Range<usize>,
    index: u64,
    source_start: i64,
    presentation_start: i64,
    priming_samples: Option<u64>,
    random_access_hint: Option<bool>,
    safe_random_access: bool,
}

struct MediaIndex {
    duration_frames: u64,
    seekable_from_frame: Option<u64>,
    access_units: Vec<IndexedAccessUnit>,
}

impl MediaIndex {
    fn new(duration_frames: u64, access_units: Vec<IndexedAccessUnit>) -> Result<Self, String> {
        if access_units.is_empty() {
            return Err("Input contains no AC-4 access unit".to_owned());
        }
        let seekable_from_frame = access_units
            .iter()
            .filter(|unit| unit.safe_random_access)
            .map(|unit| u64::try_from(unit.presentation_start.max(0)).unwrap_or(u64::MAX))
            .min();
        Ok(Self {
            duration_frames,
            seekable_from_frame,
            access_units,
        })
    }

    fn seek_start_before(&self, target_frame: i64, before_index: usize) -> Option<usize> {
        self.access_units
            .iter()
            .enumerate()
            .take(before_index)
            .rev()
            .find(|(_, unit)| unit.safe_random_access && unit.presentation_start <= target_frame)
            .map(|(index, _)| index)
    }
}

enum MediaIndexState {
    Building,
    Ready(Arc<MediaIndex>),
    Failed(String),
}

struct SharedMediaIndex {
    state: Arc<Mutex<MediaIndexState>>,
    cancel: Arc<AtomicBool>,
}

impl SharedMediaIndex {
    fn spawn(
        key: PlaybackKey,
        container: DecodeContainer,
        bytes: Arc<Vec<u8>>,
        known_duration: Option<u64>,
        event_sender: &Sender<WorkerEvent>,
    ) -> Self {
        let state = Arc::new(Mutex::new(MediaIndexState::Building));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_state = Arc::clone(&state);
        let worker_cancel = Arc::clone(&cancel);
        let worker_events = event_sender.clone();
        let spawn_result = thread::Builder::new()
            .name("ac4-seek-index".to_owned())
            .spawn(move || {
                let result = match container {
                    DecodeContainer::IsoBmff => build_mp4_index(&bytes, &worker_cancel),
                    DecodeContainer::RawAc4 => build_raw_index(&bytes, &worker_cancel),
                };
                if worker_cancel.load(Ordering::Acquire) {
                    return;
                }
                let (duration_frames, seekable_from_frame, error, state_update) = match result {
                    Ok(Some(index)) => {
                        let index = Arc::new(index);
                        (
                            Some(index.duration_frames),
                            index.seekable_from_frame,
                            None,
                            MediaIndexState::Ready(index),
                        )
                    }
                    Ok(None) => return,
                    Err(error) => (
                        known_duration,
                        None,
                        Some(error.clone()),
                        MediaIndexState::Failed(error),
                    ),
                };
                *lock_recover(&worker_state) = state_update;
                let _ = worker_events.send(WorkerEvent {
                    kind: WorkerEventKind::IndexFinished {
                        request_id: key.request_id(),
                        duration_frames,
                        seekable_from_frame,
                        error,
                    },
                });
            });
        if let Err(error) = spawn_result {
            let error = format!("Failed to start the AC-4 seek index worker: {error}");
            *lock_recover(&state) = MediaIndexState::Failed(error.clone());
            let _ = event_sender.send(WorkerEvent {
                kind: WorkerEventKind::IndexFinished {
                    request_id: key.request_id(),
                    duration_frames: known_duration,
                    seekable_from_frame: None,
                    error: Some(error),
                },
            });
        }
        Self { state, cancel }
    }

    fn ready(&self) -> Result<Arc<MediaIndex>, String> {
        match &*lock_recover(&self.state) {
            MediaIndexState::Building => Err("Media indexing has not completed yet".to_owned()),
            MediaIndexState::Ready(index) => Ok(Arc::clone(index)),
            MediaIndexState::Failed(error) => Err(format!("Media seek index failed: {error}")),
        }
    }
}

impl Drop for SharedMediaIndex {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone, Copy)]
struct Mp4Timing {
    sample_rate: u32,
    priming_samples: u64,
    presentation_shift: i64,
    duration_frames: u64,
}

fn parse_mp4_timing(
    bytes: &[u8],
) -> Result<(Ac4Mp4<'_>, Ac4Mp4Timeline<MAX_MP4_EDIT_ENTRIES>, Mp4Timing), String> {
    let source = Ac4Mp4::parse(bytes).map_err(|error| error.to_string())?;
    let timeline = source
        .presentation_timeline::<MAX_MP4_EDIT_ENTRIES>()
        .map_err(|error| error.to_string())?;
    if timeline.media_edit_count() > 1 {
        return Err("Multiple discontiguous MP4 media edits are not supported yet".to_owned());
    }
    let sample_rate = source.dsi().base_sampling_frequency.hz();
    let timing = Mp4Timing {
        sample_rate,
        priming_samples: timeline
            .priming_samples(sample_rate)
            .map_err(|error| error.to_string())?,
        presentation_shift: timeline
            .presentation_sample_shift(sample_rate)
            .map_err(|error| error.to_string())?
            .unwrap_or(0),
        duration_frames: timeline
            .presentation_duration_samples(sample_rate)
            .map_err(|error| error.to_string())?,
    };
    Ok((source, timeline, timing))
}

fn build_mp4_index(bytes: &[u8], cancel: &AtomicBool) -> Result<Option<MediaIndex>, String> {
    let (source, timeline, timing) = parse_mp4_timing(bytes)?;
    let mut access_units =
        Vec::with_capacity(usize::try_from(source.sample_count()).unwrap_or(usize::MAX));
    for item in source.access_units() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let access_unit = item.map_err(|error| error.to_string())?;
        let source_start = timeline
            .media_time_samples(access_unit.info.composition_time, timing.sample_rate)
            .map_err(|error| error.to_string())?;
        let presentation_start = source_start
            .checked_add(timing.presentation_shift)
            .ok_or_else(|| "MP4 presentation position overflow after applying edits".to_owned())?;
        let full_random_access = Ac4Topology::parse(access_unit.payload)
            .map_err(|error| error.to_string())?
            .random_access()
            == RandomAccess::Full;
        access_units.push(IndexedAccessUnit {
            range: access_unit.range,
            index: u64::from(access_unit.info.index),
            source_start,
            presentation_start,
            priming_samples: Some(timing.priming_samples),
            random_access_hint: Some(access_unit.info.is_sync),
            safe_random_access: access_unit.info.is_sync && full_random_access,
        });
    }
    MediaIndex::new(timing.duration_frames, access_units).map(Some)
}

fn raw_payload_range(
    offset: usize,
    total_size: usize,
    raw_frame_len: usize,
    has_crc: bool,
) -> Result<Range<usize>, String> {
    let crc_bytes = usize::from(has_crc) * 2;
    let start = offset
        .checked_add(total_size)
        .and_then(|end| end.checked_sub(raw_frame_len + crc_bytes))
        .ok_or_else(|| "Raw AC-4 sync-frame range underflow".to_owned())?;
    let end = start
        .checked_add(raw_frame_len)
        .ok_or_else(|| "Raw AC-4 sync-frame range overflow".to_owned())?;
    Ok(start..end)
}

fn build_raw_index(bytes: &[u8], cancel: &AtomicBool) -> Result<Option<MediaIndex>, String> {
    let mut access_units = Vec::new();
    let mut frame_start = 0i64;
    let mut sample_rate = None;
    for (index, item) in SyncFrameIter::new(bytes).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let sync_frame = item.map_err(|error| error.to_string())?;
        let toc = Ac4Toc::parse(sync_frame.raw_frame).map_err(|error| error.to_string())?;
        let current_sample_rate = toc
            .base_sampling_frequency_hz()
            .ok_or_else(|| "Raw AC-4 declares no supported sample rate".to_owned())?;
        if sample_rate
            .replace(current_sample_rate)
            .is_some_and(|rate| rate != current_sample_rate)
        {
            return Err("Raw AC-4 changes sample rate midstream".to_owned());
        }
        let frame_len = toc
            .codec_frame_len_base(1)
            .ok_or_else(|| "Cannot derive raw AC-4 frame length".to_owned())?;
        let range = raw_payload_range(
            sync_frame.offset,
            sync_frame.total_size,
            sync_frame.raw_frame.len(),
            sync_frame.crc_word.is_some(),
        )?;
        let safe_random_access = Ac4Topology::parse(sync_frame.raw_frame)
            .map_err(|error| error.to_string())?
            .random_access()
            == RandomAccess::Full;
        access_units.push(IndexedAccessUnit {
            range,
            index: u64::try_from(index)
                .map_err(|_| "Raw AC-4 access-unit index overflow".to_owned())?,
            source_start: frame_start,
            presentation_start: frame_start,
            priming_samples: None,
            random_access_hint: None,
            safe_random_access,
        });
        frame_start = frame_start
            .checked_add(i64::from(frame_len))
            .ok_or_else(|| "Raw AC-4 timeline overflow".to_owned())?;
    }
    let duration_frames =
        u64::try_from(frame_start).map_err(|_| "Raw AC-4 duration is negative".to_owned())?;
    MediaIndex::new(duration_frames, access_units).map(Some)
}

impl IndexedAccessUnit {
    fn context(&self) -> AccessUnitContext {
        let mut context = AccessUnitContext::new(self.index)
            .with_source_sample_start(self.source_start)
            .with_presentation_sample_start(self.presentation_start);
        if let Some(priming) = self.priming_samples {
            context = context.with_priming_samples(priming);
        }
        if let Some(hint) = self.random_access_hint {
            context = context.with_random_access_hint(hint);
        }
        context
    }
}

struct LoadedMedia {
    path: std::path::PathBuf,
    bytes: Arc<Vec<u8>>,
    container: DecodeContainer,
    sample_rate: u32,
    known_duration_frames: Option<u64>,
    index: SharedMediaIndex,
    session: Ac4DecoderSession,
}

impl LoadedMedia {
    fn open(
        key: PlaybackKey,
        path: std::path::PathBuf,
        event_sender: &Sender<WorkerEvent>,
    ) -> Result<Self, String> {
        let bytes = Arc::new(
            fs::read(&path).map_err(|error| format!("Failed to read AC-4 media: {error}"))?,
        );
        if is_raw_ac4(&bytes) {
            Self::open_raw(key, path, bytes, event_sender)
        } else {
            Self::open_mp4(key, path, bytes, event_sender)
        }
    }

    fn open_mp4(
        key: PlaybackKey,
        path: std::path::PathBuf,
        bytes: Arc<Vec<u8>>,
        event_sender: &Sender<WorkerEvent>,
    ) -> Result<Self, String> {
        let (_, _, timing) = parse_mp4_timing(&bytes)?;
        let known_duration_frames = Some(timing.duration_frames);
        let index = SharedMediaIndex::spawn(
            key,
            DecodeContainer::IsoBmff,
            Arc::clone(&bytes),
            known_duration_frames,
            event_sender,
        );
        Ok(Self {
            path,
            bytes,
            container: DecodeContainer::IsoBmff,
            sample_rate: timing.sample_rate,
            known_duration_frames,
            index,
            session: new_session(),
        })
    }

    fn open_raw(
        key: PlaybackKey,
        path: std::path::PathBuf,
        bytes: Arc<Vec<u8>>,
        event_sender: &Sender<WorkerEvent>,
    ) -> Result<Self, String> {
        let first = SyncFrameIter::new(&bytes)
            .next()
            .ok_or_else(|| "Input contains no AC-4 sync frame".to_owned())?
            .map_err(|error| error.to_string())?;
        let toc = Ac4Toc::parse(first.raw_frame).map_err(|error| error.to_string())?;
        let sample_rate = toc
            .base_sampling_frequency_hz()
            .ok_or_else(|| "Raw AC-4 declares no supported sample rate".to_owned())?;
        let index = SharedMediaIndex::spawn(
            key,
            DecodeContainer::RawAc4,
            Arc::clone(&bytes),
            None,
            event_sender,
        );
        Ok(Self {
            path,
            bytes,
            container: DecodeContainer::RawAc4,
            sample_rate,
            known_duration_frames: None,
            index,
            session: new_session(),
        })
    }

    fn decode_from(
        &mut self,
        key: PlaybackKey,
        target_frame: u64,
        discontinuity: bool,
        command_receiver: &Receiver<WorkerCommand>,
        event_sender: &Sender<WorkerEvent>,
        queue: &SharedSceneQueue,
    ) -> RunControl {
        match self.try_decode_from(
            key,
            target_frame,
            discontinuity,
            command_receiver,
            event_sender,
            queue,
        ) {
            Ok(control) => control,
            Err(error) => {
                send_failure(key, &self.path, error, event_sender);
                RunControl::Complete
            }
        }
    }

    fn try_decode_from(
        &mut self,
        key: PlaybackKey,
        target_frame: u64,
        discontinuity: bool,
        command_receiver: &Receiver<WorkerCommand>,
        event_sender: &Sender<WorkerEvent>,
        queue: &SharedSceneQueue,
    ) -> Result<RunControl, String> {
        if !discontinuity || target_frame == 0 {
            if discontinuity {
                self.session.reset();
            }
            // Presentation frame zero can follow encoded priming AUs. Replay
            // must include those AUs, just like the initial open, rather than
            // starting at a later sync sample whose presentation time is zero.
            return self.decode_initial(key, command_receiver, event_sender, queue);
        }

        let index = self.index.ready()?;
        if target_frame == index.duration_frames {
            let metrics = initial_metrics(
                self.container,
                self.sample_rate,
                Some(index.duration_frames),
                index.seekable_from_frame,
                false,
                target_frame,
            );
            queue.mark_end_of_stream(key);
            send_progress(
                key,
                &self.path,
                DecodePhase::EndOfStream,
                &metrics,
                event_sender,
            );
            return Ok(RunControl::Complete);
        }
        let target_i64 = i64::try_from(target_frame)
            .map_err(|_| "Seek target exceeds the signed Scene timeline".to_owned())?;
        let mut before_index = index.access_units.len();
        loop {
            let start_index = index
                .seek_start_before(target_i64, before_index)
                .ok_or_else(|| {
                    "No usable complete random-access point exists at or before the seek target"
                        .to_owned()
                })?;
            self.session.reset();
            let mut metrics = initial_metrics(
                self.container,
                self.sample_rate,
                Some(index.duration_frames),
                index.seekable_from_frame,
                false,
                target_frame,
            );
            let mut preroll = SeekPreroll::default();
            let attempt = (|| {
                for unit_index in start_index..index.access_units.len() {
                    if let Some(control) = pending_command(command_receiver) {
                        return Ok(control);
                    }
                    let unit = index
                        .access_units
                        .get(unit_index)
                        .ok_or_else(|| "Indexed AC-4 access unit disappeared".to_owned())?;
                    let raw_frame = self.bytes.get(unit.range.clone()).ok_or_else(|| {
                        "Indexed AC-4 access unit exceeds the cached source".to_owned()
                    })?;
                    let control = decode_access_unit(
                        key,
                        &self.path,
                        &mut self.session,
                        raw_frame,
                        unit.context(),
                        target_i64,
                        &mut metrics,
                        command_receiver,
                        event_sender,
                        queue,
                        Some(&mut preroll),
                    )?;
                    if !matches!(control, RunControl::Complete) {
                        return Ok(control);
                    }
                }
                let control = preroll.publish(
                    key,
                    &self.path,
                    &mut metrics,
                    command_receiver,
                    event_sender,
                    queue,
                )?;
                if !matches!(control, RunControl::Complete) {
                    return Ok(control);
                }
                finish(key, &self.path, &metrics, event_sender, queue)
            })();
            match attempt {
                Ok(control) => return Ok(control),
                Err(error) if !preroll.published => {
                    if index.seek_start_before(target_i64, start_index).is_none() {
                        return Err(error);
                    }
                    before_index = start_index;
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn decode_initial(
        &mut self,
        key: PlaybackKey,
        commands: &Receiver<WorkerCommand>,
        events: &Sender<WorkerEvent>,
        queue: &SharedSceneQueue,
    ) -> Result<RunControl, String> {
        match self.container {
            DecodeContainer::IsoBmff => self.decode_initial_mp4(key, commands, events, queue),
            DecodeContainer::RawAc4 => self.decode_initial_raw(key, commands, events, queue),
        }
    }

    fn decode_initial_mp4(
        &mut self,
        key: PlaybackKey,
        command_receiver: &Receiver<WorkerCommand>,
        event_sender: &Sender<WorkerEvent>,
        queue: &SharedSceneQueue,
    ) -> Result<RunControl, String> {
        let (source, timeline, timing) = parse_mp4_timing(&self.bytes)?;
        let mut metrics = initial_metrics(
            DecodeContainer::IsoBmff,
            timing.sample_rate,
            Some(timing.duration_frames),
            None,
            true,
            0,
        );
        for item in source.access_units() {
            if let Some(control) = pending_command(command_receiver) {
                return Ok(control);
            }
            let access_unit = item.map_err(|error| error.to_string())?;
            let source_start = timeline
                .media_time_samples(access_unit.info.composition_time, timing.sample_rate)
                .map_err(|error| error.to_string())?;
            let presentation_start = source_start
                .checked_add(timing.presentation_shift)
                .ok_or_else(|| {
                    "MP4 presentation position overflow after applying edits".to_owned()
                })?;
            let context = AccessUnitContext::new(u64::from(access_unit.info.index))
                .with_source_sample_start(source_start)
                .with_presentation_sample_start(presentation_start)
                .with_priming_samples(timing.priming_samples)
                .with_random_access_hint(access_unit.info.is_sync);
            let control = decode_access_unit(
                key,
                &self.path,
                &mut self.session,
                access_unit.payload,
                context,
                0,
                &mut metrics,
                command_receiver,
                event_sender,
                queue,
                None,
            )?;
            if !matches!(control, RunControl::Complete) {
                return Ok(control);
            }
        }
        finish(key, &self.path, &metrics, event_sender, queue)
    }

    fn decode_initial_raw(
        &mut self,
        key: PlaybackKey,
        command_receiver: &Receiver<WorkerCommand>,
        event_sender: &Sender<WorkerEvent>,
        queue: &SharedSceneQueue,
    ) -> Result<RunControl, String> {
        let mut metrics = initial_metrics(
            DecodeContainer::RawAc4,
            self.sample_rate,
            self.known_duration_frames,
            None,
            true,
            0,
        );
        let mut frame_start = 0i64;
        for (index, item) in SyncFrameIter::new(&self.bytes).enumerate() {
            if let Some(control) = pending_command(command_receiver) {
                return Ok(control);
            }
            let sync_frame = item.map_err(|error| error.to_string())?;
            let toc = Ac4Toc::parse(sync_frame.raw_frame).map_err(|error| error.to_string())?;
            let sample_rate = toc
                .base_sampling_frequency_hz()
                .ok_or_else(|| "Raw AC-4 declares no supported sample rate".to_owned())?;
            if sample_rate != self.sample_rate {
                return Err("Raw AC-4 changes sample rate midstream".to_owned());
            }
            let context = AccessUnitContext::new(
                u64::try_from(index)
                    .map_err(|_| "Raw AC-4 access-unit index overflow".to_owned())?,
            )
            .with_source_sample_start(frame_start)
            .with_presentation_sample_start(frame_start);
            let control = decode_access_unit(
                key,
                &self.path,
                &mut self.session,
                sync_frame.raw_frame,
                context,
                0,
                &mut metrics,
                command_receiver,
                event_sender,
                queue,
                None,
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
        }
        metrics.duration_frames = Some(
            u64::try_from(frame_start).map_err(|_| "Raw AC-4 duration is negative".to_owned())?,
        );
        finish(key, &self.path, &metrics, event_sender, queue)
    }
}

fn new_session() -> Ac4DecoderSession {
    Ac4DecoderSession::new(
        Ac4DecoderConfig::new(PresentationSelection::AutoUnique).with_decode_mode(DecodeMode::Full),
    )
}

#[allow(clippy::too_many_arguments)]
fn decode_access_unit(
    key: PlaybackKey,
    path: &Path,
    session: &mut Ac4DecoderSession,
    raw_frame: &[u8],
    context: AccessUnitContext,
    target_frame: i64,
    metrics: &mut DecodeMetrics,
    command_receiver: &Receiver<WorkerCommand>,
    event_sender: &Sender<WorkerEvent>,
    queue: &SharedSceneQueue,
    mut seek_preroll: Option<&mut SeekPreroll>,
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
        let mut block = own_scene_frame(frame)?;
        metrics.decoded_scene_frames = metrics.decoded_scene_frames.saturating_add(1);
        metrics.decoded_frames = metrics
            .decoded_frames
            .saturating_add(u64::from(block.duration_frames));
        metrics.metadata_updates = metrics
            .metadata_updates
            .saturating_add(u64::try_from(block.metadata_updates.len()).unwrap_or(u64::MAX));

        // Codec frames include encoder padding. MP4's presentation duration is
        // already known before indexing; trim before any output backend sees PCM.
        if let Some(duration) = metrics.duration_frames {
            let end =
                i64::try_from(duration).map_err(|_| "Media duration exceeds signed sample time")?;
            if !block.truncate_at(end) {
                continue;
            }
        }

        let block_end = block
            .start_frame()
            .checked_add(i64::from(block.duration_frames()))
            .ok_or_else(|| "Decoded Scene block timeline overflow".to_owned())?;
        if block_end <= target_frame {
            continue;
        }
        if metrics.scene_signature.is_none() {
            metrics.presentation_index = block.presentation_index;
            metrics.presentation_id = block.presentation_id;
            metrics.object_count = block.objects.len();
            metrics.has_lfe = block.lfe.is_some();
            metrics.state_complete = block.state_complete;
            metrics.scene_signature = Some(super::SceneSignature::from_block(&block));
        }

        let control = if let Some(preroll) = seek_preroll
            .as_deref_mut()
            .filter(|pending| !pending.published)
        {
            // An access point can emit provisional PCM before delayed A-SPX
            // controls discover missing history. Keep the normal startup
            // prebuffer private so an earlier access point can still be tried
            // without leaking PCM, metadata, or Ready events to the consumer.
            preroll.push(block);
            if !preroll.ready(metrics.sample_rate) {
                continue;
            }
            preroll.publish(key, path, metrics, command_receiver, event_sender, queue)?
        } else {
            enqueue_block(
                key,
                path,
                block,
                metrics,
                command_receiver,
                event_sender,
                queue,
            )?
        };
        if !matches!(control, RunControl::Complete) {
            return Ok(control);
        }
    }
    Ok(RunControl::Complete)
}

#[allow(clippy::too_many_arguments)]
fn enqueue_block(
    key: PlaybackKey,
    path: &Path,
    mut block: DecodedSceneBlock,
    metrics: &mut DecodeMetrics,
    command_receiver: &Receiver<WorkerCommand>,
    event_sender: &Sender<WorkerEvent>,
    queue: &SharedSceneQueue,
) -> Result<RunControl, String> {
    loop {
        match queue.try_push(key, block) {
            Ok(snapshot) => {
                metrics.buffered_frames = snapshot.buffered_frames;
                metrics.buffer_capacity_frames = snapshot.capacity_frames;
                let phase = if is_prebuffered(metrics) {
                    DecodePhase::Ready
                } else {
                    DecodePhase::Buffering
                };
                send_progress(key, path, phase, metrics, event_sender);
                return Ok(RunControl::Complete);
            }
            Err(QueuePushError::Full(returned)) => {
                block = *returned;
                send_progress(key, path, DecodePhase::Ready, metrics, event_sender);
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
    key: PlaybackKey,
    path: &Path,
    metrics: &DecodeMetrics,
    event_sender: &Sender<WorkerEvent>,
    queue: &SharedSceneQueue,
) -> Result<RunControl, String> {
    if metrics.decoded_scene_frames == 0 {
        return Err("MacinDecode Core produced no scene PCM".to_owned());
    }
    queue.mark_end_of_stream(key);
    send_progress(key, path, DecodePhase::EndOfStream, metrics, event_sender);
    Ok(RunControl::Complete)
}

fn initial_metrics(
    container: DecodeContainer,
    sample_rate: u32,
    duration_frames: Option<u64>,
    seekable_from_frame: Option<u64>,
    indexing: bool,
    target_frame: u64,
) -> DecodeMetrics {
    DecodeMetrics {
        container,
        sample_rate,
        presentation_index: 0,
        presentation_id: None,
        object_count: 0,
        has_lfe: false,
        state_complete: false,
        scene_signature: None,
        decoded_access_units: 0,
        decoded_scene_frames: 0,
        decoded_frames: 0,
        buffered_frames: 0,
        buffer_capacity_frames: u64::from(sample_rate).saturating_mul(super::MAX_BUFFER_SECONDS),
        metadata_updates: 0,
        duration_frames,
        seekable_from_frame,
        indexing,
        index_error: None,
        target_frame,
    }
}

fn is_prebuffered(metrics: &DecodeMetrics) -> bool {
    has_prebuffer(metrics.buffered_frames, metrics.sample_rate)
}

fn has_prebuffer(frames: u64, sample_rate: u32) -> bool {
    frames.saturating_mul(1_000) >= u64::from(sample_rate).saturating_mul(PREBUFFER_MILLISECONDS)
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
                bed.initial_state().map(own_state),
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
    key: PlaybackKey,
    path: &Path,
    phase: DecodePhase,
    metrics: &DecodeMetrics,
    sender: &Sender<WorkerEvent>,
) {
    let _ = sender.send(WorkerEvent {
        kind: WorkerEventKind::Snapshot {
            key,
            snapshot: Box::new(DecoderSnapshot::progress(
                phase,
                path.to_path_buf(),
                metrics.clone(),
            )),
        },
    });
}

fn send_failure(key: PlaybackKey, path: &Path, error: String, sender: &Sender<WorkerEvent>) {
    let _ = sender.send(WorkerEvent {
        kind: WorkerEventKind::Snapshot {
            key,
            snapshot: Box::new(DecoderSnapshot::failed(path.to_path_buf(), error)),
        },
    });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::super::{DecodePhase, DecoderController};
    use super::{build_mp4_index, build_raw_index, parse_mp4_timing};

    fn local_media_path() -> PathBuf {
        std::env::var_os("MACINDECODE_AC4_TEST_MEDIA")
            .map(PathBuf::from)
            .expect("set MACINDECODE_AC4_TEST_MEDIA")
    }

    fn wait_for_decoder(
        controller: &mut DecoderController,
        predicate: impl Fn(&DecoderController) -> bool,
        description: &str,
    ) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            controller.poll();
            if matches!(controller.snapshot().phase(), DecodePhase::Failed) {
                panic!(
                    "decode failed while {description}: {}",
                    controller.snapshot().detail().unwrap_or("unknown error")
                );
            }
            if predicate(controller) {
                return;
            }
            assert!(Instant::now() < deadline, "{description}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[ignore = "requires MACINDECODE_AC4_TEST_MEDIA with local AC-4 media"]
    fn decodes_local_media_into_a_bounded_scene_buffer() {
        let path = local_media_path();
        let mut controller = DecoderController::new();
        controller.ensure_open(&path);
        wait_for_decoder(
            &mut controller,
            |controller| {
                matches!(
                    controller.snapshot().phase(),
                    DecodePhase::Ready | DecodePhase::EndOfStream
                )
            },
            "decode did not prebuffer in time",
        );
        let metrics = controller.snapshot().metrics().expect("decode metrics");
        eprintln!(
            "container={} sample_rate={} presentation={} objects={} lfe={} access_units={} scene_frames={} decoded_frames={} buffered_frames={}",
            metrics.container().label(),
            metrics.sample_rate(),
            metrics.presentation_index(),
            metrics.object_count(),
            metrics.has_lfe(),
            metrics.decoded_access_units(),
            metrics.decoded_scene_frames(),
            metrics.decoded_frames(),
            metrics.buffered_frames(),
        );
        assert!(metrics.decoded_access_units() > 0);
        assert!(metrics.decoded_frames() > 0);
        assert!(metrics.object_count() > 0 || metrics.has_lfe());
        assert!(metrics.buffered_frames() <= metrics.buffer_capacity_frames());
        assert!(controller.try_pop_scene_block().is_some());
    }

    #[test]
    #[ignore = "requires MACINDECODE_AC4_TEST_MEDIA with local AC-4 MP4 media"]
    fn seeks_real_media_across_epochs_without_rereading_the_file() {
        let path = local_media_path();
        let mut controller = DecoderController::new();
        controller.ensure_open(&path);
        wait_for_decoder(
            &mut controller,
            |controller| {
                controller.snapshot().metrics().is_some_and(|metrics| {
                    !metrics.is_indexing()
                        && metrics.index_error().is_none()
                        && matches!(
                            controller.snapshot().phase(),
                            DecodePhase::Ready | DecodePhase::EndOfStream
                        )
                })
            },
            "seek index did not complete in time",
        );
        let metrics = controller.snapshot().metrics().expect("indexed metrics");
        let duration = metrics.duration_frames().expect("indexed duration");
        let first_safe = metrics
            .seekable_from_frame()
            .expect("safe random access point");
        assert!(first_safe < duration, "test media has no seekable interval");
        let first_target = first_safe.saturating_add((duration - first_safe) / 3);
        let second_target = first_safe.saturating_add((duration - first_safe) * 2 / 3);

        let initial_epoch = controller.playback_epoch();
        controller.seek(first_target).expect("first seek");
        controller
            .seek(second_target)
            .expect("rapid replacement seek");
        assert_eq!(controller.playback_epoch(), initial_epoch + 2);
        wait_for_decoder(
            &mut controller,
            |controller| {
                controller.snapshot().metrics().is_some_and(|metrics| {
                    metrics.target_frame() == second_target
                        && matches!(
                            controller.snapshot().phase(),
                            DecodePhase::Ready | DecodePhase::EndOfStream
                        )
                })
            },
            "replacement seek did not prebuffer in time",
        );
        let first_block = controller
            .try_pop_scene_block()
            .expect("seek should produce a Scene block");
        let block_end = first_block
            .start_frame()
            .saturating_add(i64::from(first_block.duration_frames()));
        assert!(block_end > i64::try_from(second_target).unwrap_or(i64::MAX));

        controller.seek(duration).expect("seek to EOS");
        wait_for_decoder(
            &mut controller,
            |controller| matches!(controller.snapshot().phase(), DecodePhase::EndOfStream),
            "seek to EOS did not complete",
        );
        controller
            .seek(first_target)
            .expect("seek backward from EOS");
        wait_for_decoder(
            &mut controller,
            |controller| {
                controller.snapshot().metrics().is_some_and(|metrics| {
                    metrics.target_frame() == first_target
                        && matches!(controller.snapshot().phase(), DecodePhase::Ready)
                })
            },
            "backward seek from EOS did not prebuffer",
        );
    }

    #[test]
    #[ignore = "requires MACINDECODE_AC4_TEST_MEDIA with local AC-4 MP4 media"]
    fn replays_real_media_after_reconfiguring_at_the_end() {
        let path = local_media_path();
        let mut controller = DecoderController::new();
        controller.ensure_open(&path);
        wait_for_decoder(
            &mut controller,
            |controller| {
                controller.snapshot().metrics().is_some_and(|metrics| {
                    !metrics.is_indexing()
                        && metrics.can_seek_to(0)
                        && matches!(controller.snapshot().phase(), DecodePhase::Ready)
                })
            },
            "initial decode and seek index did not complete",
        );
        let duration = controller
            .snapshot()
            .metrics()
            .unwrap()
            .duration_frames()
            .unwrap();
        for cycle in 0..4 {
            controller.seek(duration.saturating_sub(48_000)).unwrap();
            wait_for_decoder(
                &mut controller,
                |controller| {
                    while controller.try_pop_scene_block().is_some() {}
                    controller.snapshot().phase() == DecodePhase::EndOfStream
                },
                "tail did not reach decoded EOS",
            );
            while controller.try_pop_scene_block().is_some() {}
            // Rebuilding an output after natural completion seeks to the
            // already-presented end before the Play button requests frame zero.
            controller.seek(duration).unwrap();
            wait_for_decoder(
                &mut controller,
                |controller| controller.snapshot().phase() == DecodePhase::EndOfStream,
                "output reconfiguration at EOS did not complete",
            );
            controller.seek(0).unwrap();
            wait_for_decoder(
                &mut controller,
                |controller| controller.snapshot().phase() == DecodePhase::Ready,
                "replay after output reconfiguration failed",
            );
            let first = controller.try_pop_scene_block().unwrap();
            assert!(
                first.start_frame() <= 0,
                "cycle {cycle}: skipped the beginning"
            );
            assert!(first.start_frame() + i64::from(first.duration_frames()) > 0);
        }
    }

    #[test]
    #[ignore = "requires MACINDECODE_AC4_TEST_MEDIA with local AC-4 MP4 media"]
    fn seeks_real_media_just_after_random_access_points() {
        let path = local_media_path();
        let bytes = std::fs::read(&path).unwrap();
        let index = build_mp4_index(&bytes, &AtomicBool::new(false))
            .unwrap()
            .unwrap();
        let mut targets: Vec<_> = index
            .access_units
            .iter()
            .filter(|unit| unit.safe_random_access && unit.presentation_start > 48_000)
            .take(12)
            .map(|unit| u64::try_from(unit.presentation_start).unwrap() + 1)
            .collect();
        assert!(
            !targets.is_empty(),
            "test media needs random access points during playback"
        );
        targets.insert(0, 1);
        targets.push(index.duration_frames - 1);
        let mut controller = DecoderController::new();
        controller.ensure_open(&path);
        wait_for_decoder(
            &mut controller,
            |controller| {
                controller.snapshot().phase() == DecodePhase::Ready
                    && controller
                        .snapshot()
                        .metrics()
                        .is_some_and(|metrics| !metrics.is_indexing())
            },
            "initial indexing",
        );
        for target in targets {
            controller.seek(target).unwrap();
            wait_for_decoder(
                &mut controller,
                |controller| {
                    matches!(
                        controller.snapshot().phase(),
                        DecodePhase::Ready | DecodePhase::EndOfStream
                    )
                },
                &format!("mode-change seek to {target}"),
            );
            let first = controller.try_pop_scene_block().unwrap();
            assert!(first.start_frame() <= i64::try_from(target).unwrap());
            assert!(
                first.start_frame() + i64::from(first.duration_frames())
                    > i64::try_from(target).unwrap()
            );
        }
    }

    #[test]
    #[ignore = "requires MACINDECODE_AC4_TEST_MEDIA with local AC-4 MP4 media"]
    fn failed_real_media_can_retry_from_cached_bytes() {
        struct MediaCopy(PathBuf);
        impl Drop for MediaCopy {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let copy = MediaCopy(std::env::temp_dir().join(format!(
            "macindecode-retry-{}-{unique}.mp4",
            std::process::id()
        )));
        std::fs::copy(local_media_path(), &copy.0).unwrap();
        let mut controller = DecoderController::new();
        controller.ensure_open(&copy.0);
        wait_for_decoder(
            &mut controller,
            |controller| {
                controller.snapshot().phase() == DecodePhase::Ready
                    && controller
                        .snapshot()
                        .metrics()
                        .is_some_and(|metrics| !metrics.is_indexing())
            },
            "initial media did not load",
        );
        std::fs::remove_file(&copy.0).unwrap();
        let old_key = controller.playback_key();
        controller.snapshot = super::super::DecoderSnapshot::failed(
            copy.0.clone(),
            "simulated Core failure after loading",
        );
        controller.retry().unwrap();
        wait_for_decoder(
            &mut controller,
            |controller| controller.snapshot().phase() == DecodePhase::Ready,
            "retry did not resume from cached media",
        );
        assert_ne!(controller.playback_key(), old_key);
        let first = controller.try_pop_scene_block().unwrap();
        assert!(first.start_frame() <= 0);
        assert!(first.start_frame() + i64::from(first.duration_frames()) > 0);
        assert!(
            !copy.0.exists(),
            "retry must not depend on reopening the source file"
        );
    }

    #[test]
    #[ignore = "requires MACINDECODE_AC4_TEST_MEDIA with local AC-4 MP4 media"]
    fn indexes_an_in_memory_raw_sync_stream_built_from_mp4_access_units() {
        let bytes = std::fs::read(local_media_path()).expect("read local media once for test");
        let (source, _, _) = parse_mp4_timing(&bytes).expect("parse AC-4 MP4");
        let mut sync_stream = Vec::new();
        let mut access_unit_count = 0usize;
        for item in source.access_units() {
            let access_unit = item.expect("bounded MP4 access unit");
            append_plain_sync_frame(&mut sync_stream, access_unit.payload);
            access_unit_count += 1;
        }
        let index = build_raw_index(&sync_stream, &AtomicBool::new(false))
            .expect("index raw sync stream")
            .expect("index was not cancelled");
        assert_eq!(index.access_units.len(), access_unit_count);
        assert!(index.duration_frames > 0);
        assert!(index.seekable_from_frame.is_some());
    }

    fn append_plain_sync_frame(stream: &mut Vec<u8>, raw_frame: &[u8]) {
        stream.extend_from_slice(&[0xAC, 0x40]);
        if let Ok(short) = u16::try_from(raw_frame.len())
            && short != u16::MAX
        {
            stream.extend_from_slice(&short.to_be_bytes());
        } else {
            let extended = u32::try_from(raw_frame.len()).expect("AC-4 AU length fits u32");
            assert!(extended <= 0x00FF_FFFF, "AC-4 AU length fits 24 bits");
            stream.extend_from_slice(&u16::MAX.to_be_bytes());
            stream.extend_from_slice(&extended.to_be_bytes()[1..]);
        }
        stream.extend_from_slice(raw_frame);
    }
}
