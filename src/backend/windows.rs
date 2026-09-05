use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use macindecode_windows_spatial_audio::{
    OutputDeviceSelection as NativeDeviceSelection, RenderPhase, RenderSnapshot, Renderer,
    StreamConfig,
};

use super::source::SceneRenderSource;
use super::{
    OutputDeviceInfo, OutputDeviceSelection, OutputPhase, OutputSnapshot, OutputStreamConfig,
};
use crate::decoder::SceneQueueReader;
use crate::scene_view::SceneViewMirror;

pub(super) fn spawn(
    config: &OutputStreamConfig,
    reader: SceneQueueReader,
    mirror: Arc<SceneViewMirror>,
    pose: Arc<crate::head_tracking::PoseMirror>,
) -> Result<Renderer, String> {
    Renderer::spawn(
        StreamConfig {
            sample_rate: config.sample_rate,
            dynamic_object_count: config.dynamic_object_count,
            has_lfe: config.has_lfe,
            start_frame: config.start_frame,
            output_device: native_selection(&config.output_device),
        },
        Box::new(
            SceneRenderSource::new(
                reader,
                mirror,
                config.sample_rate,
                config.dynamic_object_count,
                config.has_lfe,
                config.scene_signature.clone(),
                config.start_frame,
            )
            .with_pose(pose),
        ),
    )
}

pub(super) fn replace_source(
    renderer: &Renderer,
    config: &OutputStreamConfig,
    reader: SceneQueueReader,
    mirror: Arc<SceneViewMirror>,
    pose: Arc<crate::head_tracking::PoseMirror>,
) -> Result<(), String> {
    renderer.replace_source(
        Box::new(
            SceneRenderSource::new(
                reader,
                mirror,
                config.sample_rate,
                config.dynamic_object_count,
                config.has_lfe,
                config.scene_signature.clone(),
                config.start_frame,
            )
            .with_pose(pose),
        ),
        config.start_frame,
    )
}

fn native_selection(selection: &OutputDeviceSelection) -> NativeDeviceSelection {
    match selection {
        OutputDeviceSelection::SystemDefault => NativeDeviceSelection::SystemDefault,
        OutputDeviceSelection::EndpointId(id) => NativeDeviceSelection::EndpointId(id.clone()),
    }
}

pub(super) struct DeviceCatalogWorker {
    receiver: Receiver<Result<Vec<OutputDeviceInfo>, String>>,
    stop_sender: Sender<()>,
    join_handle: Option<JoinHandle<()>>,
}

impl DeviceCatalogWorker {
    pub(super) fn spawn() -> Self {
        let (event_sender, receiver) = mpsc::channel();
        let (stop_sender, stop_receiver) = mpsc::channel();
        let join_handle = thread::Builder::new()
            .name("windows-audio-device-catalog".to_owned())
            .spawn(move || {
                loop {
                    let update = macindecode_windows_spatial_audio::enumerate_output_devices().map(
                        |devices| {
                            devices
                                .into_iter()
                                .map(|device| OutputDeviceInfo {
                                    id: device.id,
                                    label: device.label,
                                    is_default: device.is_default,
                                    max_dynamic_objects: device.max_dynamic_objects,
                                    spatial_error: device.spatial_error,
                                })
                                .collect()
                        },
                    );
                    if event_sender.send(update).is_err() {
                        break;
                    }
                    match stop_receiver.recv_timeout(Duration::from_secs(2)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })
            .ok();
        Self {
            receiver,
            stop_sender,
            join_handle,
        }
    }

    pub(super) fn poll(&self) -> Option<Result<Vec<OutputDeviceInfo>, String>> {
        let mut latest = None;
        loop {
            match self.receiver.try_recv() {
                Ok(update) => latest = Some(update),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return latest,
            }
        }
    }
}

impl Drop for DeviceCatalogWorker {
    fn drop(&mut self) {
        let _ = self.stop_sender.send(());
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

pub(super) fn snapshot(native: RenderSnapshot) -> OutputSnapshot {
    OutputSnapshot {
        queued_output_frames: None,
        clock: super::OutputClock::Callback,
        phase: match native.phase {
            RenderPhase::Initializing => OutputPhase::Initializing,
            RenderPhase::Ready => OutputPhase::Ready,
            RenderPhase::Playing => OutputPhase::Playing,
            RenderPhase::Paused => OutputPhase::Paused,
            RenderPhase::Ended => OutputPhase::Ended,
            RenderPhase::Failed => OutputPhase::Failed,
        },
        device_label: native.device_label,
        max_dynamic_objects: native.max_dynamic_objects,
        reserved_dynamic_objects: native.reserved_dynamic_objects,
        active_dynamic_objects: native.active_dynamic_objects,
        render_updates: native.render_updates,
        submitted_frames: native.submitted_frames,
        playhead_frames: native.playhead_frames,
        object_buffer_submissions: native.object_buffer_submissions,
        position_updates: native.position_updates,
        underruns: native.underruns,
        error: native.error,
        // A native renderer is by definition not the preview: this snapshot is
        // reporting a real audio endpoint that was actually handed samples.
        preview: false,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::super::{
        OutputDeviceSelection, OutputPhase, OutputStreamConfig, SpatialOutputController,
    };
    use crate::decoder::{DecodePhase, DecoderController};

    #[test]
    #[ignore = "requires MACINDECODE_AC4_TEST_MEDIA and a Spatial Audio-capable default endpoint"]
    fn submits_decoded_scene_to_windows_spatial_audio() {
        let path = std::env::var_os("MACINDECODE_AC4_TEST_MEDIA")
            .map(PathBuf::from)
            .expect("set MACINDECODE_AC4_TEST_MEDIA");
        let mut decoder = DecoderController::new();
        decoder.ensure_open(&path);
        wait_for_decode_ready(&mut decoder);

        let metrics = decoder
            .snapshot()
            .metrics()
            .expect("decode metrics")
            .clone();
        let config = OutputStreamConfig::new(
            decoder.request_id(),
            decoder.playback_epoch(),
            metrics.target_frame(),
            metrics.sample_rate(),
            metrics
                .scene_signature()
                .cloned()
                .expect("ready Scene signature"),
            OutputDeviceSelection::SystemDefault,
        )
        .expect("valid Spatial Audio config");
        let mut output = SpatialOutputController::new();
        output.ensure_configured(&config, decoder.scene_reader());
        wait_for_output_ready(&mut output);
        let baseline_updates = output.snapshot().render_updates();
        output.play();

        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            decoder.poll();
            output.poll();
            let state = output.snapshot();
            assert!(
                state.phase() != OutputPhase::Failed,
                "Spatial Audio failed: {}",
                state.error().unwrap_or("unknown error")
            );
            if state.submitted_frames() > 0
                && state.active_dynamic_objects()
                    == u32::try_from(metrics.object_count()).unwrap_or(u32::MAX)
                && state.render_updates() >= baseline_updates.saturating_add(20)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Spatial Audio did not submit 20 render updates in time: {state:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }

        output.pause();
        wait_for_output_phase(&mut output, OutputPhase::Paused);
        let state = output.snapshot();
        eprintln!(
            "spatial_phase={:?} max_dynamic={} active_dynamic={} updates={} frames={} buffers={} positions={} underruns={}",
            state.phase(),
            state.max_dynamic_objects(),
            state.active_dynamic_objects(),
            state.render_updates(),
            state.submitted_frames(),
            state.object_buffer_submissions(),
            state.position_updates(),
            state.underruns(),
        );
        assert!(state.object_buffer_submissions() > 0);
        assert!(state.position_updates() > 0);
        assert_eq!(state.underruns(), 0);

        let paused_frame = state.playhead_frames();
        wait_for_decode_indexed(&mut decoder);
        let indexed = decoder.snapshot().metrics().expect("indexed metrics");
        let duration = indexed.duration_frames().expect("indexed duration");
        let first_safe = indexed
            .seekable_from_frame()
            .expect("safe random-access point");
        let seek_target = paused_frame.max(first_safe).min(duration.saturating_sub(1));
        decoder.seek(seek_target).expect("paused seek");
        wait_for_decode_ready(&mut decoder);
        let seek_metrics = decoder.snapshot().metrics().expect("seek metrics");
        let replacement = OutputStreamConfig::new(
            decoder.request_id(),
            decoder.playback_epoch(),
            seek_metrics.target_frame(),
            seek_metrics.sample_rate(),
            seek_metrics
                .scene_signature()
                .cloned()
                .expect("seek Scene signature"),
            OutputDeviceSelection::SystemDefault,
        )
        .expect("replacement config");
        output.ensure_configured(&replacement, decoder.scene_reader());
        wait_for_output_phase(&mut output, OutputPhase::Paused);
        assert_eq!(output.snapshot().playhead_frames(), seek_target);
    }

    fn wait_for_decode_ready(decoder: &mut DecoderController) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            decoder.poll();
            match decoder.snapshot().phase() {
                DecodePhase::Ready | DecodePhase::EndOfStream => return,
                DecodePhase::Failed => panic!(
                    "decode failed: {}",
                    decoder.snapshot().detail().unwrap_or("unknown error")
                ),
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "decoder did not prebuffer in time"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_decode_indexed(decoder: &mut DecoderController) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            decoder.poll();
            let metrics = decoder.snapshot().metrics().expect("decode metrics");
            assert!(
                metrics.index_error().is_none(),
                "seek index failed: {}",
                metrics.index_error().unwrap_or("unknown error")
            );
            if !metrics.is_indexing() {
                return;
            }
            assert!(Instant::now() < deadline, "seek index timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_output_ready(output: &mut SpatialOutputController) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            output.poll();
            match output.snapshot().phase() {
                OutputPhase::Ready | OutputPhase::Paused => return,
                OutputPhase::Failed => panic!(
                    "Spatial Audio setup failed: {}",
                    output.snapshot().error().unwrap_or("unknown error")
                ),
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "Spatial Audio setup timed out: {:?}",
                output.snapshot()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_output_phase(output: &mut SpatialOutputController, expected: OutputPhase) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            output.poll();
            let state = output.snapshot();
            assert_ne!(
                state.phase(),
                OutputPhase::Failed,
                "Spatial Audio failed: {}",
                state.error().unwrap_or("unknown error")
            );
            if state.phase() == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Spatial Audio did not reach {expected:?}: {state:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}
