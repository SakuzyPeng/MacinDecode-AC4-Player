use super::*;
use crate::preferences::DataDirectory;

fn open(path: &Path) -> (PlayerApp, egui::Context) {
    let context = egui::Context::default();
    let app = PlayerApp::from_storage(
        &context,
        None,
        false,
        DataDirectory::at(path.into()).unwrap(),
    );
    (app, context)
}
fn until(app: &mut PlayerApp, context: &egui::Context, predicate: impl Fn(&PlayerApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        app.tick(context, false);
        if predicate(app) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timeout: {} / {:?}",
            app.library.message,
            app.library.error
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn settle(app: &mut PlayerApp, context: &egui::Context) {
    until(app, context, |app| app.library.ready && !app.library.busy());
    assert!(app.library.error.is_none(), "{:?}", app.library.error);
}

fn painted_text(output: &egui::FullOutput) -> Vec<(String, egui::Rect, egui::Rect)> {
    fn visit(
        shape: &egui::Shape,
        clip: egui::Rect,
        text: &mut Vec<(String, egui::Rect, egui::Rect)>,
    ) {
        match shape {
            egui::Shape::Text(value) => text.push((
                value.galley.job.text.clone(),
                value.visual_bounding_rect(),
                clip,
            )),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    visit(shape, clip, text);
                }
            }
            _ => {}
        }
    }
    let mut text = Vec::new();
    for clipped in &output.shapes {
        visit(&clipped.shape, clipped.clip_rect, &mut text);
    }
    text
}

fn ui_frame(
    app: &mut PlayerApp,
    context: &egui::Context,
    time: f64,
    mut events: Vec<egui::Event>,
) -> egui::FullOutput {
    let modifiers = events
        .iter()
        .rev()
        .find_map(|event| match event {
            egui::Event::PointerButton { modifiers, .. } | egui::Event::Key { modifiers, .. } => {
                Some(*modifiers)
            }
            _ => None,
        })
        .unwrap_or_default();
    events.insert(0, egui::Event::ModifiersChanged(modifiers));
    let mut output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1180.0, 760.0),
            )),
            time: Some(time),
            events,
            focused: true,
            ..Default::default()
        },
        |ui| {
            app.draw_header(ui);
            app.draw_source_sidebar(ui);
            app.draw_transport(ui);
            for action in crate::playlist_ui::management(
                context,
                &app.library.summaries,
                &mut app.playlist_ui,
            ) {
                app.handle_playlist_action(action);
            }
        },
    );
    output.textures_delta.clear();
    output
}

fn click_widget(
    app: &mut PlayerApp,
    context: &egui::Context,
    pos: egui::Pos2,
    time: f64,
) -> egui::FullOutput {
    click_with_modifiers(app, context, pos, time, egui::Modifiers::NONE)
}

fn click_with_modifiers(
    app: &mut PlayerApp,
    context: &egui::Context,
    pos: egui::Pos2,
    time: f64,
    modifiers: egui::Modifiers,
) -> egui::FullOutput {
    let _ = ui_frame(
        app,
        context,
        time,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers,
            },
        ],
    );
    ui_frame(
        app,
        context,
        time + 0.04,
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers,
        }],
    )
}

#[test]
fn actual_widgets_select_on_single_click_play_on_double_click_and_create_lists() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    crate::theme::install(&context);
    settle(&mut app, &context);
    let id = app.library.desired_browse.unwrap();
    app.library.mutate(Mutation::Add(
        id,
        vec![dir.path().join("first.ac4"), dir.path().join("second.ac4")],
    ));
    settle(&mut app, &context);
    let second = app.library.browse.as_ref().unwrap().entries[1].id;
    let _ = ui_frame(&mut app, &context, 0.0, vec![]);
    let output = ui_frame(&mut app, &context, 0.1, vec![]);
    let position = painted_text(&output)
        .iter()
        .find(|(text, _, _)| text.contains("second.ac4"))
        .unwrap()
        .1
        .center();
    let _ = click_widget(&mut app, &context, position, 1.0);
    assert_eq!(app.browse.saved.focus, Some(second));
    assert!(
        app.cursor.is_none(),
        "single click must not open the decoder"
    );
    let output = click_widget(&mut app, &context, position, 1.15);
    assert_eq!(app.cursor.as_ref().unwrap().entry.id, second);
    let add = painted_text(&output)
        .iter()
        .find(|(text, _, _)| text == "+")
        .unwrap()
        .1
        .center();
    let _ = click_widget(&mut app, &context, add, 2.0);
    let output = ui_frame(&mut app, &context, 2.1, vec![]);
    let create = painted_text(&output)
        .iter()
        .find(|(text, _, _)| text == "Create")
        .unwrap()
        .1
        .center();
    let _ = click_widget(&mut app, &context, create, 3.0);
    settle(&mut app, &context);
    assert_eq!(app.library.summaries.len(), 2);
    assert_eq!(
        app.library.browse.as_ref().unwrap().summary.name,
        "New playlist"
    );
    assert_eq!(app.cursor.as_ref().unwrap().entry.id, second);
}

#[test]
fn enter_plays_the_focused_song_after_pointer_leaves_the_list() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    crate::theme::install(&context);
    settle(&mut app, &context);
    let id = app.library.desired_browse.unwrap();
    app.library
        .mutate(Mutation::Add(id, vec![dir.path().join("keyboard.ac4")]));
    settle(&mut app, &context);
    let entry = app.library.browse.as_ref().unwrap().entries[0].id;
    let _ = ui_frame(&mut app, &context, 0.0, vec![]);
    let output = ui_frame(&mut app, &context, 0.1, vec![]);
    let pos = painted_text(&output)
        .iter()
        .find(|(text, _, _)| text.contains("keyboard.ac4"))
        .unwrap()
        .1
        .center();
    let _ = click_widget(&mut app, &context, pos, 1.0);
    assert!(app.cursor.is_none());
    let _ = ui_frame(
        &mut app,
        &context,
        2.0,
        vec![
            egui::Event::PointerMoved(egui::pos2(800.0, 300.0)),
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    assert_eq!(
        app.cursor.as_ref().map(|cursor| cursor.entry.id),
        Some(entry)
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one input sequence carries selection and pointer state through the group drag"
)]
fn modifier_selection_select_all_and_group_drag_use_actual_row_events() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    crate::theme::install(&context);
    settle(&mut app, &context);
    let id = app.library.desired_browse.unwrap();
    app.library.mutate(Mutation::Add(
        id,
        (0..4)
            .map(|i| dir.path().join(format!("group-{i}.ac4")))
            .collect(),
    ));
    settle(&mut app, &context);
    let ids: Vec<_> = app
        .library
        .browse
        .as_ref()
        .unwrap()
        .entries
        .iter()
        .map(|entry| entry.id)
        .collect();
    let _ = ui_frame(&mut app, &context, 0.0, vec![]);
    let output = ui_frame(&mut app, &context, 0.1, vec![]);
    let text = painted_text(&output);
    let points: Vec<_> = (0..4)
        .map(|i| {
            text.iter()
                .find(|(text, _, _)| text.contains(&format!("group-{i}.ac4")))
                .unwrap()
                .1
                .center()
        })
        .collect();
    let command = egui::Modifiers {
        command: true,
        ctrl: true,
        ..Default::default()
    };
    let shift = egui::Modifiers {
        shift: true,
        ..Default::default()
    };
    let _ = click_widget(&mut app, &context, points[0], 1.0);
    let _ = click_with_modifiers(&mut app, &context, points[2], 2.0, command);
    assert_eq!(
        app.browse
            .ordered_selection(app.library.browse.as_ref().unwrap()),
        [ids[0], ids[2]]
    );
    let _ = click_with_modifiers(&mut app, &context, points[3], 3.0, shift);
    assert_eq!(
        app.browse
            .ordered_selection(app.library.browse.as_ref().unwrap()),
        [ids[2], ids[3]]
    );
    let _ = ui_frame(
        &mut app,
        &context,
        4.0,
        vec![egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: command,
        }],
    );
    assert_eq!(app.browse.selected.len(), 4);
    let _ = click_widget(&mut app, &context, points[0], 5.0);
    let _ = click_with_modifiers(&mut app, &context, points[2], 6.0, command);
    let target = points[3] + egui::vec2(0.0, 8.0);
    let _ = ui_frame(
        &mut app,
        &context,
        7.0,
        vec![
            egui::Event::PointerMoved(points[0]),
            egui::Event::PointerButton {
                pos: points[0],
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    let _ = ui_frame(
        &mut app,
        &context,
        7.1,
        vec![egui::Event::PointerMoved(target)],
    );
    let _ = ui_frame(
        &mut app,
        &context,
        7.2,
        vec![egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    settle(&mut app, &context);
    let order: Vec<_> = app
        .library
        .browse
        .as_ref()
        .unwrap()
        .entries
        .iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(order, [ids[1], ids[3], ids[0], ids[2]]);
    assert!(app.cursor.is_none(), "dragging must not start playback");
}

#[test]
#[ignore = "50,000-row UI capacity benchmark including an all-selected playlist"]
fn large_library_ui_render_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    crate::theme::install(&context);
    settle(&mut app, &context);
    let id = app.library.desired_browse.unwrap();
    app.library.mutate(Mutation::Add(
        id,
        (0..50_000)
            .map(|i| dir.path().join(format!("track-{i:05}.ac4")))
            .collect(),
    ));
    settle(&mut app, &context);
    let list = app.library.browse.clone().unwrap();
    app.browse.selected = list.entries.iter().map(|entry| entry.id).collect();
    let mut times = Vec::new();
    let mut maximum_rows = 0;
    for frame in 0..204 {
        app.browse.saved.scroll_entry = Some(list.entries[(frame * 157) % list.entries.len()].id);
        app.browse.restore_scroll = true;
        let start = Instant::now();
        app.tick(&context, true);
        let output = ui_frame(
            &mut app,
            &context,
            f64::from(u32::try_from(frame).unwrap()) / 60.0,
            vec![],
        );
        let elapsed = start.elapsed();
        let rows = painted_text(&output)
            .iter()
            .filter(|(text, _, _)| text.contains("track-"))
            .count();
        maximum_rows = maximum_rows.max(rows);
        if frame >= 4 {
            times.push(elapsed);
        }
    }
    times.sort_unstable();
    assert!(
        maximum_rows < 20,
        "offscreen rows were drawn: {maximum_rows}"
    );
    assert_eq!(app.browse.selected.len(), 50_000);
    eprintln!(
        "UI 50k/all selected: median={:?}, p95={:?}, max visible row texts={maximum_rows}",
        times[times.len() / 2],
        times[times.len() * 95 / 100]
    );
}

#[test]
fn sidebar_titles_and_status_fit_at_default_and_minimum_window_sizes() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    crate::theme::install(&context);
    settle(&mut app, &context);
    let list = app.library.desired_browse.unwrap();
    app.library.mutate(Mutation::Add(
        list,
        (0..30)
            .map(|i| dir.path().join(format!("song-{i}.ac4")))
            .collect(),
    ));
    settle(&mut app, &context);
    app.library.message = "Library ready".into();
    for size in [egui::vec2(1180.0, 760.0), egui::vec2(920.0, 620.0)] {
        for error in [
            None,
            Some("Not saved: a long storage error that must stay on one line".to_owned()),
        ] {
            app.library.error = error;
            let mut output = egui::FullOutput::default();
            for _ in 0..2 {
                output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        ..Default::default()
                    },
                    |ui| {
                        app.draw_header(ui);
                        app.draw_source_sidebar(ui);
                        app.draw_transport(ui);
                    },
                );
                output.textures_delta.clear();
            }
            let text = painted_text(&output);
            let title = text
                .iter()
                .find(|(value, _, _)| value == "BITSTREAM INFO")
                .expect("bitstream title")
                .1;
            let status = text
                .iter()
                .find(|(value, _, _)| value == "Library ready" || value.starts_with("Not saved:"))
                .expect("library status");
            assert!(
                status.2.expand(0.5).contains_rect(status.1),
                "status clipped at {size:?}: {status:?}"
            );
            let source_card = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Rect(r)
                        if r.fill == crate::theme::SURFACE
                            && r.rect.width() > 200.0
                            && r.rect.right() < 310.0 =>
                    {
                        Some(r.rect)
                    }
                    _ => None,
                })
                .min_by(|a, b| a.top().total_cmp(&b.top()))
                .expect("playlist card");
            assert!(
                source_card.bottom() + 8.0 < title.top(),
                "card overlaps title at {size:?}: {source_card:?}, {title:?}"
            );
            assert!(status.1.bottom() < source_card.bottom());
            assert!(
                text.iter()
                    .filter(|(value, _, _)| value.contains("song-"))
                    .count()
                    < 15,
                "virtual rows painted offscreen songs"
            );
        }
    }
}

#[test]
fn browsing_removal_and_reordering_do_not_reopen_the_playback_source() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    let a = app.library.desired_browse.unwrap();
    app.library.mutate(Mutation::Add(
        a,
        (0..3)
            .map(|i| dir.path().join(format!("{i}.ac4")))
            .collect(),
    ));
    settle(&mut app, &context);
    let entries = app.library.browse.as_ref().unwrap().entries.clone();
    app.play_browsed_entry(entries[0].id);
    settle(&mut app, &context);
    let request = app.decoder.request_id();
    app.library.mutate(Mutation::Create("B".into()));
    settle(&mut app, &context);
    let b = app.library.desired_browse.unwrap();
    assert_ne!(a, b);
    assert_eq!(app.cursor.as_ref().unwrap().entry.id, entries[0].id);
    assert_eq!(app.decoder.request_id(), request);
    app.library.mutate(Mutation::Remove(a, vec![entries[0].id]));
    settle(&mut app, &context);
    let cursor = app.cursor.as_ref().unwrap();
    assert!(!cursor.attached);
    assert_eq!(cursor.next_anchor, Some(entries[1].id));
    app.library.mutate(Mutation::Remove(a, vec![entries[1].id]));
    settle(&mut app, &context);
    assert_eq!(
        app.cursor.as_ref().unwrap().next_anchor,
        Some(entries[2].id)
    );
    app.library.mutate(Mutation::Delete(a));
    settle(&mut app, &context);
    assert_eq!(app.cursor.as_ref().unwrap().playlist, None);
    assert_eq!(app.decoder.request_id(), request);
    app.flush_persistence();
    app.library.shutdown();
    drop(app);
    let (mut restored, context) = open(dir.path());
    settle(&mut restored, &context);
    assert_eq!(restored.library.desired_browse, Some(b));
    assert_eq!(restored.cursor.as_ref().unwrap().entry.id, entries[0].id);
    assert_eq!(restored.cursor.as_ref().unwrap().playlist, None);
    assert!(!restored.playback_intent);
}

#[test]
fn late_list_queries_and_a_pending_resume_cannot_override_an_explicit_play() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    let a = app.library.desired_browse.unwrap();
    app.library
        .mutate(Mutation::Add(a, vec![dir.path().join("a.ac4")]));
    settle(&mut app, &context);
    let entry = app.library.browse.as_ref().unwrap().entries[0].clone();
    app.library.mutate(Mutation::Create("B".into()));
    settle(&mut app, &context);
    let b = app.library.desired_browse.unwrap();
    app.switch_playlist(a);
    app.switch_playlist(b);
    app.switch_playlist(a);
    settle(&mut app, &context);
    assert_eq!(app.browse.playlist, Some(a));
    assert_eq!(app.library.browse.as_ref().unwrap().summary.id, a);
    app.resume = Some(SessionState {
        frame: 999_999,
        cursor: Some(PlaybackCursor::new(a, entry.clone())),
        ..Default::default()
    });
    app.play_browsed_entry(entry.id);
    assert!(app.resume.is_none());
    assert_eq!(app.checkpoint.frame, 0);
    settle(&mut app, &context);
}

#[test]
#[cfg(feature = "decode")]
fn inspection_and_playback_share_the_open_media_and_explicit_retry_opens_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.ac4");
    std::fs::write(&path, [0xac, 0x40, 0, 0, 0, 0]).unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    let list = app.library.desired_browse.unwrap();
    app.library.mutate(Mutation::Add(list, vec![path]));
    settle(&mut app, &context);
    let inspected = app.browsed_media().unwrap().open().unwrap();
    let entry = app.library.browse.as_ref().unwrap().entries[0].id;
    app.play_browsed_entry(entry);
    let playing = app.playback_media().unwrap().open().unwrap();
    assert!(Arc::ptr_eq(&inspected, &playing));
    app.retry_playback();
    let retried = app.playback_media().unwrap().open().unwrap();
    assert!(!Arc::ptr_eq(&playing, &retried));
}

#[test]
#[cfg(not(feature = "decode"))]
fn inspection_only_releases_the_open_file_when_a_report_is_finished() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inspection.ac4");
    std::fs::write(&path, [0xac, 0x40, 0, 0]).unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    let id = app.library.desired_browse.unwrap();
    app.library.mutate(Mutation::Add(id, vec![path]));
    settle(&mut app, &context);
    until(&mut app, &context, |app| !app.inspection.has_pending());
    let source = app.browsed_media().unwrap();
    let opened = source.open().unwrap();
    let weak = Arc::downgrade(&opened);
    drop(opened);
    drop(source);
    assert!(
        weak.upgrade().is_none(),
        "the inspection-only UI must not retain an open file"
    );
}

#[test]
fn pending_restore_keeps_async_settings_dialogs_moving() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    let polled = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&polled);
    app.sofa_picker = Some(Box::pin(async move {
        signal.store(true, Ordering::SeqCst);
        None
    }));
    app.resume = Some(SessionState::default());
    app.tick(&context, false);
    assert!(polled.load(Ordering::SeqCst));
    assert!(app.sofa_picker.is_none());
}

#[test]
fn pending_audio_settings_do_not_replace_committed_preferences() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    let old = app.preferences.output.clone();
    let mut candidate = old.clone();
    candidate.layout = crate::backend::SpeakerLayout::TwentyTwoTwo;
    app.output.install_settings(candidate);
    app.pending_output_change = Some(old.clone());
    app.volume = 0.42;
    app.muted = true;
    app.flush_persistence();
    app.library.shutdown();
    drop(app);
    let (mut restored, context) = open(dir.path());
    settle(&mut restored, &context);
    assert_eq!(restored.preferences.output, old);
    assert!((restored.volume - 0.42).abs() < f32::EPSILON);
    assert!(restored.muted);
}

#[test]
fn retry_keeps_the_error_until_pending_preferences_are_actually_saved() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    let path = dir.path().join("settings.json");
    let original = std::fs::read(&path).unwrap();
    // Simulate a damaged primary during a running session, without a recovery backup.
    std::fs::write(&path, b"broken").unwrap();
    let backup = dir.path().join("settings.json.bak");
    if backup.exists() {
        std::fs::remove_file(backup).unwrap();
    }
    app.volume = 0.23;
    app.flush_persistence();
    until(&mut app, &context, |app| {
        !app.library.busy() && app.library.error.is_some()
    });
    app.library.retry();
    assert!(
        app.library.error.is_some(),
        "requesting retry is not a successful save"
    );
    until(&mut app, &context, |app| !app.library.busy());
    assert!(app.library.error.is_some());
    assert_eq!(std::fs::read(&path).unwrap(), b"broken");
    std::fs::write(&path, original).unwrap();
    app.library.retry();
    settle(&mut app, &context);
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!((saved["preferences"]["volume"].as_f64().unwrap() - 0.23).abs() < 1e-6);
}

#[test]
fn media_error_updates_reuse_list_snapshots_and_are_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    let id = app.library.desired_browse.unwrap();
    app.library
        .mutate(Mutation::Add(id, vec![dir.path().join("missing.ac4")]));
    settle(&mut app, &context);
    let before = app.library.browse.clone().unwrap();
    let media = before.entries[0].media;
    app.library
        .mutate(Mutation::MediaError(media, Some("File missing".into())));
    settle(&mut app, &context);
    assert!(Arc::ptr_eq(&before, app.library.browse.as_ref().unwrap()));
    assert_eq!(
        app.library.media_errors[&media].as_deref(),
        Some("File missing")
    );
    app.library.mutate(Mutation::Rename(id, "Renamed".into()));
    settle(&mut app, &context);
    assert!(app.library.media_errors.is_empty());
    assert_eq!(
        app.library.browse.as_ref().unwrap().entries[0]
            .error
            .as_deref(),
        Some("File missing")
    );
    app.library.mutate(Mutation::MediaError(media, None));
    settle(&mut app, &context);
    assert!(app.library.media_errors[&media].is_none());
}

#[cfg(macinrender_output)]
#[test]
#[ignore = "requires MACINDECODE_AC4_TEST_MEDIA; silent native playback and app restart"]
fn real_playback_browse_pause_and_restart_restore_the_output_position() {
    let media = std::env::var_os("MACINDECODE_AC4_TEST_MEDIA").expect("real media fixture");
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    let mut settings = app.output.settings().clone();
    settings.null_output = true;
    settings.mode = crate::backend::SpatialBackendKind::SafBinaural;
    settings.head_source = crate::head_tracking::HeadSource::Off;
    app.output.install_settings(settings);
    let a = app.library.desired_browse.unwrap();
    app.library.mutate(Mutation::Add(a, vec![media.into()]));
    settle(&mut app, &context);
    let entry = app.library.browse.as_ref().unwrap().entries[0].id;
    app.play_browsed_entry(entry);
    until(&mut app, &context, |app| {
        app.output.snapshot().playhead_frames() > 12_000
    });
    let request = app.decoder.request_id();
    app.library
        .mutate(Mutation::Create("Browse while playing".into()));
    settle(&mut app, &context);
    assert_eq!(app.decoder.request_id(), request);
    assert!(app.output.snapshot().is_playing());
    app.playback_intent = false;
    app.output.pause();
    app.tick(&context, false);
    app.flush_persistence();
    let frame = app.checkpoint.frame;
    assert!(frame > 0);
    app.library.shutdown();
    drop(app);
    let (mut restored, context) = open(dir.path());
    // Install a silent test endpoint before permitting decoder/output work.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !restored.library.ready {
        restored.poll_library(&context);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(restored.checkpoint.frame, frame);
    let mut settings = restored.output.settings().clone();
    settings.null_output = true;
    restored.output.install_settings(settings);
    until(&mut restored, &context, |app| {
        app.resume.is_none() && app.output.snapshot().phase() == crate::backend::OutputPhase::Paused
    });
    assert!(!restored.playback_intent);
    assert_eq!(restored.cursor.as_ref().unwrap().entry.id, entry);
    assert_eq!(restored.output.snapshot().playhead_frames(), frame);
}
