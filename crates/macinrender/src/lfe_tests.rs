use super::*;
use std::time::{Duration, Instant};

#[repr(C)]
#[derive(Default)]
struct PullResult {
    size: u32,
    flags: u32,
    epoch: u64,
    first: u64,
    media_frames: u32,
    requested_frames: u32,
}

unsafe extern "C" {
    fn adm_scene_stream_pull(
        stream: *mut std::ffi::c_void,
        output: *mut f32,
        frames: u32,
        result: *mut PullResult,
    ) -> i32;
}

#[test]
fn apple_22_2_keeps_two_lfe_inputs_in_separate_output_channels() {
    check_lfe_output(false, 0.125, -0.25, [0.125, -0.25]);
}

#[test]
fn apple_equal_power_copy_handles_one_or_two_audible_inputs() {
    let half_power = std::f32::consts::FRAC_1_SQRT_2;
    check_lfe_output(true, 0.125, 0.0, [0.125 * half_power; 2]);
    check_lfe_output(true, 0.0, -0.25, [-0.25 * half_power; 2]);
    // The player normalizes the two audible inputs before native split-power.
    check_lfe_output(true, 0.125 * half_power, -0.25 * half_power, [-0.0625; 2]);
}

#[test]
fn triple_balance_player_capabilities_are_enabled_for_all_three_layouts() {
    for layout in ["4+7+0", "9.1.6", "9+10+3"] {
        probe_player_renderer(
            &RendererSettings {
                speaker_renderer: SpeakerRenderer::TripleBalance,
                binaural: false,
                layout: layout.into(),
                sofa: String::new(),
                split_lfe: false,
            },
            48_000,
        )
        .unwrap_or_else(|error| panic!("{layout} capability probe: {error}"));
    }
}

#[test]
fn triple_balance_plays_objects_with_an_lfe_only_bed() {
    for (layout, lfes) in [
        ("4+7+0", &[8][..]),
        ("9.1.6", &[8][..]),
        ("9+10+3", &[8][..]),
    ] {
        let mut session = Session::new(&Config {
            renderer: RendererSettings {
                speaker_renderer: SpeakerRenderer::TripleBalance,
                binaural: false,
                layout: layout.into(),
                sofa: String::new(),
                split_lfe: false,
            },
            output: OutputKind::Null,
            device_id: String::new(),
            input_rate: 48_000,
        })
        .unwrap();
        session.reset(1, 0).unwrap();
        session
            .configure_lfes(1, 0, &[7], lfes)
            .unwrap_or_else(|error| panic!("{layout} with independent LFEs {lfes:?}: {error}"));
        let pcm = [0.01; 1024];
        let planes: Vec<_> = std::iter::once(7)
            .chain(lfes.iter().copied())
            .map(|element| Plane {
                element,
                samples: &pcm,
            })
            .collect();
        let initial: Vec<_> = std::iter::once(7)
            .chain(lfes.iter().copied())
            .map(|element| {
                (
                    element,
                    ObjectState {
                        active: true,
                        gain: 1.0,
                        position: (element == 7).then_some([0.0, 1.0, 0.0]),
                        head_locked: false,
                    },
                )
            })
            .collect();
        session
            .submit(&Frame {
                epoch: 1,
                generation: 0,
                start: 0,
                duration: 1024,
                complete: true,
                planes: &planes,
                initial: &initial,
                updates: &[],
            })
            .unwrap();
        session.end(1, 1024).unwrap();
        let control = session.control();
        control.play(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = control.status().unwrap();
            assert!(
                status.phase != Phase::Failed,
                "{layout} with LFE {lfes:?}: {}",
                control.failure_message()
            );
            if status.phase == Phase::Ended {
                assert_eq!(status.presented, 1024);
                break;
            }
            assert!(Instant::now() < deadline, "{layout} stalled: {status:?}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn check_lfe_output(split: bool, first: f32, second: f32, expected: [f32; 2]) {
    check_renderer_lfe_output(
        SpeakerRenderer::SafVbap,
        "9+10+3",
        split,
        &[first, second],
        expected,
    );
}

#[test]
fn triple_balance_silent_bed_routes_only_lfe_energy_at_the_expected_level() {
    for layout in ["4+7+0", "9.1.6", "9+10+3"] {
        check_renderer_lfe_output(
            SpeakerRenderer::TripleBalance,
            layout,
            false,
            &[0.125],
            [0.125, 0.0],
        );
    }
    check_renderer_lfe_output(
        SpeakerRenderer::TripleBalance,
        "9+10+3",
        true,
        &[0.125],
        [0.125 * std::f32::consts::FRAC_1_SQRT_2; 2],
    );
}

#[allow(
    clippy::too_many_lines,
    reason = "one native PCM fixture verifies silent padding and channel isolation"
)]
fn check_renderer_lfe_output(
    renderer: SpeakerRenderer,
    layout: &str,
    split: bool,
    inputs: &[f32],
    expected: [f32; 2],
) {
    let channels = match layout {
        "4+7+0" => 12,
        "9.1.6" => 16,
        _ => 24,
    };
    let mut session = Session::new(&Config {
        renderer: RendererSettings {
            speaker_renderer: renderer,
            binaural: false,
            layout: layout.into(),
            sofa: String::new(),
            split_lfe: split,
        },
        // Leave output paused and pull the native PCM directly for inspection.
        output: OutputKind::Null,
        device_id: String::new(),
        input_rate: 48_000,
    })
    .unwrap();
    session.reset(1, 0).unwrap();
    {
        let inner = Arc::get_mut(&mut session.inner).expect("no control handles yet");
        // SAFETY: this test exclusively owns the paused output. Its destruction
        // releases the stream's consumer reservation before direct PCM pulls.
        unsafe {
            (inner.api.adm_destroy_scene_output)(inner.output);
        }
        inner.output = std::ptr::null_mut();
    }
    let ids = &[4, 10][..inputs.len()];
    session.configure_lfes(1, 0, &[], ids).unwrap();
    let samples: Vec<_> = inputs.iter().map(|value| vec![*value; 4096]).collect();
    let planes: Vec<_> = ids
        .iter()
        .zip(&samples)
        .map(|(&element, samples)| Plane { element, samples })
        .collect();
    let state = ObjectState {
        active: true,
        gain: 1.0,
        position: None,
        head_locked: false,
    };
    assert!(
        session
            .submit(&Frame {
                epoch: 1,
                generation: 0,
                start: 0,
                duration: 4096,
                complete: true,
                planes: &planes,
                initial: &ids.iter().map(|id| (*id, state)).collect::<Vec<_>>(),
                updates: &[],
            })
            .unwrap()
    );
    session.end(1, 4096).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut received = 0;
    let mut full_level = 0;
    while received < 4096 {
        let mut pcm = vec![0.0; 256 * channels];
        let mut result = PullResult {
            size: size::<PullResult>(),
            ..Default::default()
        };
        {
            let inner = &session.inner;
            let _guard = inner.gate.lock().unwrap();
            // SAFETY: the output was detached and there is one consumer; buffers
            // cover 256 frames of the selected 24-channel native layout.
            let code = unsafe {
                adm_scene_stream_pull(inner.stream, pcm.as_mut_ptr(), 256, &raw mut result)
            };
            inner.error(code, 1).unwrap();
        }
        assert_eq!(
            result.flags & 8,
            0,
            "{}",
            session.control().failure_message()
        );
        for frame in pcm
            .chunks_exact(channels)
            .take(result.media_frames as usize)
        {
            for (channel, sample) in frame.iter().enumerate() {
                if channel != 3 && !(channels == 24 && channel == 9) {
                    assert!(sample.abs() < 1e-6, "LFE leaked to channel {channel}");
                }
            }
            if (frame[3] - expected[0]).abs() < 1e-6
                && (if channels == 24 { frame[9] } else { 0.0 } - expected[1]).abs() < 1e-6
            {
                full_level += 1;
            }
        }
        received += result.media_frames;
        assert!(
            Instant::now() < deadline,
            "native 22.2 render stalled at {received}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        full_level > 2048,
        "LFE routing or normalization was lost: {expected:?}"
    );
}
