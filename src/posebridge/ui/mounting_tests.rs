//! Synthetic poses and egui input only; no sensor is accessed.
use super::*;
use crate::head_tracking::HeadStatus;
use mounting::Motion;

const IDENTITY: [i8; 3] = [1, 2, 3];
const RECORDED: [i8; 3] = [-2, 1, 3];

fn active(pose: Quaternion) -> HeadSnapshot {
    HeadSnapshot {
        pose,
        status: HeadStatus::BridgeActive,
        measurement: Some(pose),
    }
}

fn connected(mounting: [i8; 3]) -> (Preferences, View) {
    let prefs = Preferences {
        device: Device {
            id: "sensor-A".into(),
            mounting,
            ..Device::default()
        },
        ..Preferences::default()
    };
    let view = View {
        request: 1,
        phase: Phase::Tracking,
        device: Some(prefs.device.clone()),
        // Constructing a controller does not start device access.
        snapshot: Some(pb::Controller::new().unwrap().snapshot()),
        ..View::default()
    };
    (prefs, view)
}

fn capture(panel: &mut Panel, prefs: &Preferences, view: &View, motion: Motion, angles: [f32; 3]) {
    panel.advance_check(&active(Quaternion::default()), prefs, view, true);
    panel.check.start(motion, Quaternion::default());
    assert!(panel.check.capturing.is_some());
    panel.advance_check(&active(Quaternion::from_euler(angles)), prefs, view, true);
    // Returning to neutral at the deadline must retain the observed peak.
    panel.check.capturing.as_mut().unwrap().2 = Instant::now();
    panel.advance_check(&active(Quaternion::default()), prefs, view, true);
    assert!(panel.check.capturing.is_none());
}

fn capture_all(panel: &mut Panel, prefs: &Preferences, view: &View) {
    for (motion, angles) in [
        (Motion::Nod, [0., 40., 0.]),
        (Motion::Shake, [40., 0., 0.]),
        (Motion::Tilt, [0., 0., 40.]),
    ] {
        capture(panel, prefs, view, motion, angles);
    }
}

fn assert_empty(panel: &Panel) {
    assert_eq!(
        panel.check.calibration.resolve(),
        Err("Perform all three motions.")
    );
    assert!(panel.check.results.iter().all(Option::is_none));
    assert!(panel.check.capturing.is_none());
    assert!(panel.check.peak.is_none());
}

#[test]
fn three_captured_peaks_resolve_after_returning_to_neutral() {
    let (prefs, view) = connected(RECORDED);
    let mut panel = Panel::default();
    capture_all(&mut panel, &prefs, &view);
    assert_eq!(panel.check.calibration.resolve(), Ok(RECORDED));
    assert!(panel.check.refused.is_none());
    for motion in Motion::ALL {
        let peak = panel.check.results[motion.axis()].unwrap();
        assert_eq!(peak.axis, motion.axis());
        assert!((peak.degrees - 40.0).abs() < 0.01);
    }
}

#[test]
fn retries_remove_the_previous_answer_before_they_finish() {
    let (prefs, view) = connected(IDENTITY);
    for angles in [[0., 5., 0.], [30., 30., 0.]] {
        let mut panel = Panel::default();
        capture_all(&mut panel, &prefs, &view);
        panel.check.start(Motion::Nod, Quaternion::default());
        assert_eq!(
            panel.check.calibration.resolve(),
            Err("Perform all three motions.")
        );
        assert!(panel.check.results[Motion::Nod.axis()].is_none());
        // Stop cannot restore the previous result.
        panel.check.capturing = None;
        capture(&mut panel, &prefs, &view, Motion::Nod, angles);
        assert!(panel.check.refused.is_some());
        assert!(!panel.check.results[Motion::Nod.axis()].unwrap().usable());
        assert_eq!(
            panel.check.calibration.resolve(),
            Err("Perform all three motions.")
        );
        capture(&mut panel, &prefs, &view, Motion::Nod, [0., 40., 0.]);
        assert_eq!(panel.check.calibration.resolve(), Ok(IDENTITY));
    }
}

#[test]
fn source_changes_discard_completed_and_pending_checks() {
    let (prefs, view) = connected(IDENTITY);
    for pending in [false, true] {
        for change in 0..10 {
            let mut panel = Panel::default();
            capture_all(&mut panel, &prefs, &view);
            if pending {
                panel.check.start(Motion::Nod, Quaternion::default());
            }
            let mut next_prefs = prefs.clone();
            let mut next_view = view.clone();
            let mut head = active(Quaternion::default());
            let mut enabled = true;
            match change {
                0 => {
                    // No disconnected UI frame need be observed between devices.
                    next_prefs.select(Transport::Usb, "sensor-B".into(), "B".into());
                    next_prefs.device.mounting = IDENTITY;
                    next_view.device = Some(next_prefs.device.clone());
                }
                1 => next_view.request += 1,
                2 => next_view.snapshot.as_mut().unwrap().descriptor.instance_id += 1,
                3 => next_view.snapshot.as_mut().unwrap().descriptor.session_id += 1,
                4 => {
                    next_view
                        .snapshot
                        .as_mut()
                        .unwrap()
                        .descriptor
                        .reference_epoch += 1;
                }
                5 => head.status = HeadStatus::BridgeFrozen,
                6 => next_view.phase = Phase::Stopping,
                7 => next_prefs.device.mounting = RECORDED,
                8 => enabled = false,
                9 => next_view.phase = Phase::Idle,
                _ => unreachable!(),
            }
            panel.advance_check(&head, &next_prefs, &next_view, enabled);
            assert_empty(&panel);
            assert_eq!(panel.check.refused.is_some(), pending);
        }
    }
}

#[test]
fn ordinary_status_updates_preserve_the_checks() {
    let (prefs, mut view) = connected(IDENTITY);
    let mut panel = Panel::default();
    capture_all(&mut panel, &prefs, &view);
    let snapshot = view.snapshot.as_mut().unwrap();
    snapshot.status.session_samples += 1;
    snapshot.descriptor.metadata_revision += 1;
    panel.advance_check(&active(Quaternion::default()), &prefs, &view, true);
    assert_eq!(panel.check.calibration.resolve(), Ok(IDENTITY));
}

#[test]
fn mounting_uses_measurement_even_when_the_presented_pose_differs() {
    let (prefs, view) = connected(IDENTITY);
    let mut panel = Panel::default();
    panel.advance_check(&active(Quaternion::default()), &prefs, &view, true);
    panel.check.start(Motion::Nod, Quaternion::default());
    let mut head = active(Quaternion::from_euler([0., 40., 0.]));
    head.pose = Quaternion::from_euler([50., 0., 0.]);
    panel.advance_check(&head, &prefs, &view, true);
    let observation = panel.check.peak.unwrap();
    assert_eq!(observation.axis, Motion::Nod.axis());
    assert!((observation.degrees - 40.).abs() < 0.01);
}

fn render_result(
    context: &egui::Context,
    panel: &mut Panel,
    prefs: &mut Preferences,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let mut output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000., 760.),
            )),
            events,
            ..Default::default()
        },
        |root| {
            egui::CentralPanel::default().show(root, |ui| panel.check_result(ui, prefs));
        },
    );
    output.textures_delta.clear();
    output
}

fn find_text(shape: &egui::Shape, needle: &str) -> Option<egui::Pos2> {
    match shape {
        egui::Shape::Text(text) if text.galley.job.text == needle => {
            Some(text.pos + text.galley.rect.center().to_vec2())
        }
        egui::Shape::Vec(shapes) => shapes.iter().find_map(|s| find_text(s, needle)),
        _ => None,
    }
}

#[test]
fn applying_a_mounting_requires_reconnection_before_another_check() {
    let (mut prefs, mut view) = connected(IDENTITY);
    let mut panel = Panel::default();
    // The recorded physical mounting under an identity-configured connection.
    for (motion, angles) in [
        (Motion::Nod, [0., 0., -40.]),
        (Motion::Shake, [40., 0., 0.]),
        (Motion::Tilt, [0., 40., 0.]),
    ] {
        capture(&mut panel, &prefs, &view, motion, angles);
    }
    assert_eq!(panel.check.calibration.resolve(), Ok(RECORDED));
    let context = egui::Context::default();
    let _ = render_result(&context, &mut panel, &mut prefs, vec![]);
    let output = render_result(&context, &mut panel, &mut prefs, vec![]);
    let pos = output
        .shapes
        .iter()
        .find_map(|s| find_text(&s.shape, "Use this mounting"))
        .expect("the apply button was rendered");
    for pressed in [true, false] {
        let _ = render_result(
            &context,
            &mut panel,
            &mut prefs,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                },
            ],
        );
    }
    assert_eq!(prefs.device.mounting, RECORDED);
    assert_eq!(view.device.as_ref().unwrap().mounting, IDENTITY);
    panel.advance_check(&active(Quaternion::default()), &prefs, &view, true);
    assert!(panel.check.source.is_none());
    panel.check.start(Motion::Nod, Quaternion::default());
    assert_empty(&panel);

    view.request += 1;
    view.device = Some(prefs.device.clone());
    capture_all(&mut panel, &prefs, &view);
    assert_eq!(panel.check.calibration.resolve(), Ok(RECORDED));
}

#[test]
#[allow(
    clippy::cast_possible_truncation,
    reason = "PoseBridge Euler output is bounded degrees, as at the service boundary"
)]
fn all_mountings_roundtrip_through_posebridge_and_player_coordinates() {
    let axes = [-3, -2, -1, 1, 2, 3];
    let valid: Vec<_> = axes
        .into_iter()
        .flat_map(|right| {
            axes.into_iter().flat_map(move |forward| {
                axes.into_iter()
                    .map(move |up| pb::pose::Mounting { right, forward, up })
            })
        })
        .filter(|mounting| mounting.validate().is_ok())
        .collect();
    assert_eq!(valid.len(), 24);
    for current in &valid {
        for actual in &valid {
            let actual_axes = [actual.right, actual.forward, actual.up];
            let current_axes = [current.right, current.forward, current.up];
            for base in [[0., 0., 0.], [10., 20., 30.], [-20., 5., -15.]] {
                let start = Quaternion::from_euler(base);
                let to_player = |q: Quaternion| {
                    let [w, x, y, z] = q.0;
                    let gui = current.from_sensor_quaternion([x, y, z, w]).unwrap();
                    Quaternion::from_euler(pb::pose::to_euler(gui).unwrap().map(|a| a as f32))
                };
                let mut calibration = mounting::Calibration::default();
                for motion in Motion::ALL {
                    let sensor_axis = actual_axes[motion.axis()];
                    let half = 20_f64.to_radians();
                    let mut turn = [half.cos(), 0., 0., 0.];
                    turn[usize::from(sensor_axis.unsigned_abs())] =
                        half.sin() * f64::from(sensor_axis.signum());
                    let observation = mounting::observe(
                        to_player(start),
                        to_player(start.multiply(Quaternion(turn))),
                    );
                    assert!(calibration.record(current_axes, motion, observation));
                }
                assert_eq!(
                    calibration.resolve(),
                    Ok(actual_axes),
                    "current={current:?}, actual={actual:?}, base={base:?}"
                );
            }
        }
    }
}
