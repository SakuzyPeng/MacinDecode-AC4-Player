//! Playlist widgets emit commands; they never open media or write storage.
use crate::library::Mutation;
use crate::playlist::{
    BrowseState, EntryId, MediaId, PlaybackCursor, PlaybackMode, PlaylistId, PlaylistSnapshot,
    PlaylistSummary,
};
use crate::theme;
use eframe::egui::{self, RichText};
use std::collections::HashSet;

pub enum Action {
    Switch(PlaylistId),
    Add,
    Play(EntryId),
    Remove,
    Retry(EntryId),
    Relocate(MediaId),
    MoveSelection(bool),
    Drop {
        entries: HashSet<EntryId>,
        gap: usize,
    },
    Transfer {
        to: PlaylistId,
        remove: bool,
    },
    Mutate(Mutation),
}
#[derive(Default)]
pub struct State {
    pub manage_open: bool,
    name: String,
    editing: Option<PlaylistId>,
    deleting: Option<PlaylistId>,
}
#[derive(Clone)]
struct DragEntries {
    playlist: PlaylistId,
    entries: HashSet<EntryId>,
}

pub fn header(
    ui: &mut egui::Ui,
    summaries: &[PlaylistSummary],
    selected: Option<PlaylistId>,
    state: &mut State,
) -> Option<Action> {
    let mut action = None;
    ui.horizontal(|ui| {
        let title = summaries
            .iter()
            .find(|p| Some(p.id) == selected)
            .map_or_else(
                || "Loading…".into(),
                |p| format!("{} · {}", p.name, p.count),
            );
        let selector_width = (ui.available_width() - 100.0).max(80.0);
        ui.allocate_ui(
            egui::vec2(selector_width, ui.spacing().interact_size.y),
            |ui| {
                egui::ComboBox::from_id_salt("playlist-selector")
                    .width(selector_width)
                    .truncate()
                    .selected_text(title)
                    .show_ui(ui, |ui| {
                        for p in summaries {
                            if ui
                                .selectable_label(
                                    Some(p.id) == selected,
                                    format!("{} · {}", p.name, p.count),
                                )
                                .clicked()
                            {
                                action = Some(Action::Switch(p.id));
                            }
                        }
                    });
            },
        );
        if ui.button("+").on_hover_text("New playlist").clicked() {
            state.manage_open = true;
            state.editing = None;
            state.name = "New playlist".into();
        }
        if ui.button("⋯").on_hover_text("Manage playlists").clicked() {
            state.manage_open = true;
        }
    });
    action
}

#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "virtual rows use egui's bounded pixel scroll coordinates and keep row interaction together"
)]
pub fn contents(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    list: &PlaylistSnapshot,
    browse: &mut BrowseState,
    cursor: Option<&PlaybackCursor>,
    media_errors: &std::collections::HashMap<MediaId, Option<String>>,
) -> Vec<Action> {
    let mut actions = Vec::new();
    // Buttons consume Enter during interaction. Capture it before drawing the
    // rows, and retain keyboard focus independently of pointer hover.
    let enter_pressed = ui.input(|input| input.key_pressed(egui::Key::Enter));
    let keyboard_allowed = !egui::Popup::is_any_open(ui.ctx());
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(rect.intersect(ui.clip_rect()));
            if rect.height() < 1.0 {
                return;
            }
            if list.entries.is_empty() {
                ui.label("Playlist is empty");
                ui.label(RichText::new("Add or drop AC-4 media files").color(theme::MUTED));
                return;
            }
            let stride = 34.0 + ui.spacing().item_spacing.y;
            let mut scroll = egui::ScrollArea::vertical()
                .id_salt(("playlist-rows", list.summary.id.0))
                .max_height(rect.height())
                .auto_shrink([false, false]);
            if browse.restore_scroll {
                let index = browse
                    .saved
                    .scroll_entry
                    .and_then(|id| list.positions.get(&id))
                    .copied()
                    .unwrap_or(0);
                scroll = scroll.vertical_scroll_offset(
                    index as f32 * stride + browse.saved.scroll_offset.clamp(0.0, stride),
                );
                browse.restore_scroll = false;
            }
            let mut row_focused = false;
            let output = scroll.show_rows(ui, 34.0, list.entries.len(), |ui, range| {
                for index in range {
                    let entry = &list.entries[index];
                    let error = media_errors
                        .get(&entry.media)
                        .map_or(entry.error.as_ref(), Option::as_ref);
                    let playing = cursor.is_some_and(|c| {
                        c.attached && c.playlist == Some(list.summary.id) && c.entry.id == entry.id
                    });
                    let marker = if playing {
                        "▶ "
                    } else if error.is_some() {
                        "! "
                    } else {
                        ""
                    };
                    let response = ui
                        .push_id(entry.id.0, |ui| {
                            ui.add_sized(
                                [ui.available_width(), 34.0],
                                egui::Button::selectable(browse.selected.contains(&entry.id), ())
                                    .left_text(
                                        RichText::new(format!(
                                            "{marker}{:02}  {}",
                                            index + 1,
                                            entry.source.display_name()
                                        ))
                                        .size(12.0)
                                        .color(
                                            if error.is_some() {
                                                theme::WARNING
                                            } else {
                                                theme::TEXT
                                            },
                                        ),
                                    )
                                    .truncate()
                                    .sense(egui::Sense::click_and_drag()),
                            )
                        })
                        .inner
                        .on_hover_text(error.map_or_else(
                            || entry.source.path().display().to_string(),
                            |e| format!("{}\n{e}", entry.source.path().display()),
                        ));
                    row_focused |= response.has_focus();
                    if response.clicked() {
                        response.request_focus();
                        if !enter_pressed {
                            let modifiers = ui.input(|i| i.modifiers);
                            browse.select(list, entry.id, modifiers.command, modifiers.shift);
                        }
                    }
                    if response.double_clicked() {
                        actions.push(Action::Play(entry.id));
                    }
                    if response.secondary_clicked() && !browse.selected.contains(&entry.id) {
                        browse.select(list, entry.id, false, false);
                    }
                    response.context_menu(|ui| {
                        if ui.button("Play").clicked() {
                            actions.push(Action::Play(entry.id));
                            ui.close();
                        }
                        if ui.button("Retry media").clicked() {
                            actions.push(Action::Retry(entry.id));
                            ui.close();
                        }
                        if ui.button("Locate file…").clicked() {
                            actions.push(Action::Relocate(entry.media));
                            ui.close();
                        }
                        ui.separator();
                        for (label, up) in [("Move up", true), ("Move down", false)] {
                            if ui.button(label).clicked() {
                                actions.push(Action::MoveSelection(up));
                                ui.close();
                            }
                        }
                        if ui.button("Remove selected").clicked() {
                            actions.push(Action::Remove);
                            ui.close();
                        }
                    });
                    if response.drag_started() {
                        if !browse.selected.contains(&entry.id) {
                            browse.select(list, entry.id, false, false);
                        }
                        response.dnd_set_drag_payload(DragEntries {
                            playlist: list.summary.id,
                            entries: browse.selected.clone(),
                        });
                    }
                    if let Some(payload) = response.dnd_hover_payload::<DragEntries>()
                        && payload.playlist == list.summary.id
                    {
                        let below = ui
                            .input(|i| i.pointer.hover_pos())
                            .is_some_and(|p| p.y > response.rect.center().y);
                        let gap = index + usize::from(below);
                        let y = if below {
                            response.rect.bottom()
                        } else {
                            response.rect.top()
                        };
                        ui.painter().line_segment(
                            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                            egui::Stroke::new(2.0, theme::ACCENT),
                        );
                        if let Some(payload) = response.dnd_release_payload::<DragEntries>() {
                            actions.push(Action::Drop {
                                entries: payload.entries.clone(),
                                gap,
                            });
                        }
                    }
                }
            });
            let top = (output.state.offset.y / stride).floor().max(0.0) as usize;
            browse.saved.scroll_entry = list.entries.get(top).map(|e| e.id);
            browse.saved.scroll_offset =
                (output.state.offset.y - top as f32 * stride).clamp(0.0, stride);
            if let Some(payload) = ui
                .interact(
                    rect,
                    ui.id().with("playlist-drop-tail"),
                    egui::Sense::hover(),
                )
                .dnd_release_payload::<DragEntries>()
                && payload.playlist == list.summary.id
            {
                actions.push(Action::Drop {
                    entries: payload.entries.clone(),
                    gap: list.entries.len(),
                });
            }
            if keyboard_allowed
                && (row_focused
                    || (ui.rect_contains_pointer(rect) && !ui.ctx().egui_wants_keyboard_input()))
            {
                ui.input(|input| {
                    if input.modifiers.command && input.key_pressed(egui::Key::A) {
                        browse.selected = list.entries.iter().map(|e| e.id).collect();
                    }
                    if enter_pressed && let Some(id) = browse.saved.focus {
                        actions.push(Action::Play(id));
                    }
                    if input.key_pressed(egui::Key::Delete)
                        || input.key_pressed(egui::Key::Backspace)
                    {
                        actions.push(Action::Remove);
                    }
                });
            }
        },
    );
    actions
}

pub fn actions(
    ui: &mut egui::Ui,
    summaries: &[PlaylistSummary],
    browse: &BrowseState,
) -> Vec<Action> {
    let mut actions = Vec::new();
    ui.horizontal(|ui| {
        if ui.button("Add files").clicked() {
            actions.push(Action::Add);
        }
        ui.add_enabled_ui(!browse.selected.is_empty(), |ui| {
            if ui.button("Remove").clicked() {
                actions.push(Action::Remove);
            }
            ui.menu_button("⋯", |ui| {
                for (label, remove) in [("Copy to", false), ("Move to", true)] {
                    ui.menu_button(label, |ui| {
                        for p in summaries.iter().filter(|p| Some(p.id) != browse.playlist) {
                            if ui.button(&p.name).clicked() {
                                actions.push(Action::Transfer { to: p.id, remove });
                                ui.close();
                            }
                        }
                    });
                }
                for (label, up) in [("Move up", true), ("Move down", false)] {
                    if ui.button(label).clicked() {
                        actions.push(Action::MoveSelection(up));
                        ui.close();
                    }
                }
            });
        });
    });
    actions
}

#[allow(
    clippy::too_many_lines,
    reason = "one management window keeps list and confirmation actions together"
)]
pub fn management(
    context: &egui::Context,
    summaries: &[PlaylistSummary],
    state: &mut State,
) -> Vec<Action> {
    let mut actions = Vec::new();
    if !state.manage_open {
        return actions;
    }
    let mut open = true;
    egui::Window::new("Manage playlists")
        .open(&mut open)
        .default_width(460.0)
        .show(context, |ui| {
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    for (index, p) in summaries.iter().enumerate() {
                        ui.push_id(p.id.0, |ui| {
                            ui.horizontal(|ui| {
                                if ui
                                    .selectable_label(
                                        state.editing == Some(p.id),
                                        format!("{} · {}", p.name, p.count),
                                    )
                                    .clicked()
                                {
                                    state.editing = Some(p.id);
                                    state.name.clone_from(&p.name);
                                }
                                for (label, destination) in [
                                    ("↑", index.checked_sub(1)),
                                    ("↓", (index + 1 < summaries.len()).then_some(index + 1)),
                                ] {
                                    if ui
                                        .add_enabled(
                                            destination.is_some(),
                                            egui::Button::new(label),
                                        )
                                        .clicked()
                                        && let Some(destination) = destination
                                    {
                                        let mut ids: Vec<_> =
                                            summaries.iter().map(|p| p.id).collect();
                                        ids.swap(index, destination);
                                        actions.push(Action::Mutate(Mutation::OrderPlaylists(ids)));
                                    }
                                }
                                if ui.button("Delete").clicked() {
                                    state.deleting = Some(p.id);
                                }
                            });
                            let mut mode = p.mode;
                            egui::ComboBox::from_id_salt("playlist-mode")
                                .selected_text(mode.label())
                                .show_ui(ui, |ui| {
                                    for value in PlaybackMode::ALL {
                                        ui.selectable_value(&mut mode, value, value.label());
                                    }
                                });
                            if mode != p.mode {
                                actions.push(Action::Mutate(Mutation::Mode(p.id, mode)));
                            }
                        });
                    }
                });
            ui.separator();
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut state.name);
                if ui
                    .add_enabled(
                        !state.name.trim().is_empty(),
                        egui::Button::new(if state.editing.is_some() {
                            "Rename"
                        } else {
                            "Create"
                        }),
                    )
                    .clicked()
                {
                    actions.push(Action::Mutate(state.editing.map_or_else(
                        || Mutation::Create(state.name.clone()),
                        |id| Mutation::Rename(id, state.name.clone()),
                    )));
                    state.name.clear();
                    state.editing = None;
                }
                if ui.button("New").clicked() {
                    state.editing = None;
                    state.name = "New playlist".into();
                }
            });
            if let Some(id) = state.deleting {
                ui.separator();
                ui.label("Delete this playlist? Media files will be kept.");
                ui.horizontal(|ui| {
                    if ui.button("Delete playlist").clicked() {
                        actions.push(Action::Mutate(Mutation::Delete(id)));
                        state.deleting = None;
                    }
                    if ui.button("Cancel").clicked() {
                        state.deleting = None;
                    }
                });
            }
        });
    state.manage_open = open;
    actions
}
