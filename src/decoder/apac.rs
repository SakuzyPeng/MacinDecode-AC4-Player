//! APAC playback stays on the existing decode worker and bounded Scene FIFO.
use apac_container::{IndexBatch, Media, Playback, PlaybackOptions};

use crate::apac::{Destination, Layout, Source};

use super::{
    DECODE_STACK_BYTES, RunControl, enqueue_block, initial_metrics, lock_recover, pending_command,
    send_failure, send_progress,
};
use crate::decoder::{
    DecodeContainer, DecodePhase, DecodedSceneBlock, PlaybackKey, SceneLfePcm, SceneObjectPcm,
    SceneSignature, SharedSceneQueue, SpatialObjectState, SpatialPosition, WorkerCommand,
    WorkerEvent, WorkerEventKind,
};
use crate::media::MediaSource;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
};
use std::thread;

pub(super) struct Stream {
    pub(super) path: std::path::PathBuf,
    playback: Playback<Source>,
    layout: Layout,
    container: DecodeContainer,
    sample_rate: u32,
    active_key: Arc<Mutex<PlaybackKey>>,
    cancel_index: Arc<AtomicBool>,
    index: Receiver<IndexBatch>,
}

impl Stream {
    pub(super) fn open(
        key: PlaybackKey,
        source: &MediaSource,
        events: &Sender<WorkerEvent>,
        queue: &SharedSceneQueue,
    ) -> Result<Self, String> {
        let opened = source.open()?;
        let active_key = Arc::new(Mutex::new(key));
        let read_key = Arc::clone(&active_key);
        let read_queue = queue.clone();
        let input = Source::new(
            &opened,
            Arc::new(move || read_queue.snapshot(*lock_recover(&read_key)).is_none()),
        );
        let media = Media::open(input).map_err(|error| error.to_string())?;
        let container = if media.container() == "caf" {
            DecodeContainer::ApacCaf
        } else {
            DecodeContainer::ApacMp4
        };
        let layout = Layout::from_core(&media.track().layout, media.track().channels)?;
        // The background indexer holds one bounded batch. Pick its interval
        // from the packet count so even a paused, hours-long file stays small.
        let interval = media.track().packet_count.div_ceil(256).max(64);
        let playback = Playback::open_with(
            media,
            PlaybackOptions {
                checkpoint_interval: interval,
                max_checkpoints: 258,
            },
        )
        .map_err(|error| error.to_string())?;
        let sample_rate = u32::try_from(playback.decoder().info().sample_rate_hz)
            .map_err(|_| "APAC sample rate exceeds u32")?;
        let duration = playback.frames();
        i64::try_from(duration).map_err(|_| "APAC duration exceeds the player timeline")?;
        let cancel_index = Arc::new(AtomicBool::new(false));
        let read_cancel = Arc::clone(&cancel_index);
        let mut indexer = playback
            .indexer(Source::new(
                &opened,
                Arc::new(move || read_cancel.load(Ordering::Acquire)),
            ))
            .map_err(|error| error.to_string())?;
        let (send, index) = mpsc::channel();
        let worker_cancel = Arc::clone(&cancel_index);
        let worker_events = events.clone();
        thread::Builder::new()
            .name("apac-seek-index".into())
            .stack_size(DECODE_STACK_BYTES)
            .spawn(move || {
                let result = loop {
                    if worker_cancel.load(Ordering::Acquire) {
                        return;
                    }
                    match indexer.run(16) {
                        Ok(false) => {}
                        Ok(true) => break Ok(indexer.take_batch()),
                        Err(error) => break Err(error.to_string()),
                    }
                };
                if worker_cancel.load(Ordering::Acquire) {
                    return;
                }
                let error = match result {
                    Ok(batch) => {
                        if send.send(batch).is_err() {
                            return;
                        }
                        None
                    }
                    Err(error) => Some(error),
                };
                let _ = worker_events.send(WorkerEvent {
                    kind: WorkerEventKind::IndexFinished {
                        request_id: key.request_id(),
                        duration_frames: Some(duration),
                        seekable_from_frame: error.is_none().then_some(0),
                        error,
                    },
                });
            })
            .map_err(|error| format!("Failed to start APAC indexer: {error}"))?;
        Ok(Self {
            path: source.path().to_path_buf(),
            playback,
            layout,
            container,
            sample_rate,
            active_key,
            cancel_index,
            index,
        })
    }

    pub(super) fn decode_from(
        &mut self,
        key: PlaybackKey,
        target: u64,
        commands: &Receiver<WorkerCommand>,
        events: &Sender<WorkerEvent>,
        queue: &SharedSceneQueue,
    ) -> RunControl {
        *lock_recover(&self.active_key) = key;
        match self.decode(key, target, commands, events, queue) {
            Ok(control) => control,
            Err(_) if queue.snapshot(key).is_none() => {
                pending_command(commands).unwrap_or(RunControl::Complete)
            }
            Err(error) => {
                send_failure(key, Some(&self.path), error, events);
                RunControl::Complete
            }
        }
    }

    fn merge_index(&mut self) -> Result<(), String> {
        while let Ok(batch) = self.index.try_recv() {
            self.playback
                .merge_index(batch)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn decode(
        &mut self,
        key: PlaybackKey,
        target: u64,
        commands: &Receiver<WorkerCommand>,
        events: &Sender<WorkerEvent>,
        queue: &SharedSceneQueue,
    ) -> Result<RunControl, String> {
        self.merge_index()?;
        self.playback
            .seek(target)
            .map_err(|error| error.to_string())?;
        let mut metrics = initial_metrics(
            self.container,
            self.sample_rate,
            Some(self.playback.frames()),
            Some(0),
            !self.playback.index_complete(),
            target,
        );
        // Publish topology even for an inclusive endpoint seek with no PCM.
        let empty = scene_block(self.layout, self.sample_rate, target, &[])?;
        metrics.object_count = empty.objects().len();
        metrics.has_lfe = !empty.lfes().is_empty();
        metrics.state_complete = true;
        metrics.scene_signature = Some(SceneSignature::from_block(&empty));
        metrics.decoded_frames = target;
        let mut pcm = vec![0.0; 1024 * self.layout.channels.len()];
        loop {
            if let Some(control) = pending_command(commands) {
                return Ok(control);
            }
            if queue.snapshot(key).is_none() {
                return Ok(RunControl::Complete);
            }
            self.merge_index()?;
            let start = self.playback.position();
            let frames = self
                .playback
                .read(&mut pcm)
                .map_err(|error| format!("APAC decode: {error}"))?;
            if frames == 0 {
                queue.mark_end_of_stream(key);
                send_progress(
                    key,
                    Some(&self.path),
                    DecodePhase::EndOfStream,
                    &metrics,
                    events,
                );
                return Ok(RunControl::Complete);
            }
            let block = scene_block(
                self.layout,
                self.sample_rate,
                start,
                &pcm[..frames * self.layout.channels.len()],
            )?;
            metrics.decoded_access_units += 1;
            metrics.decoded_scene_frames += frames as u64;
            metrics.decoded_frames = self.playback.position();
            metrics.indexing = !self.playback.index_complete();
            match enqueue_block(
                key,
                Some(&self.path),
                block,
                &mut metrics,
                commands,
                events,
                queue,
            )? {
                RunControl::Complete => {}
                control => return Ok(control),
            }
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.cancel_index.store(true, Ordering::Release);
    }
}

fn scene_block(
    layout: Layout,
    rate: u32,
    start: u64,
    pcm: &[f32],
) -> Result<DecodedSceneBlock, String> {
    let channels = layout.channels.len();
    if !pcm.len().is_multiple_of(channels) || pcm.iter().any(|sample| !sample.is_finite()) {
        return Err("APAC produced invalid interleaved PCM".into());
    }
    let frames = u32::try_from(pcm.len() / channels).map_err(|_| "APAC block is too long")?;
    let start = i64::try_from(start).map_err(|_| "APAC timestamp exceeds i64")?;
    let mut objects = Vec::new();
    let mut lfes = Vec::new();
    for (channel, description) in layout.channels.iter().enumerate() {
        let samples = pcm
            .chunks_exact(channels)
            .map(|frame| frame[channel])
            .collect();
        let id = channel as u64 + 1;
        match description.destination {
            Destination::Speaker { azimuth, elevation } => {
                let (azimuth, elevation) = (azimuth.to_radians(), elevation.to_radians());
                // ADM x points right, y forwards, z upwards. Apple positive
                // azimuth is left, so the horizontal sine has the opposite sign.
                let position = SpatialPosition::new(
                    -azimuth.sin() * elevation.cos(),
                    azimuth.cos() * elevation.cos(),
                    elevation.sin(),
                );
                objects.push(SceneObjectPcm::new(
                    id,
                    Some(SpatialObjectState::new(
                        true,
                        Some(position),
                        Some(1.0),
                        true,
                    )),
                    samples,
                ));
            }
            Destination::Lfe => lfes.push(SceneLfePcm::new(
                id,
                Some(SpatialObjectState::new(true, None, Some(1.0), true)),
                samples,
            )),
        }
    }
    Ok(
        DecodedSceneBlock::new(rate, start, frames, 0, 0, None, true, objects, None, vec![])
            .with_lfes(lfes),
    )
}

#[cfg(test)]
#[path = "apac_tests.rs"]
mod tests;
