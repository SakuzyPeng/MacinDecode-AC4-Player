//! Opt-in diagnostic: production decode, Scene output and PoseBridge control workers.
//! Set MACINDECODE_UNDERRUN_MEDIA and MACINDECODE_UNDERRUN_SETTINGS explicitly.
//! HEAD selects off/held/posebridge; FULL_TRACK keeps tracking through EOF.
use super::*;
use crate::decoder::{DecodePhase, DecoderController};
use crate::head_tracking::{HeadSource, HeadStatus, HeadTracker};
use crate::posebridge::service::Command;
use serde_json::json;
use std::time::Instant;

#[test]
#[ignore = "requires explicit media/settings paths and an available stereo endpoint; logs timing"]
fn current_player_saf_pose_underrun_probe() {
    let media = std::env::var_os("MACINDECODE_UNDERRUN_MEDIA").expect("media path");
    let prefs_path = std::env::var_os("MACINDECODE_UNDERRUN_SETTINGS").expect("settings path");
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(prefs_path).unwrap()).unwrap();
    let mut settings: OutputSettings =
        serde_json::from_value(document["preferences"]["output"].clone()).unwrap();
    settings.mode = SpatialBackendKind::SafBinaural;
    settings.null_output = false;
    let head = std::env::var("MACINDECODE_UNDERRUN_HEAD").unwrap_or_else(|_| "posebridge".into());
    assert!(matches!(head.as_str(), "off" | "posebridge" | "held"));
    let full_track = std::env::var_os("MACINDECODE_UNDERRUN_FULL_TRACK").is_some();
    if std::env::var_os("MACINDECODE_UNDERRUN_BUILTIN").is_some() {
        settings.sofa.clear();
    }
    if std::env::var_os("MACINDECODE_UNDERRUN_NO_HPTF").is_some() {
        settings.hptf.clear();
    }
    let mut decoder = DecoderController::new();
    decoder.ensure_open(std::path::Path::new(&media));
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        decoder.poll();
        let snapshot = decoder.snapshot();
        assert_ne!(snapshot.phase(), DecodePhase::Failed, "{snapshot:?}");
        if matches!(
            snapshot.phase(),
            DecodePhase::Ready | DecodePhase::EndOfStream
        ) {
            break;
        }
        assert!(Instant::now() < deadline, "decode startup timed out");
        thread::sleep(Duration::from_millis(5));
    }
    let metrics = decoder.snapshot().metrics().unwrap();
    let playback_timeout = Duration::from_secs(
        metrics
            .duration_frames()
            .unwrap_or(u64::from(metrics.sample_rate()) * 1200)
            / u64::from(metrics.sample_rate())
            + 180,
    );
    println!(
        "{}",
        json!({"event":"configuration","media":media.to_string_lossy(),
        "head":head,"sofa":settings.sofa,"hptf":settings.hptf,
        "rate":metrics.sample_rate(),"objects":metrics.object_count(),"lfes":metrics.lfe_count(),
        "optimized":!cfg!(debug_assertions),"device":settings.stereo_device,
        "full_track":full_track,"duration_frames":metrics.duration_frames()})
    );
    let config = OutputStreamConfig::new(
        decoder.request_id(),
        decoder.playback_epoch(),
        metrics.target_frame(),
        metrics.sample_rate(),
        metrics.scene_signature().unwrap().clone(),
        settings.stereo_device.clone(),
    )
    .unwrap();
    let hptf = crate::backend::HptfRequest {
        settings: settings.hptf(),
        adjustment: settings.hptf_adjustment(),
    };
    let tracker = HeadTracker::new();
    tracker.configure_bridge(&settings.posebridge);
    let runtime = Runtime::spawn(
        config,
        settings,
        decoder.scene_reader(),
        Arc::new(SceneViewMirror::new()),
        true,
        0.0,
    )
    .unwrap();
    runtime.set_hptf(hptf.clone(), 1);
    let initial_underruns;
    loop {
        decoder.poll();
        let output = runtime.snapshot();
        assert_ne!(output.phase, OutputPhase::Failed, "{output:?}");
        if let Some((_, result)) = runtime.take_hptf_result() {
            assert!(result.unwrap());
        }
        let applied = runtime.hptf_status();
        if applied.applied_revision == 1 && output.playhead_frames > 0 {
            initial_underruns = output.underruns;
            assert_eq!(applied.enabled, !hptf.settings.profile.is_empty());
            println!(
                "{}",
                json!({"event":"ready","hptf_enabled":applied.enabled,
                "hptf_bands":applied.bands,"hptf_revision":applied.applied_revision,
                "playhead":output.playhead_frames,"underruns":output.underruns})
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "output startup timed out: {output:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let start = Instant::now();
    let mut next_log = Duration::ZERO;
    let mut started = false;
    let mut stopped = false;
    let mut held = false;
    let mut active_snapshots = 0;
    while full_track || start.elapsed() < Duration::from_secs(30) {
        let elapsed = start.elapsed();
        assert!(
            elapsed < playback_timeout,
            "playback did not finish before timeout"
        );
        decoder.poll();
        assert_ne!(
            decoder.snapshot().phase(),
            DecodePhase::Failed,
            "{:?}",
            decoder.snapshot()
        );
        if head != "off" && elapsed >= Duration::from_secs(10) && !started {
            tracker.configure(HeadSource::PoseBridge, true, false);
            tracker.set_target(Some(runtime.control_slot()));
            tracker
                .bridge
                .send(Command::Simulate { rate: 100 })
                .unwrap();
            started = true;
        }
        if head == "held" && !held && tracker.snapshot().status == HeadStatus::BridgeActive {
            tracker.toggle_hold();
            held = true;
        }
        if !full_track && started && elapsed >= Duration::from_secs(20) && !stopped {
            tracker.set_target(None);
            tracker.bridge.send(Command::Stop).unwrap();
            stopped = true;
        }
        let output = runtime.snapshot();
        assert_ne!(output.phase, OutputPhase::Failed, "{output:?}");
        if full_track && output.phase == OutputPhase::Ended {
            break;
        }
        assert_ne!(
            output.phase,
            OutputPhase::Ended,
            "test media ended before measurement finished"
        );
        if elapsed >= next_log {
            let metrics = decoder.snapshot().metrics().unwrap();
            let pose = tracker.snapshot();
            active_snapshots += usize::from(pose.status == HeadStatus::BridgeActive);
            let bridge = tracker.bridge.view();
            let sequence = tracker
                .bridge
                .sample
                .lock()
                .unwrap()
                .as_ref()
                .map(|s| s.sequence);
            println!(
                "{}",
                json!({"event":"sample","t":elapsed.as_secs_f64(),
                "stage":if full_track { u64::from(started) } else { elapsed.as_secs()/10 },
                "underruns":output.underruns,
                "playhead":output.playhead_frames,"output_queue":output.queued_output_frames,
                "decode_buffer":metrics.buffered_frames(),"decoded_frames":metrics.decoded_frames(),
                "decode_phase":format!("{:?}",decoder.snapshot().phase()),
                "head_status":format!("{:?}",pose.status),"angles":pose.pose.euler(),
                "bridge_phase":format!("{:?}",bridge.phase),"bridge_hz":bridge.delivery_hz,
                "bridge_sequence":sequence,"bridge_error":bridge.error})
            );
            next_log = elapsed + Duration::from_millis(100);
        }
        thread::sleep(Duration::from_millis(5));
    }
    let output = runtime.snapshot();
    tracker.set_target(None);
    runtime.play(false);
    if head != "off" {
        assert!(active_snapshots > 50, "no sustained PoseBridge tracking");
    }
    println!(
        "{}",
        json!({"event":"result","head":head,"underruns":output.underruns,
        "measurement_underruns":output.underruns-initial_underruns,
        "elapsed_seconds":start.elapsed().as_secs_f64(),"ended":output.phase==OutputPhase::Ended,
        "playhead":output.playhead_frames,"tracking_snapshots":active_snapshots})
    );
}
