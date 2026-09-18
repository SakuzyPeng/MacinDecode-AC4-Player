use super::*;
use crate::preferences::DataDirectory;

mod demo;

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
            app.draw_visual_settings(context);
            app.draw_output_settings(context);
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
fn drawing_a_fallback_audio_page_keeps_the_remembered_choice() {
    use crate::app::OutputPage;

    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    app.output_settings_open = true;
    app.output_page = OutputPage::Headphones;
    for time in [0.0, 0.1] {
        let _ = ui_frame(&mut app, &context, time, vec![]);
    }
    // Automatic has no Headphones page on any platform. Drawing its fallback
    // must not forget the choice to restore when returning to binaural.
    assert_eq!(app.output_page, OutputPage::Headphones);
}

#[test]
#[cfg(macinrender_output)]
fn audio_page_round_trips_preserve_choices_until_a_tab_is_clicked() {
    use crate::app::OutputPage;
    use crate::backend::{OutputSettings, SpatialBackendKind};

    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    crate::theme::install(&context);
    app.output_settings_open = true;
    let mut settings = OutputSettings {
        null_output: true,
        mode: SpatialBackendKind::SafBinaural,
        head_source: crate::head_tracking::HeadSource::Manual,
        ..Default::default()
    };
    app.output.install_settings(settings.clone());
    let _ = ui_frame(&mut app, &context, 0.0, vec![]);
    let output = ui_frame(&mut app, &context, 0.1, vec![]);
    let widget = |output: &egui::FullOutput, label: &str| {
        painted_text(output)
            .into_iter()
            .find(|(text, _, _)| text == label)
            .unwrap_or_else(|| panic!("missing widget {label}"))
            .1
            .center()
    };
    let _ = click_widget(&mut app, &context, widget(&output, "Headphones"), 1.0);
    assert_eq!(app.output_page, OutputPage::Headphones);

    settings.mode = SpatialBackendKind::SystemSpatial;
    app.output.install_settings(settings.clone());
    let output = ui_frame(&mut app, &context, 2.0, vec![]);
    let _ = widget(&output, "Speaker layout");
    assert_eq!(app.output_page, OutputPage::Headphones);

    settings.mode = SpatialBackendKind::SafBinaural;
    app.output.install_settings(settings.clone());
    let output = ui_frame(&mut app, &context, 3.0, vec![]);
    let _ = widget(&output, "Choose AutoEq profile…");
    assert_eq!(app.output_page, OutputPage::Headphones);

    // Even clicking the already displayed fallback is an explicit choice:
    // Response::changed() would miss this click and retain Headphones.
    settings.mode = SpatialBackendKind::SystemSpatial;
    app.output.install_settings(settings.clone());
    let _ = ui_frame(&mut app, &context, 4.0, vec![]);
    let output = ui_frame(&mut app, &context, 4.1, vec![]);
    let _ = click_widget(&mut app, &context, widget(&output, "Speakers"), 5.0);
    assert_eq!(app.output_page, OutputPage::Speakers);

    settings.mode = SpatialBackendKind::SafBinaural;
    app.output.install_settings(settings);
    let output = ui_frame(&mut app, &context, 6.0, vec![]);
    let _ = widget(&output, "HRTF: built-in KEMAR");
    assert_eq!(app.output_page, OutputPage::Speakers);
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
fn the_object_visual_switches_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    // Every one of them has to be moved off its default for the assertions
    // below to mean that a value made the round trip rather than that a default
    // happened to agree with it.
    assert!(app.object_numbers_visible);
    assert!(app.object_loudness_visible);
    assert!(app.fade_silent_objects);
    assert!(!app.meter_bank_open);
    assert_eq!(app.meter_readout, crate::app::MeterReadout::Fast);
    app.object_numbers_visible = false;
    app.object_loudness_visible = false;
    app.fade_silent_objects = false;
    app.meter_bank_open = true;
    app.meter_readout = crate::app::MeterReadout::Momentary;
    app.flush_persistence();
    app.library.shutdown();
    drop(app);

    let (mut restored, context) = open(dir.path());
    settle(&mut restored, &context);
    assert!(!restored.object_numbers_visible);
    assert!(!restored.object_loudness_visible);
    assert!(!restored.fade_silent_objects);
    assert!(restored.meter_bank_open);
    assert_eq!(
        restored.meter_readout,
        crate::app::MeterReadout::Momentary,
        "the bank's unit is a preference, not a per-session mode"
    );
}

#[test]
fn skin_import_switch_override_and_restore_use_the_visual_settings_widgets() {
    use crate::scene3d::skin::{
        BodyType,
        tests::{png, sample_image},
    };
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    crate::theme::install(&context);
    settle(&mut app, &context);
    for model in [BodyType::Steve, BodyType::Alex] {
        let path = dir.path().join(format!("{model:?} test.png"));
        std::fs::write(&path, png(&sample_image(model, false))).unwrap();
        app.skins.import(path.clone(), &context);
        until(&mut app, &context, |app| !app.skins.busy());
        assert_eq!(app.skins.active.as_ref().unwrap().model, model);
        std::fs::remove_file(path).unwrap();
    }
    assert_eq!(app.preferences.skins.entries.len(), 2);
    let selected = app.preferences.skins.selected.clone();
    app.skins.import(dir.path().join("missing.png"), &context);
    until(&mut app, &context, |app| !app.skins.busy());
    assert!(app.skins.error.is_some());
    assert_eq!(app.preferences.skins.selected, selected);
    assert_eq!(app.skins.active.as_ref().unwrap().model, BodyType::Alex);

    let _ = ui_frame(&mut app, &context, 0.0, vec![]);
    let output = ui_frame(&mut app, &context, 0.1, vec![]);
    let widget = |output: &egui::FullOutput, label: &str| {
        painted_text(output)
            .into_iter()
            .find(|(text, _, _)| text == label)
            .unwrap_or_else(|| panic!("missing widget {label}: {:?}", painted_text(output)))
            .1
            .center()
    };
    let _ = click_widget(&mut app, &context, widget(&output, "Visual settings"), 1.0);
    assert!(app.visual_settings_open);
    let output = ui_frame(&mut app, &context, 1.2, vec![]);
    assert!(
        painted_text(&output)
            .iter()
            .any(|(text, _, _)| text == "Import skin PNG…")
    );
    let _ = click_widget(&mut app, &context, widget(&output, "Alex test"), 2.0);
    let output = ui_frame(&mut app, &context, 2.2, vec![]);
    let _ = click_widget(&mut app, &context, widget(&output, "Steve test"), 3.0);
    until(&mut app, &context, |app| !app.skins.busy());
    assert_eq!(app.skins.active.as_ref().unwrap().model, BodyType::Steve);
    assert!(app.skins.error.is_none());
    let output = ui_frame(&mut app, &context, 3.2, vec![]);
    let (_, meter_label, clip) = painted_text(&output)
        .into_iter()
        .find(|(text, _, _)| text == "Meter bank (side panel)")
        .unwrap_or_else(|| panic!("missing meter switch: {:?}", painted_text(&output)));
    assert!(
        clip.contains_rect(meter_label),
        "the last visual switch must fit at the default window size"
    );
    let _ = click_widget(
        &mut app,
        &context,
        widget(&output, "Auto: Steve (classic, 4 px arms)"),
        4.0,
    );
    let output = ui_frame(&mut app, &context, 4.2, vec![]);
    let _ = click_widget(
        &mut app,
        &context,
        widget(&output, "Alex (slim, 3 px arms)"),
        5.0,
    );
    assert_eq!(app.skins.active.as_ref().unwrap().model, BodyType::Alex);
    app.flush_persistence();
    app.library.shutdown();
    drop(app);

    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    until(&mut app, &context, |app| !app.skins.busy());
    let skin = app.skins.active.as_ref().unwrap();
    assert_eq!(skin.detected, BodyType::Steve);
    assert_eq!(skin.model, BodyType::Alex);
    assert_eq!(app.preferences.skins.entries.len(), 2);
    app.visual_settings_open = true;
    let _ = ui_frame(&mut app, &context, 0.0, vec![]);
    let output = ui_frame(&mut app, &context, 0.1, vec![]);
    let _ = click_widget(&mut app, &context, widget(&output, "Steve test"), 1.0);
    let output = ui_frame(&mut app, &context, 1.2, vec![]);
    let _ = click_widget(&mut app, &context, widget(&output, "Default figure"), 2.0);
    assert!(app.skins.active.is_none());
    assert!(app.preferences.skins.selected.is_none());
    assert_eq!(app.preferences.skins.entries.len(), 2);
}

#[test]
fn meter_unit_switches_lfe_and_object_rows_and_scene_readouts() {
    use super::super::MeterReadout;
    use crate::scene_view::ObjectView;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    crate::theme::install(&context);
    settle(&mut app, &context);
    app.meter_bank_open = true;
    let key = app.decoder.playback_key();
    // A loud 370 ms followed by a quiet 30 ms distinguishes the actual
    // averaging windows as well as their numeric unit offsets.
    for bin in 0..40 {
        let lfe = measured_lfe(if bin < 37 { 0.5 } else { 0.05 }, 480);
        app.output.scene_view().write_with_lfe(
            key,
            [ObjectView {
                element_id: 7,
                active: true,
                gain: 1.0,
                position: [0.3, 0.4, -0.2],
                energy: lfe.energy,
                ..Default::default()
            }],
            Some(lfe),
            i64::from(bin * 480),
            48_000,
        );
    }
    let frame = app.output.scene_view().read(key).unwrap();
    let start = Instant::now();
    for step in 0..3 {
        app.object_meters
            .advance(&frame, key, 48_000, start + Duration::from_secs(step));
    }
    for (readout, expected) in [
        (MeterReadout::Fast, "26.0"),
        (MeterReadout::Momentary, "7.0"),
    ] {
        app.meter_readout = readout;
        let _ = meter_bank_frame(&mut app, &context, &frame, egui::vec2(1180.0, 760.0), 0.0);
        let output = meter_bank_frame(&mut app, &context, &frame, egui::vec2(1180.0, 760.0), 0.0);
        let text = painted_text(&output);
        for label in ["0", "1", readout.unit()] {
            assert!(
                text.iter().any(|(text, ..)| text == label),
                "missing meter label {label}"
            );
        }
        assert_eq!(
            text.iter().filter(|(text, ..)| text == expected).count(),
            2,
            "LFE and object meter must both use {readout:?}"
        );
        assert!(
            !text
                .iter()
                .any(|(text, ..)| text.contains('→') || text == "0: dBFS")
        );
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 640.0),
                )),
                ..Default::default()
            },
            |ui| app.draw_scene(ui, Some(&frame)),
        );
        output.textures_delta.clear();
        assert_eq!(
            painted_text(&output)
                .iter()
                .filter(|(text, ..)| text == expected)
                .count(),
            2,
            "LFE and object nameplates must both use {readout:?}"
        );
    }
}

fn measured_lfe(level: f32, frames: u32) -> crate::scene_view::LfeView {
    crate::scene_view::LfeView {
        element_id: 99,
        active: true,
        gain: 1.0,
        energy: crate::scene_view::ObjectEnergy {
            sum_squares: f64::from(level).powi(2) * f64::from(frames),
            frames,
            peak: level,
        },
    }
}

fn meter_bank_frame(
    app: &mut PlayerApp,
    context: &egui::Context,
    frame: &crate::scene_view::SceneViewFrame,
    size: egui::Vec2,
    scroll_delta: f32,
) -> egui::FullOutput {
    let mut bank_rect = egui::Rect::NOTHING;
    let mut bank_shapes = Vec::new();
    let mut events = vec![egui::Event::PointerMoved(egui::pos2(
        size.x - 120.0,
        size.y / 2.0,
    ))];
    if scroll_delta != 0.0 {
        events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, scroll_delta),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
    }
    let mut output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            focused: true,
            ..Default::default()
        },
        |ui| {
            // Reserve space using the actual panels, including the bottom bar.
            app.draw_header(ui);
            app.draw_source_sidebar(ui);
            app.draw_transport(ui);
            bank_rect = ui.available_rect_before_wrap();
            bank_rect.min.x = bank_rect.right() - 240.0;
            let start = context.graphics(|layers| layers.get(ui.layer_id()).unwrap().next_idx().0);
            app.draw_meter_bank(ui, Some(frame));
            bank_shapes = context.graphics(|layers| {
                layers
                    .get(ui.layer_id())
                    .unwrap()
                    .all_entries()
                    .skip(start)
                    .cloned()
                    .collect()
            });
        },
    );
    output.textures_delta.clear();
    output.shapes = bank_shapes;
    // Check all visible paint, including separators drawn on the parent layer.
    for clipped in &output.shapes {
        let visible = clipped
            .shape
            .visual_bounding_rect()
            .intersect(clipped.clip_rect);
        if visible.is_positive() {
            assert!(
                bank_rect.expand(0.5).contains_rect(visible),
                "meter bank paints {visible:?} outside {bank_rect:?}"
            );
        }
    }
    output
}

#[test]
fn every_meter_row_fits_inside_the_bank_and_reads_out_what_it_measured() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    crate::theme::install(&context);
    settle(&mut app, &context);
    app.meter_bank_open = true;

    // A full bank, with one object at full scale, one twelve decibels down,
    // one that clipped, and the rest silent.
    let key = app.decoder.playback_key();
    let bin = crate::scene_view::loudness_bin_frames(48_000);
    let objects: Vec<_> = (0..crate::scene_view::MAX_VIEW_OBJECTS)
        .map(|slot| {
            let mean_square = match slot {
                0 => 1.0,
                1 => 0.063_095_734,
                _ => 0.0,
            };
            crate::scene_view::ObjectView {
                element_id: u64::try_from(slot).unwrap() + 1,
                active: true,
                gain: 1.0,
                energy: crate::scene_view::ObjectEnergy {
                    sum_squares: mean_square * f64::from(bin),
                    frames: bin,
                    peak: if slot == 3 { 1.0 } else { 0.0 },
                },
                ..Default::default()
            }
        })
        .collect();
    app.output
        .scene_view()
        .write_with_lfe(key, objects, Some(measured_lfe(0.25, bin)), 0, 48_000);
    let frame = app
        .output
        .scene_view()
        .read(key)
        .expect("the mirror was just written");
    // Several long steps, so the ballistics have arrived rather than started.
    let start = Instant::now();
    for step in 0..6 {
        app.object_meters
            .advance(&frame, key, 48_000, start + Duration::from_secs(step));
    }

    for size in [egui::vec2(1180.0, 760.0), egui::vec2(920.0, 620.0)] {
        // Scroll to the start after resizing, allowing the scroll state to settle.
        let mut output = egui::FullOutput::default();
        for _ in 0..8 {
            output = meter_bank_frame(&mut app, &context, &frame, size, 1000.0);
        }
        let text = painted_text(&output);
        for number in 0..=crate::scene_view::MAX_VIEW_OBJECTS {
            let number = number.to_string();
            assert!(
                text.iter().any(|(value, ..)| *value == number),
                "no row {number}"
            );
        }
        for expected in ["0.0", "12.0", "∞"] {
            assert!(
                text.iter().any(|(value, ..)| value == expected),
                "no readout {expected}"
            );
        }
        // Even clipped rows must keep their readouts within the column width.
        for (value, bounds, _) in text.iter().filter(|(value, ..)| !value.trim().is_empty()) {
            assert!(
                bounds.left() >= size.x - 241.0 && bounds.right() <= size.x + 1.0,
                "{value:?} painted at {bounds:?}, outside the bank"
            );
        }
        let header = text
            .iter()
            .find(|(value, ..)| value == "METER BANK")
            .unwrap()
            .1;
        for _ in 0..8 {
            output = meter_bank_frame(&mut app, &context, &frame, size, -1000.0);
        }
        let scrolled = painted_text(&output);
        assert!(
            scrolled
                .iter()
                .any(|(value, bounds, clip)| value == "20" && clip.contains_rect(*bounds)),
            "the last row cannot be reached at {size:?}"
        );
        assert_eq!(
            scrolled
                .iter()
                .find(|(value, ..)| value == "METER BANK")
                .unwrap()
                .1,
            header,
            "scrolling the rows moved the bank header"
        );
    }
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

fn settle_hptf_catalog(app: &mut PlayerApp, context: &egui::Context) {
    // The managed folders are driven from `logic`, which no harness pass
    // reaches, so turn that one crank explicitly.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        app.tick(context, false);
        app.poll_file_catalogs(context);
        if app.hptf.started && !app.hptf.busy() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the profile folder never settled"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Saving a tuned profile has one invariant worth a test: selecting the new
/// file and putting the knobs back are *one* settings change. Split them and a
/// frame of the new profile — which already has the adjustment in its bands —
/// runs with the knobs still up, applying it twice.
#[test]
fn saving_a_tuned_profile_selects_it_and_puts_the_knobs_back() {
    use crate::hptf_profile::{Adjustment, Profile};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    settle_hptf_catalog(&mut app, &context);

    let adjustment = Adjustment {
        bass_db: 2.5,
        tilt_db_per_octave: -1.0,
    };
    let mut settings = app.output.settings().clone();
    settings.hptf_bass_db = 2.5;
    settings.hptf_tilt_db = -1.0;
    app.change_output_settings(settings, &context);
    // Without this the assertions below would pass on a test that never turned
    // a knob in the first place.
    assert!((app.output.settings().hptf_bass_db - 2.5).abs() < f32::EPSILON);

    // Staged the way the Save button stages it: the adjustment already in the
    // bands, under the name the folder will keep.
    let tuned = Profile::parse("Preamp: -1 dB\nFilter 1: ON PK Fc 1000 Hz Gain 2 dB Q 1\n")
        .unwrap()
        .with(adjustment)
        .unwrap();
    let name = "Tuned (bass +2.5 dB, tilt -1.00 dB per octave).txt";
    let staging = tempfile::tempdir().unwrap();
    let file = staging.path().join(name);
    std::fs::write(&file, tuned.to_parametric_eq()).unwrap();
    app.hptf_saving = Some(staging);
    app.hptf.refresh(Some(file), &context);
    settle_hptf_catalog(&mut app, &context);

    let settings = app.output.settings().clone();
    assert!(settings.hptf.ends_with(name), "{}", settings.hptf);
    assert!(settings.hptf_bass_db == 0.0 && settings.hptf_tilt_db == 0.0);
    assert!(
        app.hptf_saving.is_none(),
        "the staging copy is released once the import has read it"
    );

    // What landed in hptf/ is the cascade that was staged, and it carries the
    // file's one band plus the adjustment's four.
    let landed = Profile::parse(&std::fs::read_to_string(&settings.hptf).unwrap()).unwrap();
    assert_eq!(tuned.disagreement(&landed), None);
    assert_eq!(
        landed.bands(),
        1 + u32::try_from(adjustment.bands().len()).unwrap()
    );

    app.flush_persistence();
    app.library.shutdown();
}

#[test]
fn failed_profile_save_releases_staging_and_preserves_later_import_adjustments() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, context) = open(dir.path());
    settle(&mut app, &context);
    settle_hptf_catalog(&mut app, &context);

    let current = dir.path().join("current.txt");
    std::fs::write(&current, "Preamp: -2 dB\n").unwrap();
    let mut original = app.output.settings().clone();
    original.hptf = current.to_str().unwrap().to_owned();
    original.hptf_bass_db = 2.5;
    original.hptf_tilt_db = -1.0;
    app.change_output_settings(original.clone(), &context);
    assert_eq!(app.output.settings(), &original);

    // Fail the import deterministically, without relying on permissions or
    // available disk space: this isolated destination is no longer a folder.
    std::fs::remove_dir(&app.hptf.root).unwrap();
    std::fs::write(&app.hptf.root, b"not a directory").unwrap();
    let staging = tempfile::tempdir().unwrap();
    let staged_path = staging.path().to_path_buf();
    let saved = staged_path.join("saved.txt");
    std::fs::write(&saved, "Preamp: -3 dB\n").unwrap();
    app.hptf_saving = Some(staging);
    app.hptf.refresh(Some(saved), &context);
    settle_hptf_catalog(&mut app, &context);

    assert!(app.hptf.message.contains("regular"), "{}", app.hptf.message);
    assert!(app.hptf_saving.is_none());
    assert!(
        !staged_path.exists(),
        "failed saves must release their staging copy"
    );
    assert_eq!(app.output.settings(), &original);

    // Recover the folder, then import an ordinary profile through the same
    // path as the file picker. Its selection must preserve both adjustments.
    std::fs::remove_file(&app.hptf.root).unwrap();
    std::fs::create_dir(&app.hptf.root).unwrap();
    let plain = dir.path().join("ordinary-import.txt");
    std::fs::write(&plain, "Preamp: -1 dB\n").unwrap();
    app.hptf.refresh(Some(plain), &context);
    settle_hptf_catalog(&mut app, &context);

    let imported = app.hptf.root.join("ordinary-import.txt");
    assert!(imported.is_file());
    let mut expected = original;
    expected.hptf = imported.to_str().unwrap().to_owned();
    assert_eq!(app.output.settings(), &expected);
    app.flush_persistence();
    app.library.shutdown();
}
