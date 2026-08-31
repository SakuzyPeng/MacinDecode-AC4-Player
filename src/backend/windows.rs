use macindecode_windows_spatial_audio::{RenderPhase, RenderSnapshot, Renderer, StreamConfig};

use super::source::SceneRenderSource;
use super::{OutputPhase, OutputSnapshot, OutputStreamConfig};
use crate::decoder::SceneQueueReader;

pub(super) fn spawn(
    config: OutputStreamConfig,
    reader: SceneQueueReader,
) -> Result<Renderer, String> {
    Renderer::spawn(
        StreamConfig {
            sample_rate: config.sample_rate,
            dynamic_object_count: config.dynamic_object_count,
            has_lfe: config.has_lfe,
        },
        Box::new(SceneRenderSource::new(
            reader,
            config.sample_rate,
            config.dynamic_object_count,
            config.has_lfe,
        )),
    )
}

pub(super) fn snapshot(native: RenderSnapshot) -> OutputSnapshot {
    OutputSnapshot {
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
        object_buffer_submissions: native.object_buffer_submissions,
        position_updates: native.position_updates,
        underruns: native.underruns,
        error: native.error,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::super::{OutputPhase, OutputStreamConfig, SpatialOutputController};
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
            metrics.sample_rate(),
            metrics.object_count(),
            metrics.has_lfe(),
        )
        .expect("valid Spatial Audio config");
        let mut output = SpatialOutputController::new();
        output.ensure_configured(config, decoder.scene_reader());
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
