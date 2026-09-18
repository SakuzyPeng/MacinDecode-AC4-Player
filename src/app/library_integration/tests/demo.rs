use super::*;

#[cfg(all(feature = "decode", any(macinrender_output, not(spatial_output))))]
fn start_demo(app: &mut PlayerApp, context: &egui::Context) {
    crate::theme::install(context);
    #[cfg(macinrender_output)]
    {
        let mut settings = app.output.settings().clone();
        settings.null_output = true;
        settings.mode = crate::backend::SpatialBackendKind::SafBinaural;
        settings.head_source = crate::head_tracking::HeadSource::Off;
        app.output.install_settings(settings);
    }
    app.activate_demo(true);
    until(app, context, |app| app.output.snapshot().is_playing());
}

#[cfg(all(feature = "decode", any(macinrender_output, not(spatial_output))))]
fn seek_demo(app: &mut PlayerApp, frame: u64) {
    app.output.pause();
    app.decoder.seek(frame).unwrap();
    app.playback_restore_pending = true;
}

#[cfg(all(feature = "decode", any(macinrender_output, not(spatial_output))))]
fn widget(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
    painted_text(output)
        .into_iter()
        .find(|(text, _, _)| text == label)
        .unwrap_or_else(|| panic!("missing widget {label}"))
        .1
        .center()
}

#[test]
#[cfg(all(feature = "decode", any(macinrender_output, not(spatial_output))))]
fn demo_repeats_across_multiple_epochs_without_a_playlist_item() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    start_demo(&mut app, &context);
    assert_eq!(app.effective_playback_mode(), PlaybackMode::RepeatOne);
    let request = app.decoder.request_id();
    let duration = app
        .decoder
        .snapshot()
        .metrics()
        .unwrap()
        .duration_frames()
        .unwrap();
    for _ in 0..2 {
        seek_demo(&mut app, duration - 480);
        let epoch = app.decoder.playback_epoch();
        until(&mut app, &context, |app| {
            app.decoder.playback_epoch() > epoch && app.output.snapshot().is_playing()
        });
        assert_eq!(app.decoder.request_id(), request);
        assert_eq!(app.decoder.snapshot().metrics().unwrap().target_frame(), 0);
        assert!(app.demo && app.cursor.is_none());
    }
}

#[test]
#[cfg(all(feature = "decode", any(macinrender_output, not(spatial_output))))]
fn demo_mode_widget_is_independent_and_play_once_can_replay() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    let list = app.library.desired_browse.unwrap();
    app.library
        .mutate(Mutation::Mode(list, PlaybackMode::Shuffle));
    settle(&mut app, &context);
    start_demo(&mut app, &context);

    let _ = ui_frame(&mut app, &context, 0.0, vec![]);
    let output = ui_frame(&mut app, &context, 0.1, vec![]);
    let _ = click_widget(&mut app, &context, widget(&output, "Repeat one"), 1.0);
    let popup = ui_frame(&mut app, &context, 1.1, vec![]);
    let _ = click_widget(&mut app, &context, widget(&popup, "Play once"), 1.2);
    assert_eq!(app.effective_playback_mode(), PlaybackMode::Sequential);
    assert_eq!(
        app.library
            .summaries
            .iter()
            .find(|p| p.id == list)
            .unwrap()
            .mode,
        PlaybackMode::Shuffle
    );

    let duration = app
        .decoder
        .snapshot()
        .metrics()
        .unwrap()
        .duration_frames()
        .unwrap();
    seek_demo(&mut app, duration);
    let ended_epoch = app.decoder.playback_epoch();
    until(&mut app, &context, |app| {
        app.output.snapshot().phase() == crate::backend::OutputPhase::Ended && !app.playback_intent
    });
    assert_ne!(app.decoder.snapshot().phase(), DecodePhase::Failed);
    let output = ui_frame(&mut app, &context, 2.0, vec![]);
    let _ = click_widget(&mut app, &context, widget(&output, "▶"), 2.2);
    until(&mut app, &context, |app| app.output.snapshot().is_playing());
    assert_eq!(app.decoder.playback_epoch(), ended_epoch + 1);
    assert_eq!(app.decoder.snapshot().metrics().unwrap().target_frame(), 0);

    let old_request = app.decoder.request_id();
    app.retry_playback();
    until(&mut app, &context, |app| app.output.snapshot().is_playing());
    assert!(app.decoder.request_id() > old_request);
    assert!(app.demo && app.cursor.is_none());
}

#[test]
fn parked_file_tracks_library_edits_during_demo() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    let list = app.library.desired_browse.unwrap();
    app.library
        .mutate(Mutation::Add(list, vec![dir.path().join("file.ac4")]));
    settle(&mut app, &context);
    let entry = app.library.browse.as_ref().unwrap().entries[0].id;
    app.play_browsed_entry(entry);
    settle(&mut app, &context);
    app.checkpoint.frame = 480_000;
    app.checkpoint.sample_rate = 48_000;
    app.activate_demo(true);
    app.playback_intent = false;
    app.library
        .mutate(Mutation::Create("Browse during demo".into()));
    settle(&mut app, &context);
    let other = app.library.desired_browse.unwrap();
    app.switch_playlist(list);
    app.switch_playlist(other);
    settle(&mut app, &context);
    assert_eq!(app.library.desired_playing, Some(list));
    app.library.mutate(Mutation::Delete(list));
    settle(&mut app, &context);
    app.flush_persistence();
    assert_eq!(app.checkpoint.cursor.as_ref().unwrap().playlist, None);
    assert_eq!(app.checkpoint.frame, 480_000);
    app.activate_demo(false);
    assert_eq!(app.cursor.as_ref().unwrap().entry.id, entry);
    assert_eq!(app.cursor.as_ref().unwrap().playlist, None);
    assert_eq!(app.resume.as_ref().unwrap().frame, 480_000);
    assert_eq!(app.library.desired_browse, Some(other));
}

#[test]
fn demo_explicit_file_play_replaces_demo() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    let list = app.library.desired_browse.unwrap();
    app.library
        .mutate(Mutation::Add(list, vec![dir.path().join("file.ac4")]));
    settle(&mut app, &context);
    let entry = app.library.browse.as_ref().unwrap().entries[0].id;
    app.activate_demo(true);
    app.play_browsed_entry(entry);
    assert!(
        matches!(app.playback_source(), Some(PlaybackSource::Media(_))),
        "explicit file playback still resolves to Demo (demo={})",
        app.demo
    );
}

#[test]
#[cfg(all(feature = "decode", any(macinrender_output, not(spatial_output))))]
fn demo_pause_widget_pauses_without_a_library_item() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    crate::theme::install(&context);
    settle(&mut app, &context);
    start_demo(&mut app, &context);
    let _ = ui_frame(&mut app, &context, 0.0, vec![]);
    let output = ui_frame(&mut app, &context, 0.1, vec![]);
    let text = painted_text(&output);
    let left = text.iter().find(|(s, _, _)| s == "◀◀").unwrap().1.center();
    let right = text.iter().find(|(s, _, _)| s == "▶▶").unwrap().1.center();
    let center = egui::pos2(left.x.midpoint(right.x), left.y.midpoint(right.y));
    let _ = click_widget(&mut app, &context, center, 1.0);
    assert!(
        !app.playback_intent,
        "clicking the visible pause button left playback running"
    );
    until(&mut app, &context, |app| {
        app.output.snapshot().phase() == crate::backend::OutputPhase::Paused
    });
    let paused_at = app.output.snapshot().playhead_frames();
    let output = ui_frame(&mut app, &context, 1.5, vec![]);
    let _ = click_widget(&mut app, &context, widget(&output, "▶"), 1.7);
    until(&mut app, &context, |app| app.output.snapshot().is_playing());
    assert!(app.output.snapshot().playhead_frames() >= paused_at);
}

#[test]
fn demo_stop_preserves_saved_file_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    let list = app.library.desired_browse.unwrap();
    app.library
        .mutate(Mutation::Add(list, vec![dir.path().join("file.ac4")]));
    settle(&mut app, &context);
    let entry = app.library.browse.as_ref().unwrap().entries[0].id;
    app.play_browsed_entry(entry);
    app.checkpoint.frame = 480_000;
    app.checkpoint.sample_rate = 48_000;
    app.activate_demo(true);
    assert_eq!(
        app.checkpoint.cursor.as_ref().map(|c| c.entry.id),
        Some(entry)
    );
    assert_eq!(app.checkpoint.frame, 480_000);
    app.activate_demo(false);
    assert!(!app.playback_intent);
    assert_eq!(app.resume.as_ref().unwrap().frame, 480_000);
    app.flush_persistence();
    app.library.shutdown();
    drop(app);
    let (mut restored, context) = open(dir.path());
    settle(&mut restored, &context);
    assert_eq!(
        restored.cursor.as_ref().map(|c| c.entry.id),
        Some(entry),
        "stopping the demo erased the saved file cursor"
    );
    assert_eq!(restored.checkpoint.frame, 480_000);
}
