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
    Buffering,
    Ready,
    EndOfStream,
    Failed,
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
    pub(super) decoded_access_units: u64,
    pub(super) decoded_scene_frames: u64,
    pub(super) decoded_frames: u64,
    pub(super) buffered_frames: u64,
    pub(super) buffer_capacity_frames: u64,
    pub(super) metadata_updates: u64,
    pub(super) duration_frames: Option<u64>,
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
    #[cfg(target_os = "windows")]
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
    #[cfg(target_os = "windows")]
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

#[derive(Debug)]
pub struct SceneLfePcm {
    element_id: u64,
    samples: Vec<f32>,
}

impl SceneLfePcm {
    #[cfg(target_os = "windows")]
    pub(super) fn new(element_id: u64, samples: Vec<f32>) -> Self {
        Self {
            element_id,
            samples,
        }
    }

    pub const fn element_id(&self) -> u64 {
        self.element_id
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
    #[cfg(target_os = "windows")]
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
    request_id: u64,
    sample_rate: u32,
    buffered_frames: u64,
    capacity_frames: u64,
    blocks: VecDeque<DecodedSceneBlock>,
}

#[derive(Debug, Clone)]
pub(super) struct SharedSceneQueue {
    state: Arc<(Mutex<SceneQueueInner>, Condvar)>,
}

#[derive(Debug)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(super) enum QueuePushError {
    Full(DecodedSceneBlock),
    Stale,
    Format(String),
}

impl SharedSceneQueue {
    fn new() -> Self {
        Self {
            state: Arc::new((
                Mutex::new(SceneQueueInner {
                    request_id: 0,
                    sample_rate: 0,
                    buffered_frames: 0,
                    capacity_frames: 0,
                    blocks: VecDeque::new(),
                }),
                Condvar::new(),
            )),
        }
    }

    fn reset(&self, request_id: u64) {
        let (mutex, changed) = &*self.state;
        let mut queue = lock_recover(mutex);
        queue.request_id = request_id;
        queue.sample_rate = 0;
        queue.buffered_frames = 0;
        queue.capacity_frames = 0;
        queue.blocks.clear();
        changed.notify_all();
    }

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(super) fn try_push(
        &self,
        request_id: u64,
        block: DecodedSceneBlock,
    ) -> Result<QueueSnapshot, QueuePushError> {
        let (mutex, _) = &*self.state;
        let mut queue = lock_recover(mutex);
        if queue.request_id != request_id {
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
            return Err(QueuePushError::Full(block));
        }
        queue.buffered_frames = queue.buffered_frames.saturating_add(duration);
        queue.blocks.push_back(block);
        Ok(QueueSnapshot {
            buffered_frames: queue.buffered_frames,
            capacity_frames: queue.capacity_frames,
        })
    }

    fn try_pop(&self) -> Option<DecodedSceneBlock> {
        let (mutex, changed) = &*self.state;
        let mut queue = lock_recover(mutex);
        let block = queue.blocks.pop_front()?;
        queue.buffered_frames = queue
            .buffered_frames
            .saturating_sub(u64::from(block.duration_frames));
        changed.notify_all();
        Some(block)
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

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(super) enum WorkerCommand {
    Open { request_id: u64, path: PathBuf },
    Close,
    Shutdown,
}

#[derive(Debug)]
pub(super) struct WorkerEvent {
    request_id: u64,
    snapshot: DecoderSnapshot,
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
        self.queue.reset(self.request_id);
        self.snapshot = DecoderSnapshot::opening(path.to_path_buf());
        self.revision = self.revision.saturating_add(1);
        let command = WorkerCommand::Open {
            request_id: self.request_id,
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
        self.queue.reset(self.request_id);
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
            if event.request_id != self.request_id {
                continue;
            }
            self.snapshot = event.snapshot;
            self.revision = self.revision.saturating_add(1);
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
            DecodePhase::Opening | DecodePhase::Buffering | DecodePhase::Ready
        )
    }

    pub fn try_pop_scene_block(&self) -> Option<DecodedSceneBlock> {
        self.queue.try_pop()
    }

    fn advance_request(&mut self) {
        self.request_id = self.request_id.checked_add(1).unwrap_or(1);
    }
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

    #[test]
    fn scene_queue_is_bounded_to_two_seconds_and_pop_releases_space() {
        let queue = SharedSceneQueue::new();
        queue.reset(7);
        let first = queue
            .try_push(7, block(48_000, 48_000))
            .expect("first second should fit");
        assert_eq!(first.buffered_frames, 48_000);
        assert_eq!(first.capacity_frames, 96_000);
        assert!(queue.try_push(7, block(48_000, 48_000)).is_ok());
        match queue.try_push(7, block(48_000, 1)) {
            Err(QueuePushError::Full(returned)) => assert_eq!(returned.duration_frames(), 1),
            other => panic!("expected a full queue, got {other:?}"),
        }

        assert_eq!(
            queue.try_pop().map(|item| item.duration_frames()),
            Some(48_000)
        );
        assert!(queue.try_push(7, block(48_000, 1)).is_ok());
    }

    #[test]
    fn reset_invalidates_frames_from_an_older_decode_request() {
        let queue = SharedSceneQueue::new();
        queue.reset(2);
        assert!(matches!(
            queue.try_push(1, block(48_000, 2_048)),
            Err(QueuePushError::Stale)
        ));
        assert!(queue.try_push(2, block(48_000, 2_048)).is_ok());
    }

    #[test]
    fn queue_rejects_a_midstream_sample_rate_change() {
        let queue = SharedSceneQueue::new();
        queue.reset(4);
        assert!(queue.try_push(4, block(48_000, 2_048)).is_ok());
        match queue.try_push(4, block(44_100, 2_048)) {
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
}
