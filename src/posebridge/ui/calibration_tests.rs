//! The page without a device: stages from fabricated sessions, the grid's
//! geometry and colours, and headless renders that must reach no hardware.
use super::*;
use crate::posebridge::Device;
use crate::posebridge::magnetic::Run;
use crate::posebridge::magnetic::fixtures::{batch, sample};
use pb::MagneticPhase::{
    Calibrated, Calibrating, ExternalCalibration, Monitoring, Opening, Saving, Starting, Stopping,
};

fn device() -> Device {
    Device {
        transport: crate::posebridge::Transport::Usb,
        id: "test-port".into(),
        name: "Test sensor".into(),
        mounting: [-2, 1, 3],
        ..Device::default()
    }
}

/// `count` directions spread evenly over the sphere, as the Earth's field of
/// 50 µT seen through an offset, in sensor axes.
fn sphere(first: u64, count: u32, phase: pb::MagneticPhase) -> Vec<pb::MagneticSample> {
    let golden = std::f64::consts::PI * (3.0 - 5.0_f64.sqrt());
    (0..count)
        .map(|step| {
            let at = f64::from(step);
            let up = 1.0 - (2.0 * at + 1.0) / f64::from(count);
            let across = (1.0 - up * up).sqrt();
            let field = [
                50.0 * across * (golden * at).sin() + 11.0,
                50.0 * across * (golden * at).cos() - 8.0,
                50.0 * up + 6.0,
            ];
            sample(first + u64::from(step), phase, field, [0; 3])
        })
        .collect()
}

/// A session that read `samples`, reporting `phase`.
fn session(samples: Vec<pb::MagneticSample>, phase: pb::MagneticPhase) -> magnetic::Session {
    let mut run = Run::new(device()).unwrap();
    let mut session = run.absorb(batch(samples, true));
    session.phase = phase;
    session
}

fn view(phase: Phase, session: Option<magnetic::Session>) -> View {
    View {
        phase,
        device: Some(device()),
        magnetic_session: session,
        ..View::default()
    }
}

#[test]
fn the_stage_follows_the_session_and_the_worker() {
    assert_eq!(Stage::of(&View::default()), Stage::Intro);
    assert_eq!(Stage::of(&view(Phase::Magnetic, None)), Stage::Opening);
    for (phase, stage) in [
        (Opening, Stage::Opening),
        (Monitoring, Stage::Before),
        (ExternalCalibration, Stage::External),
        (Starting, Stage::During),
        (Calibrating, Stage::During),
        (Stopping, Stage::During),
        (Calibrated, Stage::After),
        (Saving, Stage::After),
        (pb::MagneticPhase::Closed, Stage::Ended),
        (pb::MagneticPhase::Failed, Stage::Ended),
    ] {
        let open = view(Phase::Magnetic, Some(session(Vec::new(), phase)));
        assert_eq!(Stage::of(&open), stage, "{phase:?}");
        // Whatever the session last said, a worker that left it has ended it.
        for worker in [Phase::Stopping, Phase::Idle, Phase::Failed] {
            let left = view(worker, Some(session(Vec::new(), phase)));
            assert_eq!(Stage::of(&left), Stage::Ended, "{phase:?} {worker:?}");
        }
    }
}

fn snapshot(next: Option<coverage::Next>, read: &[usize]) -> coverage::Snapshot {
    let mut counts = [0; CELLS];
    for &cell in read {
        counts[cell] = 1;
    }
    coverage::Snapshot {
        counts,
        readings: 100,
        centre: None,
        spread: None,
        next,
        latest: None,
    }
}

#[test]
fn an_instruction_holds_until_its_target_is_read_or_its_time_is_up() {
    let nod = coverage::Next {
        cell: 5,
        motion: crate::posebridge::mounting::Motion::Nod,
        positive: true,
    };
    let tilt = coverage::Next {
        cell: 5,
        motion: crate::posebridge::mounting::Motion::Tilt,
        positive: false,
    };
    let elsewhere = coverage::Next { cell: 9, ..tilt };
    let start = Instant::now();
    let mut instruction = Instruction::default();
    assert_eq!(
        instruction.hold(Sweep::During, &snapshot(Some(nod), &[]), start),
        Some(nod)
    );
    // Nearly equal components: the words would swap with every reading.
    let soon = start + HOLD / 2;
    assert_eq!(
        instruction.hold(Sweep::During, &snapshot(Some(tilt), &[]), soon),
        Some(nod)
    );
    // Once it has been up long enough, the new one shows.
    let later = start + HOLD;
    assert_eq!(
        instruction.hold(Sweep::During, &snapshot(Some(tilt), &[]), later),
        Some(tilt)
    );
    // Reaching the target, or moving to another sweep, replaces it at once.
    assert_eq!(
        instruction.hold(Sweep::During, &snapshot(Some(elsewhere), &[5]), later),
        Some(elsewhere)
    );
    assert_eq!(
        instruction.hold(Sweep::After, &snapshot(Some(nod), &[]), later),
        Some(nod)
    );
    // Nothing left to reach shows nothing.
    assert_eq!(
        instruction.hold(Sweep::After, &snapshot(None, &[]), later),
        None
    );
}

/// The marker must land in the cell its reading was counted in, and the grid
/// must read the way `coverage` numbers it: rows from the top, columns from
/// the left.
#[test]
fn the_marker_sits_in_the_cell_its_place_is_counted_in() {
    let area = egui::Rect::from_min_size(egui::pos2(40., 10.), egui::vec2(360., 120.));
    for index in 0..CELLS {
        let place = [
            f64::from(at(index % COLUMNS)) + 0.5,
            f64::from(at(index / COLUMNS)) + 0.5,
        ];
        assert!(
            cell_rect(area, index).contains(marker(area, place)),
            "{index}"
        );
    }
    assert_eq!(cell_rect(area, 0).min, area.min);
    assert_eq!(cell_rect(area, CELLS - 1).max, area.max);
    assert_eq!(marker(area, [0.0, 0.0]), area.min);
    let far = [f64::from(at(COLUMNS)), f64::from(at(ROWS))];
    assert_eq!(marker(area, far), area.max);
    // A place a hair outside the grid is drawn on its edge.
    assert_eq!(marker(area, [-0.1, 4.1]), area.left_bottom());
}

#[test]
fn a_cell_deepens_with_readings_up_to_full_at_and_no_further() {
    assert_eq!(fill(0), theme::STAGE);
    assert_ne!(fill(1), theme::STAGE);
    assert_ne!(fill(1), fill(2));
    assert_eq!(fill(u32::from(FULL_AT)), theme::ACCENT);
    assert_eq!(fill(u32::from(FULL_AT) + 100), theme::ACCENT);
}

/// The colours `docs/POSEBRIDGE-MAGNETIC.md` names, and the contrast the
/// deepened ones were chosen for.
#[test]
fn the_documented_colours_hold() {
    let line =
        theme::BORDER.lerp_to_gamma(theme::MUTED, crate::scene3d::params::FLOOR_GRID_CONTRAST);
    assert_eq!(line, egui::Color32::from_rgb(0xcf, 0xc4, 0xb3));
    let luminance = |colour: egui::Color32| {
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.040_45 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(colour.r()) + 0.7152 * channel(colour.g()) + 0.0722 * channel(colour.b())
    };
    let contrast = |colour| (luminance(theme::BACKGROUND) + 0.05) / (luminance(colour) + 0.05);
    for (colour, original) in [
        (MUTED_DEEP, theme::MUTED),
        (ACCENT_DEEP, theme::ACCENT),
        (SUCCESS_DEEP, theme::SUCCESS),
    ] {
        assert!(contrast(colour) >= 4.5, "{colour:?}");
        assert!(contrast(original) < 4.5, "{original:?}");
    }
}

/// The texts one headless pass drew.
fn drawn(panel: &mut Panel, prefs: &Preferences, service: &Service, view: &View) -> Vec<String> {
    let context = egui::Context::default();
    crate::theme::install(&context);
    let mut texts = Vec::new();
    for frame in 0..2 {
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000., 1400.),
                )),
                time: Some(f64::from(frame) * 0.1),
                ..Default::default()
            },
            |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    panel.calibration(ui, prefs, service, view);
                });
            },
        );
        // This headless test inspects shapes without a renderer to upload textures.
        output.textures_delta.clear();
        texts = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.job.text.clone()),
                _ => None,
            })
            .collect();
    }
    texts
}

fn shows(texts: &[String], text: &str) -> bool {
    texts.iter().any(|drawn| drawn == text)
}

#[test]
fn each_stage_offers_only_its_own_operations() {
    let service = Service::new();
    let prefs = Preferences {
        device: device(),
        ..Preferences::default()
    };
    let mut panel = Panel::default();

    let intro = drawn(&mut panel, &prefs, &service, &View::default());
    assert!(shows(&intro, "Open magnetic session"));
    assert!(shows(&intro, "Mounting: Right −Y · Forward +X · Up +Z"));
    assert!(!shows(&intro, "End calibration"));

    let before = view(
        Phase::Magnetic,
        Some(session(sphere(1, 40, Monitoring), Monitoring)),
    );
    let before = drawn(&mut panel, &prefs, &service, &before);
    assert!(shows(&before, "Start calibration…"));
    assert!(shows(&before, "Close session"));
    assert!(!shows(&before, "End calibration"));

    let during = view(
        Phase::Magnetic,
        Some(session(sphere(1, 40, Calibrating), Calibrating)),
    );
    let during = drawn(&mut panel, &prefs, &service, &during);
    assert!(shows(&during, "End calibration"));
    for absent in [
        "Start calibration…",
        "Close session",
        "Save current device settings…",
        "Open magnetic session",
    ] {
        assert!(!shows(&during, absent), "{absent}");
    }

    let external = view(
        Phase::Magnetic,
        Some(session(Vec::new(), ExternalCalibration)),
    );
    let external = drawn(&mut panel, &prefs, &service, &external);
    assert!(shows(&external, "End calibration"));
    assert!(!shows(&external, "Start calibration…"));

    assert_eq!(
        service.view().request,
        0,
        "drawing the page must not reach the sensor"
    );
}

#[test]
fn after_an_end_says_whether_it_was_verified_and_that_nothing_is_saved() {
    let service = Service::new();
    let prefs = Preferences {
        device: device(),
        ..Preferences::default()
    };
    let mut panel = Panel::default();
    let mut ended = session(sphere(1, 40, Calibrated), Calibrated);
    let unverified = drawn(
        &mut panel,
        &prefs,
        &service,
        &view(Phase::Magnetic, Some(ended.clone())),
    );
    assert!(shows(&unverified, "Nothing has been saved."));
    assert!(!shows(&unverified, "End verified by readback"));
    assert!(shows(&unverified, "Save current device settings…"));

    ended.end_verified = true;
    let verified = drawn(
        &mut panel,
        &prefs,
        &service,
        &view(Phase::Magnetic, Some(ended.clone())),
    );
    assert!(shows(&verified, "End verified by readback"));

    // A save refused before it was sent saved nothing; one sent is reported
    // as sent, and the end it followed stays verified.
    let mut save = pb::OperationStatus::new("save".into());
    save.outcome = pb::OperationOutcome::Failed;
    ended.operation = Some(save.clone());
    let refused = drawn(
        &mut panel,
        &prefs,
        &service,
        &view(Phase::Magnetic, Some(ended.clone())),
    );
    assert!(shows(&refused, "Nothing has been saved."));
    save.command_sent = true;
    save.outcome = pb::OperationOutcome::Unverified;
    ended.operation = Some(save);
    let saved = drawn(
        &mut panel,
        &prefs,
        &service,
        &view(Phase::Magnetic, Some(ended)),
    );
    assert!(!shows(&saved, "Nothing has been saved."));
    assert!(shows(&saved, "End verified by readback"));
    assert_eq!(service.view().request, 0);
}

/// Spreads over different directions do not compare, so the page compares
/// the sweeps only once both have read every one.
#[test]
fn before_and_after_compare_only_when_both_are_complete() {
    let service = Service::new();
    let prefs = Preferences {
        device: device(),
        ..Preferences::default()
    };
    let mut panel = Panel::default();
    let mut samples = sphere(1, 600, Monitoring);
    samples.extend(sphere(601, 20, Calibrating));
    samples.extend(sphere(621, 40, Calibrated));
    let partial = session(samples.clone(), Calibrated);
    assert!(partial.sweep(Sweep::Before).complete());
    assert!(!partial.sweep(Sweep::After).complete());
    let texts = drawn(
        &mut panel,
        &prefs,
        &service,
        &view(Phase::Magnetic, Some(partial)),
    );
    assert!(!shows(&texts, "Spread before"));

    samples.extend(sphere(661, 600, Calibrated));
    let complete = session(samples, Calibrated);
    assert!(complete.sweep(Sweep::After).complete());
    let texts = drawn(
        &mut panel,
        &prefs,
        &service,
        &view(Phase::Magnetic, Some(complete)),
    );
    assert!(shows(&texts, "Spread before"));
    assert!(shows(&texts, "Spread after"));
    assert!(
        texts
            .iter()
            .any(|text| text.starts_with("Centre offset ") && text.contains('→'))
    );
}

#[test]
fn a_mounting_that_cannot_carry_the_guidance_keeps_the_session_shut() {
    let service = Service::new();
    let mut prefs = Preferences::default();
    let mut panel = Panel::default();
    let texts = drawn(&mut panel, &prefs, &service, &View::default());
    assert!(shows(&texts, "Choose a sensor on the Device page."));
    prefs.device = Device {
        mounting: [2, 1, 3],
        ..device()
    };
    let texts = drawn(&mut panel, &prefs, &service, &View::default());
    assert!(shows(
        &texts,
        "Set the sensor mounting first: calibration guidance is drawn in headset axes."
    ));
    // Tracking holds the sensor; the session waits for it to let go.
    prefs.device = device();
    let tracking = View {
        phase: Phase::Tracking,
        ..View::default()
    };
    let texts = drawn(&mut panel, &prefs, &service, &tracking);
    assert!(shows(
        &texts,
        "Disconnect or finish the current operation first"
    ));
    assert_eq!(service.view().request, 0);
}
