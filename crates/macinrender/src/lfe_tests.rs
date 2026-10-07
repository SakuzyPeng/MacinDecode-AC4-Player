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

fn check_lfe_output(split: bool, first: f32, second: f32, expected: [f32; 2]) {
    let mut session = Session::new(&Config {
        renderer: RendererSettings {
            binaural: false,
            layout: "9+10+3".into(),
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
    session.configure_lfes(1, 0, &[], &[4, 10]).unwrap();
    let left = [first; 4096];
    let right = [second; 4096];
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
                planes: &[
                    Plane {
                        element: 4,
                        samples: &left
                    },
                    Plane {
                        element: 10,
                        samples: &right
                    }
                ],
                initial: &[(4, state), (10, state)],
                updates: &[],
            })
            .unwrap()
    );
    session.end(1, 4096).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut received = 0;
    let mut full_level = 0;
    while received < 4096 {
        let mut pcm = vec![0.0; 256 * 24];
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
        for frame in pcm
            .as_chunks::<24>()
            .0
            .iter()
            .take(result.media_frames as usize)
        {
            for (channel, sample) in frame.iter().enumerate() {
                if ![3, 9].contains(&channel) {
                    assert!(sample.abs() < 1e-6, "LFE leaked to channel {channel}");
                }
            }
            if (frame[3] - expected[0]).abs() < 1e-6 && (frame[9] - expected[1]).abs() < 1e-6 {
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
