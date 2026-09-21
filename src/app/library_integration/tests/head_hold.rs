use super::*;
use crate::app::head_puck::Command;
use crate::head_tracking::{HeadSource, HeadStatus};

fn configure(app: &mut PlayerApp, context: &egui::Context, source: HeadSource) {
    let mut settings = app.output.settings().clone();
    settings.mode = crate::backend::SpatialBackendKind::SafBinaural;
    settings.null_output = true;
    settings.head_source = source;
    app.change_output_settings(settings, context);
}

#[test]
fn a_temporary_hold_preserves_the_saved_source_and_manual_pose() {
    let directory = tempfile::tempdir().unwrap();
    let (mut app, context) = open(directory.path());
    settle(&mut app, &context);
    configure(&mut app, &context, HeadSource::Manual);
    let angles = [45.0, 20.0, -10.0];
    app.apply_head_command(Command::Turn(angles), &context);
    until(&mut app, &context, |app| {
        (app.output.head_snapshot().pose.euler()[0] - angles[0]).abs() < 0.01
    });
    app.apply_head_command(Command::Hold, &context);
    until(&mut app, &context, |app| {
        app.output.head_snapshot().status == HeadStatus::Held
    });
    app.flush_persistence();
    app.library.shutdown();
    drop(app);

    let (mut reopened, context) = open(directory.path());
    settle(&mut reopened, &context);
    assert_eq!(reopened.output.settings().head_source, HeadSource::Manual);
    assert!(
        reopened
            .preferences
            .manual_head
            .into_iter()
            .zip(angles)
            .all(|(actual, expected)| (actual - expected).abs() < f32::EPSILON)
    );
    until(&mut reopened, &context, |app| {
        let head = app.output.head_snapshot();
        head.status == HeadStatus::Manual && (head.pose.euler()[0] - angles[0]).abs() < 0.01
    });
}

#[cfg(posebridge_input)]
#[test]
fn holding_keeps_posebridge_sampling_and_releases_without_reconnecting() {
    use crate::posebridge::service::{Command as SensorCommand, Phase};

    let directory = tempfile::tempdir().unwrap();
    let (mut app, context) = open(directory.path());
    settle(&mut app, &context);
    configure(&mut app, &context, HeadSource::PoseBridge);
    app.output
        .posebridge()
        .send(SensorCommand::Simulate { rate: 100 })
        .unwrap();
    until(&mut app, &context, |app| {
        app.output.head_snapshot().status == HeadStatus::BridgeActive
    });
    app.apply_head_command(Command::Hold, &context);
    until(&mut app, &context, |app| {
        app.output.head_snapshot().status == HeadStatus::Held
    });
    let held = app.output.head_snapshot().pose;
    let sample = app
        .output
        .posebridge()
        .sample
        .lock()
        .unwrap()
        .clone()
        .unwrap();
    let request = app.output.posebridge().view().request;
    until(&mut app, &context, |app| {
        app.output
            .posebridge()
            .sample
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|next| next.sequence > sample.sequence + 5)
    });
    assert_eq!(app.output.settings().head_source, HeadSource::PoseBridge);
    assert_eq!(app.output.head_snapshot().pose, held);
    assert_eq!(app.output.posebridge().view().phase, Phase::Tracking);
    app.apply_head_command(Command::Hold, &context);
    until(&mut app, &context, |app| {
        let snapshot = app.output.head_snapshot();
        snapshot.status == HeadStatus::BridgeActive && snapshot.pose != held
    });
    let resumed = app
        .output
        .posebridge()
        .sample
        .lock()
        .unwrap()
        .clone()
        .unwrap();
    assert_eq!(resumed.instance, sample.instance);
    assert_eq!(resumed.session, sample.session);
    assert_eq!(resumed.reference, sample.reference);
    assert_eq!(app.output.posebridge().view().request, request);
}
