#![allow(
    clippy::must_use_candidate,
    reason = "decoder snapshots and Scene FIFO blocks are passive data-transfer views"
)]

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
#[cfg(target_os = "windows")]
use std::time::Duration;

#[cfg(target_os = "windows")]
mod windows;

pub const PREBUFFER_MILLISECONDS: u64 = 300;
pub const MAX_BUFFER_SECONDS: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeContainer {
    RawAc4,
    IsoBmff,
}

impl DecodeContainer {
    pub const fn label(self) -> &'static str {
        match self {
            Self::RawAc4 => "raw AC-4",
            Self::IsoBmff => "ISO BMFF",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodePhase {
    Unavailable,
    Idle,
    Opening,
    Seeking,
    Buffering,
    Ready,
    EndOfStream,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneSignature {
    configuration_generation: u32,
    presentation_index: u32,
    presentation_id: Option<u32>,
    object_element_ids: Vec<u64>,
    lfe_element_id: Option<u64>,
}

impl SceneSignature {
    pub(crate) fn new(
        configuration_generation: u32,
        presentation_index: u32,
        presentation_id: Option<u32>,
        mut object_element_ids: Vec<u64>,
        lfe_element_id: Option<u64>,
    ) -> Self {
        object_element_ids.sort_unstable();
        Self {
            configuration_generation,
            presentation_index,
            presentation_id,
            object_element_ids,
            lfe_element_id,
        }
    }

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(crate) fn from_block(block: &DecodedSceneBlock) -> Self {
        let object_element_ids = block
            .objects()
            .iter()
            .map(SceneObjectPcm::element_id)
            .collect::<Vec<_>>();
        Self::new(
            block.configuration_generation(),
            block.presentation_index(),
            block.presentation_id(),
            object_element_ids,
            block.lfe().map(SceneLfePcm::element_id),
        )
    }

    pub const fn configuration_generation(&self) -> u32 {
        self.configuration_generation
    }

    pub const fn presentation_index(&self) -> u32 {
        self.presentation_index
    }

    pub const fn presentation_id(&self) -> Option<u32> {
        self.presentation_id
    }

    pub fn object_element_ids(&self) -> &[u64] {
        &self.object_element_ids
    }

    pub const fn lfe_element_id(&self) -> Option<u64> {
        self.lfe_element_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeMetrics {
    pub(super) container: DecodeContainer,
    pub(super) sample_rate: u32,
    pub(super) presentation_index: u32,
    pub(super) presentation_id: Option<u32>,
    pub(super) object_count: usize,
    pub(super) has_lfe: bool,
    pub(super) state_complete: bool,
    pub(super) scene_signature: Option<SceneSignature>,
    pub(super) decoded_access_units: u64,
    pub(super) decoded_scene_frames: u64,
    pub(super) decoded_frames: u64,
    pub(super) buffered_frames: u64,
    pub(super) buffer_capacity_frames: u64,
    pub(super) metadata_updates: u64,
    pub(super) duration_frames: Option<u64>,
    pub(super) seekable_from_frame: Option<u64>,
    pub(super) indexing: bool,
    pub(super) index_error: Option<String>,
    pub(super) target_frame: u64,
}

impl DecodeMetrics {
    pub const fn container(&self) -> DecodeContainer {
        self.container
    }

    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub const fn presentation_index(&self) -> u32 {
        self.presentation_index
    }

    pub const fn presentation_id(&self) -> Option<u32> {
        self.presentation_id
    }

    pub const fn object_count(&self) -> usize {
        self.object_count
    }

    pub const fn has_lfe(&self) -> bool {
        self.has_lfe
    }

    pub const fn state_complete(&self) -> bool {
        self.state_complete
    }

    pub const fn scene_signature(&self) -> Option<&SceneSignature> {
        self.scene_signature.as_ref()
    }

    pub const fn decoded_access_units(&self) -> u64 {
        self.decoded_access_units
    }

    pub const fn decoded_scene_frames(&self) -> u64 {
        self.decoded_scene_frames
    }

    pub const fn decoded_frames(&self) -> u64 {
        self.decoded_frames
    }

    pub const fn buffered_frames(&self) -> u64 {
        self.buffered_frames
    }

    pub const fn buffer_capacity_frames(&self) -> u64 {
        self.buffer_capacity_frames
    }

    pub const fn metadata_updates(&self) -> u64 {
        self.metadata_updates
    }

    pub const fn duration_frames(&self) -> Option<u64> {
        self.duration_frames
    }

    pub const fn seekable_from_frame(&self) -> Option<u64> {
        self.seekable_from_frame
    }

    pub const fn is_indexing(&self) -> bool {
        self.indexing
    }

    pub fn index_error(&self) -> Option<&str> {
        self.index_error.as_deref()
    }

    pub const fn target_frame(&self) -> u64 {
        self.target_frame
    }

    pub fn can_seek_to(&self, target_frame: u64) -> bool {
        if self.indexing || self.index_error.is_some() {
            return false;
        }
        let Some(duration) = self.duration_frames else {
            return false;
        };
        if target_frame > duration {
            return false;
        }
        target_frame == duration
            || self
                .seekable_from_frame
                .is_some_and(|first| target_frame >= first)
    }

    pub fn buffered_milliseconds(&self) -> u64 {
        frames_to_milliseconds(self.buffered_frames, self.sample_rate)
    }

    pub fn decoded_milliseconds(&self) -> u64 {
        frames_to_milliseconds(self.decoded_frames, self.sample_rate)
    }
}

fn frames_to_milliseconds(frames: u64, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    frames.saturating_mul(1_000) / u64::from(sample_rate)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecoderSnapshot {
    phase: DecodePhase,
    path: Option<PathBuf>,
    metrics: Option<DecodeMetrics>,
    detail: Option<String>,
}

impl DecoderSnapshot {
    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            phase: DecodePhase::Unavailable,
            path: None,
            metrics: None,
            detail: Some(reason.into()),
        }
    }

    fn idle() -> Self {
        Self {
            phase: DecodePhase::Idle,
            path: None,
            metrics: None,
            detail: None,
        }
    }

    fn opening(path: PathBuf) -> Self {
        Self {
            phase: DecodePhase::Opening,
            path: Some(path),
            metrics: None,
            detail: None,
        }
    }

    fn seeking(path: PathBuf, mut metrics: DecodeMetrics, target_frame: u64) -> Self {
        metrics.target_frame = target_frame;
        metrics.buffered_frames = 0;
        Self {
            phase: DecodePhase::Seeking,
            path: Some(path),
            metrics: Some(metrics),
            detail: None,
        }
    }

    #[cfg(target_os = "windows")]
    pub(super) fn progress(phase: DecodePhase, path: PathBuf, metrics: DecodeMetrics) -> Self {
        debug_assert!(matches!(
            phase,
            DecodePhase::Buffering | DecodePhase::Ready | DecodePhase::EndOfStream
        ));
        Self {
            phase,
            path: Some(path),
            metrics: Some(metrics),
            detail: None,
        }
    }

    fn failed(path: PathBuf, error: impl Into<String>) -> Self {
        Self {
            phase: DecodePhase::Failed,
            path: Some(path),
            metrics: None,
            detail: Some(error.into()),
        }
    }

    pub const fn phase(&self) -> DecodePhase {
        self.phase
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub const fn metrics(&self) -> Option<&DecodeMetrics> {
        self.metrics.as_ref()
    }

    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpatialPosition {
    x: f32,
    y: f32,
    z: f32,
}

impl SpatialPosition {
    pub(super) const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    pub const fn x(self) -> f32 {
        self.x
    }

    pub const fn y(self) -> f32 {
        self.y
    }

    pub const fn z(self) -> f32 {
        self.z
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpatialObjectState {
    metadata_active: bool,
    position: Option<SpatialPosition>,
    linear_gain: Option<f32>,
    semantic_complete: bool,
}

impl SpatialObjectState {
    pub(super) const fn new(
        metadata_active: bool,
        position: Option<SpatialPosition>,
        linear_gain: Option<f32>,
        semantic_complete: bool,
    ) -> Self {
        Self {
            metadata_active,
            position,
            linear_gain,
            semantic_complete,
        }
    }

    pub const fn metadata_active(self) -> bool {
        self.metadata_active
    }

    pub const fn position(self) -> Option<SpatialPosition> {
        self.position
    }

    pub const fn linear_gain(self) -> Option<f32> {
        self.linear_gain
    }

    pub const fn semantic_complete(self) -> bool {
        self.semantic_complete
    }
}

#[derive(Debug)]
pub struct SceneObjectPcm {
    element_id: u64,
    initial_state: Option<SpatialObjectState>,
    samples: Vec<f32>,
}

impl SceneObjectPcm {
    #[cfg(any(target_os = "windows", test))]
    pub(super) fn new(
        element_id: u64,
        initial_state: Option<SpatialObjectState>,
        samples: Vec<f32>,
    ) -> Self {
        Self {
            element_id,
            initial_state,
            samples,
        }
    }

    pub const fn element_id(&self) -> u64 {
        self.element_id
    }

    pub const fn initial_state(&self) -> Option<SpatialObjectState> {
        self.initial_state
    }

    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
}

#[derive(Debug)]
pub struct SceneLfePcm {
    element_id: u64,
    initial_state: Option<SpatialObjectState>,
    samples: Vec<f32>,
}

impl SceneLfePcm {
    #[cfg(target_os = "windows")]
    pub(super) fn new(
        element_id: u64,
        initial_state: Option<SpatialObjectState>,
        samples: Vec<f32>,
    ) -> Self {
        Self {
            element_id,
            initial_state,
            samples,
        }
    }

    pub const fn element_id(&self) -> u64 {
        self.element_id
    }

    pub const fn initial_state(&self) -> Option<SpatialObjectState> {
        self.initial_state
    }

    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneMetadataUpdate {
    element_id: u64,
    offset_frames: u32,
    ramp_frames: u32,
    changed_fields: u32,
    state: SpatialObjectState,
}

impl SceneMetadataUpdate {
    #[cfg(any(target_os = "windows", test))]
    pub(super) const fn new(
        element_id: u64,
        offset_frames: u32,
        ramp_frames: u32,
        changed_fields: u32,
        state: SpatialObjectState,
    ) -> Self {
        Self {
            element_id,
            offset_frames,
            ramp_frames,
            changed_fields,
            state,
        }
    }

    pub const fn element_id(self) -> u64 {
        self.element_id
    }

    pub const fn offset_frames(self) -> u32 {
        self.offset_frames
    }

    pub const fn ramp_frames(self) -> u32 {
        self.ramp_frames
    }

    pub const fn changed_fields(self) -> u32 {
        self.changed_fields
    }

    pub const fn state(self) -> SpatialObjectState {
        self.state
    }
}

#[derive(Debug)]
pub struct DecodedSceneBlock {
    sample_rate: u32,
    start_frame: i64,
    duration_frames: u32,
    configuration_generation: u32,
    presentation_index: u32,
    presentation_id: Option<u32>,
    state_complete: bool,
    objects: Vec<SceneObjectPcm>,
    lfe: Option<SceneLfePcm>,
    metadata_updates: Vec<SceneMetadataUpdate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaybackKey {
    request_id: u64,
    epoch: u64,
}

impl PlaybackKey {
    pub(crate) const fn new(request_id: u64, epoch: u64) -> Self {
        Self { request_id, epoch }
    }

    #[cfg(target_os = "windows")]
    pub(super) const fn request_id(self) -> u64 {
        self.request_id
    }
}

impl DecodedSceneBlock {
    #[allow(clippy::too_many_arguments)]
    #[cfg(any(target_os = "windows", test))]
    pub(super) fn new(
        sample_rate: u32,
        start_frame: i64,
        duration_frames: u32,
        configuration_generation: u32,
        presentation_index: u32,
        presentation_id: Option<u32>,
        state_complete: bool,
        objects: Vec<SceneObjectPcm>,
        lfe: Option<SceneLfePcm>,
        metadata_updates: Vec<SceneMetadataUpdate>,
    ) -> Self {
        Self {
            sample_rate,
            start_frame,
            duration_frames,
            configuration_generation,
            presentation_index,
            presentation_id,
            state_complete,
            objects,
            lfe,
            metadata_updates,
        }
    }

    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub const fn start_frame(&self) -> i64 {
        self.start_frame
    }

    pub const fn duration_frames(&self) -> u32 {
        self.duration_frames
    }

    pub const fn configuration_generation(&self) -> u32 {
        self.configuration_generation
    }

    pub const fn presentation_index(&self) -> u32 {
        self.presentation_index
    }

    pub const fn presentation_id(&self) -> Option<u32> {
        self.presentation_id
    }

    pub const fn state_complete(&self) -> bool {
        self.state_complete
    }

    pub fn objects(&self) -> &[SceneObjectPcm] {
        &self.objects
    }

    pub const fn lfe(&self) -> Option<&SceneLfePcm> {
        self.lfe.as_ref()
    }

    pub fn metadata_updates(&self) -> &[SceneMetadataUpdate] {
        &self.metadata_updates
    }
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(super) struct QueueSnapshot {
    pub(super) buffered_frames: u64,
    pub(super) capacity_frames: u64,
}

#[derive(Debug)]
struct SceneQueueInner {
    key: PlaybackKey,
    sample_rate: u32,
    buffered_frames: u64,
    capacity_frames: u64,
    end_of_stream: bool,
    blocks: VecDeque<DecodedSceneBlock>,
}

#[derive(Debug, Clone)]
pub(super) struct SharedSceneQueue {
    state: Arc<(Mutex<SceneQueueInner>, Condvar)>,
}

#[derive(Debug)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(super) enum QueuePushError {
    Full(Box<DecodedSceneBlock>),
    Stale,
    Format(String),
}

impl SharedSceneQueue {
    fn new() -> Self {
        Self {
            state: Arc::new((
                Mutex::new(SceneQueueInner {
                    key: PlaybackKey::new(0, 0),
                    sample_rate: 0,
                    buffered_frames: 0,
                    capacity_frames: 0,
                    end_of_stream: false,
                    blocks: VecDeque::new(),
                }),
                Condvar::new(),
            )),
        }
    }

    fn reset(&self, key: PlaybackKey) {
        let (mutex, changed) = &*self.state;
        let mut queue = lock_recover(mutex);
        queue.key = key;
        queue.sample_rate = 0;
        queue.buffered_frames = 0;
        queue.capacity_frames = 0;
        queue.end_of_stream = false;
        queue.blocks.clear();
        changed.notify_all();
    }

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(super) fn try_push(
        &self,
        key: PlaybackKey,
        block: DecodedSceneBlock,
    ) -> Result<QueueSnapshot, QueuePushError> {
        let (mutex, _) = &*self.state;
        let mut queue = lock_recover(mutex);
        if queue.key != key {
            return Err(QueuePushError::Stale);
        }
        if queue.sample_rate == 0 {
            queue.sample_rate = block.sample_rate;
            queue.capacity_frames = u64::from(block.sample_rate).saturating_mul(MAX_BUFFER_SECONDS);
        } else if queue.sample_rate != block.sample_rate {
            return Err(QueuePushError::Format(format!(
                "Scene sample rate changed from {} to {} Hz",
                queue.sample_rate, block.sample_rate
            )));
        }

        let duration = u64::from(block.duration_frames);
        if queue.buffered_frames.saturating_add(duration) > queue.capacity_frames {
            return Err(QueuePushError::Full(Box::new(block)));
        }
        queue.buffered_frames = queue.buffered_frames.saturating_add(duration);
        queue.blocks.push_back(block);
        Ok(QueueSnapshot {
            buffered_frames: queue.buffered_frames,
            capacity_frames: queue.capacity_frames,
        })
    }

    fn try_pop(&self, key: PlaybackKey) -> Option<DecodedSceneBlock> {
        let (mutex, changed) = &*self.state;
        let mut queue = lock_recover(mutex);
        if queue.key != key {
            return None;
        }
        let block = queue.blocks.pop_front()?;
        queue.buffered_frames = queue
            .buffered_frames
            .saturating_sub(u64::from(block.duration_frames));
        changed.notify_all();
        Some(block)
    }

    #[cfg(any(target_os = "windows", test))]
    pub(super) fn mark_end_of_stream(&self, key: PlaybackKey) {
        let (mutex, changed) = &*self.state;
        let mut queue = lock_recover(mutex);
        if queue.key == key {
            queue.end_of_stream = true;
            changed.notify_all();
        }
    }

    #[cfg(any(target_os = "windows", test))]
    fn is_end_of_stream(&self, key: PlaybackKey) -> bool {
        let queue = lock_recover(&self.state.0);
        queue.key != key || (queue.end_of_stream && queue.blocks.is_empty())
    }

    #[cfg(target_os = "windows")]
    pub(super) fn wait_for_change(&self, timeout: Duration) {
        let (mutex, changed) = &*self.state;
        let queue = lock_recover(mutex);
        match changed.wait_timeout(queue, timeout) {
            Ok(_) | Err(_) => {}
        }
    }

    fn wake_all(&self) {
        self.state.1.notify_all();
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(
    not(target_os = "windows"),
    allow(
        dead_code,
        reason = "the Scene reader is consumed only by the Windows output adapter"
    )
)]
pub(crate) struct SceneQueueReader {
    queue: SharedSceneQueue,
    key: PlaybackKey,
}

impl SceneQueueReader {
    #[cfg(target_os = "windows")]
    pub(crate) fn try_pop(&self) -> Option<DecodedSceneBlock> {
        self.queue.try_pop(self.key)
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn is_end_of_stream(&self) -> bool {
        self.queue.is_end_of_stream(self.key)
    }

    /// The playback this reader is bound to.
    ///
    /// The render source stamps everything it mirrors to the UI with this, so
    /// the view is gated by exactly the key the FIFO already enforces rather
    /// than by a second, parallel notion of freshness.
    #[cfg(target_os = "windows")]
    pub(crate) const fn playback_key(&self) -> PlaybackKey {
        self.key
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(super) enum WorkerCommand {
    Open { key: PlaybackKey, path: PathBuf },
    Seek { key: PlaybackKey, target_frame: u64 },
    Close,
    Shutdown,
}

#[derive(Debug)]
pub(super) struct WorkerEvent {
    pub(super) kind: WorkerEventKind,
}

#[derive(Debug)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(super) enum WorkerEventKind {
    Snapshot {
        key: PlaybackKey,
        snapshot: Box<DecoderSnapshot>,
    },
    IndexFinished {
        request_id: u64,
        duration_frames: Option<u64>,
        seekable_from_frame: Option<u64>,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ControllerIndexState {
    Inactive,
    Building,
    Ready {
        duration_frames: Option<u64>,
        seekable_from_frame: Option<u64>,
    },
    Failed(String),
}

pub(super) struct WorkerHandle {
    command_sender: Sender<WorkerCommand>,
    event_receiver: Receiver<WorkerEvent>,
    join_handle: JoinHandle<()>,
}

pub struct DecoderController {
    command_sender: Option<Sender<WorkerCommand>>,
    event_receiver: Option<Receiver<WorkerEvent>>,
    join_handle: Option<JoinHandle<()>>,
    queue: SharedSceneQueue,
    snapshot: DecoderSnapshot,
    active_path: Option<PathBuf>,
    request_id: u64,
    playback_epoch: u64,
    index_state: ControllerIndexState,
    revision: u64,
}

impl DecoderController {
    pub fn new() -> Self {
        let queue = SharedSceneQueue::new();
        #[cfg(target_os = "windows")]
        let worker = windows::spawn(queue.clone());
        #[cfg(not(target_os = "windows"))]
        let worker: Result<WorkerHandle, String> = Err(
            "Audio decoding is enabled on Windows first; this platform keeps inspection only"
                .to_owned(),
        );

        match worker {
            Ok(worker) => Self {
                command_sender: Some(worker.command_sender),
                event_receiver: Some(worker.event_receiver),
                join_handle: Some(worker.join_handle),
                queue,
                snapshot: DecoderSnapshot::idle(),
                active_path: None,
                request_id: 0,
                playback_epoch: 0,
                index_state: ControllerIndexState::Inactive,
                revision: 0,
            },
            Err(error) => Self {
                command_sender: None,
                event_receiver: None,
                join_handle: None,
                queue,
                snapshot: DecoderSnapshot::unavailable(error),
                active_path: None,
                request_id: 0,
                playback_epoch: 0,
                index_state: ControllerIndexState::Inactive,
                revision: 0,
            },
        }
    }

    pub fn ensure_open(&mut self, path: &Path) {
        if self.active_path.as_deref() == Some(path) {
            return;
        }
        self.active_path = Some(path.to_path_buf());
        if self.command_sender.is_none() {
            return;
        }
        self.advance_request();
        self.playback_epoch = 0;
        self.index_state = ControllerIndexState::Building;
        let key = self.playback_key();
        self.queue.reset(key);
        self.snapshot = DecoderSnapshot::opening(path.to_path_buf());
        self.revision = self.revision.saturating_add(1);
        let command = WorkerCommand::Open {
            key,
            path: path.to_path_buf(),
        };
        if self
            .command_sender
            .as_ref()
            .is_none_or(|sender| sender.send(command).is_err())
        {
            self.snapshot = DecoderSnapshot::failed(
                path.to_path_buf(),
                "MacinDecode Core worker stopped unexpectedly",
            );
            self.revision = self.revision.saturating_add(1);
        }
    }

    pub fn close(&mut self) {
        if self.active_path.take().is_none() {
            return;
        }
        self.advance_request();
        self.playback_epoch = 0;
        self.index_state = ControllerIndexState::Inactive;
        self.queue.reset(self.playback_key());
        if let Some(sender) = self.command_sender.as_ref() {
            let _ = sender.send(WorkerCommand::Close);
            self.snapshot = DecoderSnapshot::idle();
            self.revision = self.revision.saturating_add(1);
        }
    }

    pub fn poll(&mut self) {
        let Some(receiver) = self.event_receiver.as_ref() else {
            return;
        };
        while let Ok(event) = receiver.try_recv() {
            match event.kind {
                WorkerEventKind::Snapshot { key, snapshot } => {
                    if key != self.playback_key() {
                        continue;
                    }
                    let mut snapshot = *snapshot;
                    inherit_scene_identity(&self.snapshot, &mut snapshot);
                    apply_index_state(&self.index_state, &mut snapshot);
                    self.snapshot = snapshot;
                    self.revision = self.revision.saturating_add(1);
                }
                WorkerEventKind::IndexFinished {
                    request_id,
                    duration_frames,
                    seekable_from_frame,
                    error,
                } => {
                    if request_id != self.request_id {
                        continue;
                    }
                    self.index_state = error.map_or_else(
                        || ControllerIndexState::Ready {
                            duration_frames,
                            seekable_from_frame,
                        },
                        ControllerIndexState::Failed,
                    );
                    apply_index_state(&self.index_state, &mut self.snapshot);
                    self.revision = self.revision.saturating_add(1);
                }
            }
        }
    }

    pub const fn snapshot(&self) -> &DecoderSnapshot {
        &self.snapshot
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn active_path(&self) -> Option<&Path> {
        self.active_path.as_deref()
    }

    pub fn is_working(&self) -> bool {
        matches!(
            self.snapshot.phase,
            DecodePhase::Opening
                | DecodePhase::Seeking
                | DecodePhase::Buffering
                | DecodePhase::Ready
        )
    }

    pub fn try_pop_scene_block(&self) -> Option<DecodedSceneBlock> {
        self.queue.try_pop(self.playback_key())
    }

    pub(crate) fn scene_reader(&self) -> SceneQueueReader {
        SceneQueueReader {
            queue: self.queue.clone(),
            key: self.playback_key(),
        }
    }

    pub const fn request_id(&self) -> u64 {
        self.request_id
    }

    pub const fn playback_epoch(&self) -> u64 {
        self.playback_epoch
    }

    pub(crate) const fn playback_key(&self) -> PlaybackKey {
        PlaybackKey::new(self.request_id, self.playback_epoch)
    }

    /// Starts a new playback epoch at an absolute presentation frame without rereading the file.
    ///
    /// # Errors
    ///
    /// Returns an error when no source/index is available, the target exceeds the duration, or
    /// no complete random-access point exists at or before the requested frame.
    pub fn seek(&mut self, target_frame: u64) -> Result<(), String> {
        let path = self
            .active_path
            .clone()
            .ok_or_else(|| "No active media is loaded".to_owned())?;
        let metrics = self
            .snapshot
            .metrics()
            .cloned()
            .ok_or_else(|| "Media indexing has not completed yet".to_owned())?;
        if metrics.is_indexing() {
            return Err("Media indexing has not completed yet".to_owned());
        }
        if let Some(error) = metrics.index_error() {
            return Err(format!("Media seek index failed: {error}"));
        }
        if !metrics.can_seek_to(target_frame) {
            return Err(match metrics.duration_frames() {
                Some(duration) if target_frame > duration => {
                    format!("Seek target {target_frame} exceeds duration {duration}")
                }
                _ => {
                    "No complete random-access point exists at or before the seek target".to_owned()
                }
            });
        }
        let sender = self
            .command_sender
            .as_ref()
            .ok_or_else(|| "The Windows decoder is unavailable".to_owned())?;
        self.playback_epoch = self.playback_epoch.checked_add(1).unwrap_or(1);
        let key = self.playback_key();
        self.queue.reset(key);
        self.snapshot = DecoderSnapshot::seeking(path, metrics, target_frame);
        self.revision = self.revision.saturating_add(1);
        sender
            .send(WorkerCommand::Seek { key, target_frame })
            .map_err(|_| "MacinDecode Core worker stopped unexpectedly".to_owned())
    }

    pub fn reopen(&mut self) {
        let _ = self.seek(0);
    }

    fn advance_request(&mut self) {
        self.request_id = self.request_id.checked_add(1).unwrap_or(1);
    }
}

fn apply_index_state(state: &ControllerIndexState, snapshot: &mut DecoderSnapshot) {
    let Some(metrics) = snapshot.metrics.as_mut() else {
        return;
    };
    match state {
        ControllerIndexState::Inactive => {
            metrics.indexing = false;
            metrics.index_error = None;
        }
        ControllerIndexState::Building => {
            metrics.indexing = true;
            metrics.index_error = None;
        }
        ControllerIndexState::Ready {
            duration_frames,
            seekable_from_frame,
        } => {
            metrics.indexing = false;
            metrics.index_error = None;
            if duration_frames.is_some() {
                metrics.duration_frames = *duration_frames;
            }
            metrics.seekable_from_frame = *seekable_from_frame;
        }
        ControllerIndexState::Failed(error) => {
            metrics.indexing = false;
            metrics.index_error = Some(error.clone());
            metrics.seekable_from_frame = None;
        }
    }
}

fn inherit_scene_identity(previous: &DecoderSnapshot, next: &mut DecoderSnapshot) {
    let (Some(previous), Some(next)) = (previous.metrics(), next.metrics.as_mut()) else {
        return;
    };
    if next.scene_signature.is_some() {
        return;
    }
    next.presentation_index = previous.presentation_index;
    next.presentation_id = previous.presentation_id;
    next.object_count = previous.object_count;
    next.has_lfe = previous.has_lfe;
    next.state_complete = previous.state_complete;
    next.scene_signature.clone_from(&previous.scene_signature);
}

impl Default for DecoderController {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DecoderController {
    fn drop(&mut self) {
        if let Some(sender) = self.command_sender.take() {
            let _ = sender.send(WorkerCommand::Shutdown);
        }
        self.queue.wake_all();
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(sample_rate: u32, duration_frames: u32) -> DecodedSceneBlock {
        DecodedSceneBlock::new(
            sample_rate,
            0,
            duration_frames,
            1,
            0,
            None,
            true,
            Vec::new(),
            None,
            Vec::new(),
        )
    }

    fn indexed_metrics() -> DecodeMetrics {
        DecodeMetrics {
            container: DecodeContainer::RawAc4,
            sample_rate: 48_000,
            presentation_index: 0,
            presentation_id: None,
            object_count: 1,
            has_lfe: false,
            state_complete: true,
            scene_signature: None,
            decoded_access_units: 0,
            decoded_scene_frames: 0,
            decoded_frames: 0,
            buffered_frames: 0,
            buffer_capacity_frames: 96_000,
            metadata_updates: 0,
            duration_frames: Some(480_000),
            seekable_from_frame: Some(48_000),
            indexing: false,
            index_error: None,
            target_frame: 0,
        }
    }

    #[test]
    fn scene_queue_is_bounded_to_two_seconds_and_pop_releases_space() {
        let queue = SharedSceneQueue::new();
        let key = PlaybackKey::new(7, 1);
        queue.reset(key);
        let first = queue
            .try_push(key, block(48_000, 48_000))
            .expect("first second should fit");
        assert_eq!(first.buffered_frames, 48_000);
        assert_eq!(first.capacity_frames, 96_000);
        assert!(queue.try_push(key, block(48_000, 48_000)).is_ok());
        match queue.try_push(key, block(48_000, 1)) {
            Err(QueuePushError::Full(returned)) => assert_eq!(returned.duration_frames(), 1),
            other => panic!("expected a full queue, got {other:?}"),
        }

        assert_eq!(
            queue.try_pop(key).map(|item| item.duration_frames()),
            Some(48_000)
        );
        assert!(queue.try_push(key, block(48_000, 1)).is_ok());
    }

    #[test]
    fn scene_reader_reports_end_only_after_queued_blocks_are_drained() {
        let queue = SharedSceneQueue::new();
        let key = PlaybackKey::new(9, 2);
        queue.reset(key);
        assert!(queue.try_push(key, block(48_000, 2_048)).is_ok());
        queue.mark_end_of_stream(key);
        assert!(!queue.is_end_of_stream(key));
        assert!(queue.try_pop(key).is_some());
        assert!(queue.is_end_of_stream(key));
    }

    #[test]
    fn reset_invalidates_frames_from_an_older_playback_epoch() {
        let queue = SharedSceneQueue::new();
        let old = PlaybackKey::new(2, 4);
        let current = PlaybackKey::new(2, 5);
        queue.reset(current);
        assert!(matches!(
            queue.try_push(old, block(48_000, 2_048)),
            Err(QueuePushError::Stale)
        ));
        assert!(queue.try_push(current, block(48_000, 2_048)).is_ok());
    }

    #[test]
    fn queue_rejects_a_midstream_sample_rate_change() {
        let queue = SharedSceneQueue::new();
        let key = PlaybackKey::new(4, 0);
        queue.reset(key);
        assert!(queue.try_push(key, block(48_000, 2_048)).is_ok());
        match queue.try_push(key, block(44_100, 2_048)) {
            Err(QueuePushError::Format(message)) => {
                assert!(message.contains("48000"));
                assert!(message.contains("44100"));
            }
            other => panic!("expected a format error, got {other:?}"),
        }
    }

    #[test]
    fn milliseconds_are_derived_from_integer_frames() {
        assert_eq!(frames_to_milliseconds(14_400, 48_000), 300);
        assert_eq!(frames_to_milliseconds(96_000, 48_000), 2_000);
        assert_eq!(frames_to_milliseconds(1, 0), 0);
    }

    #[test]
    fn seekability_requires_a_safe_point_at_or_before_the_target() {
        let metrics = indexed_metrics();
        assert!(!metrics.can_seek_to(47_999));
        assert!(metrics.can_seek_to(48_000));
        assert!(metrics.can_seek_to(480_000));
        assert!(!metrics.can_seek_to(480_001));
    }

    #[test]
    fn asynchronous_index_state_survives_later_decode_snapshots() {
        let mut snapshot = DecoderSnapshot {
            phase: DecodePhase::Ready,
            path: Some(PathBuf::from("cached.mp4")),
            metrics: Some(indexed_metrics()),
            detail: None,
        };
        apply_index_state(&ControllerIndexState::Building, &mut snapshot);
        let metrics = snapshot.metrics().expect("metrics while indexing");
        assert!(metrics.is_indexing());
        assert!(!metrics.can_seek_to(48_000));

        apply_index_state(
            &ControllerIndexState::Ready {
                duration_frames: Some(960_000),
                seekable_from_frame: Some(0),
            },
            &mut snapshot,
        );
        let metrics = snapshot.metrics().expect("metrics after indexing");
        assert!(!metrics.is_indexing());
        assert_eq!(metrics.duration_frames(), Some(960_000));
        assert!(metrics.can_seek_to(1));
    }
}
