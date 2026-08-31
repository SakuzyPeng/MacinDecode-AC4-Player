use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui::{self, Align, Color32, Layout, RichText, Stroke};

use crate::backend::SpatialBackendKind;
use crate::bitstream_ui::{self, BitstreamAction};
use crate::decoder::{
    DecodeMetrics, DecodePhase, DecoderController, DecoderSnapshot, PREBUFFER_MILLISECONDS,
};
use crate::inspection::InspectionController;
use crate::model::SelectedSource;
use crate::theme;

pub struct PlayerApp {
    playlist: Vec<SelectedSource>,
    selected_source: Option<usize>,
    inspection: InspectionController,
    decoder: DecoderController,
    decoder_revision: u64,
    backend: SpatialBackendKind,
    status: StatusLine,
    timeline_preview: f32,
    volume: f32,
    muted: bool,
    bitstream_details_open: bool,
    diagnostics_open: bool,
}

impl PlayerApp {
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        theme::install(&creation_context.egui_ctx);
        Self {
            playlist: Vec::new(),
            selected_source: None,
            inspection: InspectionController::new(),
            decoder: DecoderController::new(),
            decoder_revision: 0,
            backend: SpatialBackendKind::Automatic,
            status: StatusLine::idle("Add or drop AC-4 media files"),
            timeline_preview: 0.0,
            volume: 0.8,
            muted: false,
            bitstream_details_open: false,
            diagnostics_open: false,
        }
    }

    fn choose_sources(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .set_title("Add AC-4 media to playlist")
            .add_filter("AC-4 media", &["m4a", "mp4", "ac4"])
            .pick_files()
        {
            self.append_sources(paths);
        }
    }

    fn append_sources(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        let mut added = 0;
        let mut duplicates = 0;
        let mut rejected = 0;

        for path in paths {
            match SelectedSource::from_path(path) {
                Ok(source) => {
                    if self
                        .playlist
                        .iter()
                        .any(|item| item.path() == source.path())
                    {
                        duplicates += 1;
                    } else {
                        self.playlist.push(source);
                        added += 1;
                    }
                }
                Err(_) => rejected += 1,
            }
        }

        if self.selected_source.is_none() && !self.playlist.is_empty() {
            self.selected_source = Some(0);
        }

        if added > 0 {
            let noun = if added == 1 { "file" } else { "files" };
            self.status = StatusLine::ready(format!("Added {added} {noun} to the playlist"));
        } else if duplicates > 0 && rejected == 0 {
            self.status = StatusLine::idle("Selected files are already in the playlist");
        } else if rejected > 0 {
            self.status = StatusLine::warning("No supported AC-4 media was added");
        }
    }

    fn select_source(&mut self, index: usize) {
        if let Some(source) = self.playlist.get(index) {
            let name = source.display_name().to_owned();
            self.selected_source = Some(index);
            self.timeline_preview = 0.0;
            self.status = StatusLine::ready(format!("Selected {name}; opening MacinDecode Core"));
        }
    }

    fn remove_selected_source(&mut self) {
        let Some(index) = self.selected_source else {
            return;
        };
        let removed = self.playlist.remove(index);

        if self.playlist.is_empty() {
            self.selected_source = None;
            self.status = StatusLine::idle("Add or drop AC-4 media files");
        } else {
            self.selected_source = Some(index.min(self.playlist.len() - 1));
            self.status = StatusLine::idle(format!(
                "Removed {} from the playlist",
                removed.display_name()
            ));
        }
        self.timeline_preview = 0.0;
        self.inspection
            .retain_paths(self.playlist.iter().map(SelectedSource::path));
        if self.selected_source.is_none() {
            self.bitstream_details_open = false;
        }
    }

    fn has_selected_source(&self) -> bool {
        self.selected_source
            .is_some_and(|index| index < self.playlist.len())
    }

    fn selected_source(&self) -> Option<&SelectedSource> {
        self.selected_source
            .and_then(|index| self.playlist.get(index))
    }

    fn selected_path(&self) -> Option<&Path> {
        self.selected_source().map(SelectedSource::path)
    }

    fn sync_inspection(&mut self, context: &egui::Context) {
        self.inspection.poll();
        if let Some(path) = self.selected_path().map(Path::to_path_buf) {
            self.inspection.ensure_requested(&path);
        } else {
            self.bitstream_details_open = false;
        }
        if self.inspection.has_pending() {
            context.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn sync_decoder(&mut self, context: &egui::Context) {
        if let Some(path) = self.selected_path().map(Path::to_path_buf) {
            self.decoder.ensure_open(&path);
        } else {
            self.decoder.close();
        }
        self.decoder.poll();
        if self.decoder_revision != self.decoder.revision() {
            self.decoder_revision = self.decoder.revision();
            self.status = decoder_status_line(self.decoder.snapshot());
        }
        if self.decoder.is_working() {
            context.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn handle_bitstream_action(&mut self, action: Option<BitstreamAction>) {
        match action {
            Some(BitstreamAction::OpenDetails) => self.bitstream_details_open = true,
            Some(BitstreamAction::Retry) => {
                if let Some(path) = self.selected_path().map(Path::to_path_buf) {
                    self.inspection.retry(&path);
                }
            }
            None => {}
        }
    }

    fn accept_dropped_files(&mut self, context: &egui::Context) {
        let paths = context.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| {
                    let path = file.path();
                    (!path.as_os_str().is_empty()).then(|| path.to_path_buf())
                })
                .collect::<Vec<_>>()
        });
        if !paths.is_empty() {
            self.append_sources(paths);
        }
    }

    fn draw_header(root: &mut egui::Ui) {
        egui::Panel::top("header")
            .exact_size(72.0)
            .frame(
                egui::Frame::NONE
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(20, 0)),
            )
            .show(root, |ui| {
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("MacinDecode AC-4 Player")
                                .size(21.0)
                                .strong()
                                .color(theme::TEXT),
                        );
                        ui.label(
                            RichText::new("Native spatial playback shell")
                                .size(12.0)
                                .color(theme::MUTED),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("output-device")
                            .selected_text("No output device")
                            .width(170.0)
                            .show_ui(ui, |ui| {
                                ui.add_enabled(
                                    false,
                                    egui::Label::new("No spatial devices available"),
                                );
                            });
                        ui.label(
                            RichText::new("OUTPUT DEVICE")
                                .size(10.0)
                                .strong()
                                .color(theme::MUTED),
                        );
                    });
                });
                let clip = ui.clip_rect();
                let bottom = ui.max_rect().bottom() - 0.5;
                ui.painter().line_segment(
                    [
                        egui::pos2(clip.left(), bottom),
                        egui::pos2(clip.right(), bottom),
                    ],
                    Stroke::new(1.0, theme::BORDER),
                );
            });
    }

    fn draw_source_sidebar(&mut self, root: &mut egui::Ui) {
        egui::Panel::left("source-sidebar")
            .exact_size(310.0)
            .resizable(false)
            .frame(
                egui::Frame::NONE
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::same(18)),
            )
            .show(root, |ui| {
                const INFO_BLOCK_HEIGHT: f32 = 205.0;
                const BLOCK_GAP: f32 = 18.0;

                let available = ui.available_rect_before_wrap();
                let info_rect = egui::Rect::from_min_max(
                    egui::pos2(available.left(), available.bottom() - INFO_BLOCK_HEIGHT),
                    available.right_bottom(),
                );
                let source_rect = egui::Rect::from_min_max(
                    available.min,
                    egui::pos2(available.right(), info_rect.top() - BLOCK_GAP),
                );

                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(source_rect)
                        .layout(Layout::top_down(Align::Min)),
                    |ui| {
                        section_title(ui, "SOURCE");
                        let playlist_height = (ui.available_height() - 124.0).max(72.0);
                        card(ui, |ui| self.draw_source_card(ui, playlist_height));
                    },
                );
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(info_rect)
                        .layout(Layout::top_down(Align::Min)),
                    |ui| self.draw_bitstream_info(ui),
                );
            });
    }

    fn draw_bitstream_info(&mut self, ui: &mut egui::Ui) {
        section_title(ui, "BITSTREAM INFO");
        let source = self.selected_source();
        let state = source.and_then(|source| self.inspection.state(source.path()));
        let action = card(ui, |ui| bitstream_ui::draw_card(ui, source, state));
        self.handle_bitstream_action(action);
    }

    fn draw_source_card(&mut self, ui: &mut egui::Ui, playlist_height: f32) {
        playlist_header(ui, self.playlist.len());
        ui.separator();

        let content_width = ui.available_width();
        let action_height = 36.0;
        let action_gap = 11.0;
        let (body_rect, _) = ui.allocate_exact_size(
            egui::vec2(content_width, playlist_height + action_gap + action_height),
            egui::Sense::hover(),
        );
        let actions_rect = egui::Rect::from_min_max(
            egui::pos2(body_rect.left(), body_rect.bottom() - action_height),
            body_rect.right_bottom(),
        );
        let list_rect = egui::Rect::from_min_max(
            body_rect.min,
            egui::pos2(body_rect.right(), actions_rect.top() - action_gap),
        );
        if let Some(index) = playlist_contents(ui, list_rect, &self.playlist, self.selected_source)
        {
            self.select_source(index);
        }

        match playlist_actions(ui, actions_rect, self.has_selected_source()) {
            Some(PlaylistAction::Add) => self.choose_sources(),
            Some(PlaylistAction::Remove) => self.remove_selected_source(),
            None => {}
        }
    }

    fn draw_scene(&mut self, root: &mut egui::Ui) {
        let decoder = self.decoder.snapshot().clone();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::same(20)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(RichText::new("Object scene").color(theme::TEXT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_sized([36.0, 30.0], egui::Button::new("..."))
                            .on_hover_text("Open diagnostics")
                            .clicked()
                        {
                            self.diagnostics_open = true;
                        }
                        ui.add_space(4.0);
                        let format = decoder.metrics().map_or_else(
                            || "48 kHz · planar f32".to_owned(),
                            |metrics| format!("{} Hz · planar f32", metrics.sample_rate()),
                        );
                        ui.label(RichText::new(format).size(11.0).color(theme::MUTED));
                    });
                });
                ui.add_space(10.0);

                metric_strip(ui, &decoder);

                ui.add_space(16.0);
                scene_placeholder(ui, self.has_selected_source(), &decoder);
            });
    }

    fn draw_diagnostics_window(&mut self, context: &egui::Context) {
        if !self.diagnostics_open {
            return;
        }

        let decoder = self.decoder.snapshot().clone();
        let remains_open = context.show_viewport_immediate(
            egui::ViewportId::from_hash_of("playback-diagnostics"),
            egui::ViewportBuilder::default()
                .with_title("MacinDecode AC-4 Diagnostics")
                .with_inner_size([460.0, 390.0])
                .with_min_inner_size([400.0, 300.0]),
            |root, _class| {
                let close_requested = root.ctx().input(|input| input.viewport().close_requested());
                draw_diagnostics_content(root, self.backend, &decoder);
                !close_requested
            },
        );
        self.diagnostics_open = remains_open;
    }

    fn draw_bitstream_details_window(&mut self, context: &egui::Context) {
        if !self.bitstream_details_open {
            return;
        }
        let Some(source) = self.selected_source() else {
            self.bitstream_details_open = false;
            return;
        };
        let state = self.inspection.state(source.path());
        let mut requested_action = None;
        let remains_open = context.show_viewport_immediate(
            egui::ViewportId::from_hash_of("bitstream-details"),
            egui::ViewportBuilder::default()
                .with_title("MacinDecode AC-4 Bitstream Details")
                .with_inner_size([760.0, 680.0])
                .with_min_inner_size([560.0, 420.0]),
            |root, _class| {
                let close_requested = root.ctx().input(|input| input.viewport().close_requested());
                requested_action = bitstream_ui::draw_details(root, source, state);
                !close_requested
            },
        );
        self.bitstream_details_open = remains_open;
        self.handle_bitstream_action(requested_action);
    }

    fn draw_transport(&mut self, root: &mut egui::Ui) {
        egui::Panel::bottom("transport")
            .exact_size(136.0)
            .frame(
                egui::Frame::NONE
                    .fill(theme::SURFACE)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::symmetric(22, 14)),
            )
            .show(root, |ui| {
                let (content, _) =
                    ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
                let status_rect =
                    egui::Rect::from_min_size(content.min, egui::vec2(content.width(), 20.0));
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(status_rect)
                        .layout(Layout::left_to_right(Align::Center)),
                    |ui| {
                        ui.colored_label(self.status.color(), "●");
                        ui.label(RichText::new(&self.status.text).color(theme::MUTED));
                    },
                );

                let control_height = 34.0 + ui.spacing().item_spacing.y + 20.0;
                let control_rect = egui::Rect::from_center_size(
                    content.center(),
                    egui::vec2(content.width(), control_height),
                );
                let volume_width = if content.width() >= 700.0 {
                    150.0
                } else {
                    120.0
                };
                let side_reserve = volume_width + 20.0;
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(control_rect)
                        .layout(Layout::top_down(Align::Center)),
                    |ui| {
                        transport_buttons(ui);
                        transport_progress(ui, &mut self.timeline_preview, side_reserve);
                    },
                );

                let volume_rect = egui::Rect::from_center_size(
                    egui::pos2(
                        content.right() - volume_width / 2.0,
                        control_rect.bottom() - 10.0,
                    ),
                    egui::vec2(volume_width, 28.0),
                );
                volume_control(ui, volume_rect, &mut self.volume, &mut self.muted);
            });
    }
}

enum PlaylistAction {
    Add,
    Remove,
}

fn playlist_header(ui: &mut egui::Ui, item_count: usize) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("PLAYLIST")
                .size(10.0)
                .strong()
                .color(theme::MUTED),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let noun = if item_count == 1 { "ITEM" } else { "ITEMS" };
            ui.label(
                RichText::new(format!("{item_count} {noun}"))
                    .size(10.0)
                    .color(theme::MUTED),
            );
        });
    });
}

fn playlist_contents(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    playlist: &[SelectedSource],
    selected_source: Option<usize>,
) -> Option<usize> {
    let mut requested_selection = None;
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::top_down(Align::Min)),
        |ui| {
            if playlist.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(((rect.height() - 48.0) / 2.0).max(16.0));
                    ui.label(RichText::new("Playlist is empty").color(theme::TEXT));
                    ui.label(
                        RichText::new("Drop .m4a, .mp4, or .ac4 files here")
                            .size(11.0)
                            .color(theme::MUTED),
                    );
                });
                return;
            }

            egui::ScrollArea::vertical()
                .id_salt("source-playlist")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (index, source) in playlist.iter().enumerate() {
                        let label = format!("{:02}  {}", index + 1, source.display_name());
                        let selected = selected_source == Some(index);
                        if ui
                            .add_sized(
                                [ui.available_width(), 34.0],
                                egui::Button::selectable(selected, RichText::new(label).size(12.0))
                                    .truncate(),
                            )
                            .on_hover_text(source.path().display().to_string())
                            .clicked()
                        {
                            requested_selection = Some(index);
                        }
                    }
                });
        },
    );
    requested_selection
}

fn playlist_actions(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    can_remove: bool,
) -> Option<PlaylistAction> {
    let remove_width = 76.0;
    let add_width = (rect.width() - remove_width - ui.spacing().item_spacing.x).max(100.0);
    let mut action = None;
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            if ui
                .add_sized(
                    [add_width, rect.height()],
                    egui::Button::new(RichText::new("Add files").strong().color(Color32::WHITE))
                        .fill(theme::ACCENT)
                        .stroke(Stroke::NONE),
                )
                .clicked()
            {
                action = Some(PlaylistAction::Add);
            }
            if ui
                .add_enabled(
                    can_remove,
                    egui::Button::new("Remove").min_size(egui::vec2(remove_width, rect.height())),
                )
                .clicked()
            {
                action = Some(PlaylistAction::Remove);
            }
        },
    );
    action
}

fn transport_buttons(ui: &mut egui::Ui) {
    let button_size = egui::vec2(44.0, 34.0);
    let group_width = button_size.x * 3.0 + ui.spacing().item_spacing.x * 2.0;
    let (row, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), button_size.y),
        egui::Sense::hover(),
    );
    let group = egui::Rect::from_center_size(row.center(), egui::vec2(group_width, button_size.y));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(group)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            disabled_transport_button(ui, "◀◀", button_size);
            disabled_transport_button(ui, "▶", button_size);
            disabled_transport_button(ui, "■", button_size);
        },
    );
}

fn disabled_transport_button(ui: &mut egui::Ui, glyph: &str, size: egui::Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::HOVER);
    ui.painter().rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, theme::BORDER),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(15.0),
        theme::MUTED,
    );
}

fn transport_progress(ui: &mut egui::Ui, timeline_preview: &mut f32, side_reserve: f32) {
    let row_width = ui.available_width();
    let time_width = 50.0;
    let spacing = ui.spacing().item_spacing.x;
    let max_group_width = (row_width - side_reserve * 2.0).max(260.0);
    let max_progress_width = (max_group_width - time_width * 2.0 - spacing * 2.0).max(140.0);
    let progress_width = (row_width * 0.45)
        .clamp(160.0, 420.0)
        .min(max_progress_width);
    let group_width = progress_width + time_width * 2.0 + ui.spacing().item_spacing.x * 2.0;
    let (row, _) = ui.allocate_exact_size(egui::vec2(row_width, 20.0), egui::Sense::hover());
    let group = egui::Rect::from_center_size(row.center(), egui::vec2(group_width, row.height()));

    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(group)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.add_sized(
                [time_width, 18.0],
                egui::Label::new(RichText::new("00:00").monospace().color(theme::MUTED))
                    .halign(Align::RIGHT),
            );
            ui.add_enabled_ui(false, |ui| {
                ui.spacing_mut().interact_size.y = 18.0;
                ui.spacing_mut().slider_width = progress_width;
                ui.add(egui::Slider::new(timeline_preview, 0.0..=1.0).show_value(false));
            });
            ui.add_sized(
                [time_width, 18.0],
                egui::Label::new(RichText::new("--:--").monospace().color(theme::MUTED))
                    .halign(Align::LEFT),
            );
        },
    );
}

fn volume_control(ui: &mut egui::Ui, rect: egui::Rect, volume: &mut f32, muted: &mut bool) {
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            if speaker_button(ui, *muted, *volume).clicked() {
                if *muted {
                    if *volume <= f32::EPSILON {
                        *volume = 0.5;
                    }
                    *muted = false;
                } else {
                    *muted = true;
                }
            }

            let slider_width = (rect.width() - 28.0 - ui.spacing().item_spacing.x).max(56.0);
            ui.spacing_mut().interact_size.y = 18.0;
            ui.spacing_mut().slider_width = slider_width;
            let response = ui.add(egui::Slider::new(volume, 0.0..=1.0).show_value(false));
            if response.changed() {
                *muted = *volume <= f32::EPSILON;
            }
            response.on_hover_text(format!("Volume: {:.0}%", *volume * 100.0));
        },
    );
}

fn speaker_button(ui: &mut egui::Ui, muted: bool, volume: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
    let response = response.on_hover_text(if muted { "Unmute" } else { "Mute" });
    let fill = if response.hovered() {
        theme::HOVER
    } else {
        theme::SURFACE
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    ui.painter().rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, theme::BORDER),
        egui::StrokeKind::Inside,
    );

    let center = rect.center();
    let color = if muted { theme::WARNING } else { theme::MUTED };
    ui.painter().rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(center.x - 9.0, center.y - 3.0),
            egui::pos2(center.x - 5.0, center.y + 3.0),
        ),
        0.0,
        color,
    );
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(center.x - 5.0, center.y - 4.0),
            egui::pos2(center.x + 1.0, center.y - 8.0),
            egui::pos2(center.x + 1.0, center.y + 8.0),
            egui::pos2(center.x - 5.0, center.y + 4.0),
        ],
        color,
        Stroke::NONE,
    ));

    if muted {
        ui.painter().line_segment(
            [
                egui::pos2(center.x + 5.0, center.y - 4.0),
                egui::pos2(center.x + 11.0, center.y + 4.0),
            ],
            Stroke::new(1.5, color),
        );
        ui.painter().line_segment(
            [
                egui::pos2(center.x + 11.0, center.y - 4.0),
                egui::pos2(center.x + 5.0, center.y + 4.0),
            ],
            Stroke::new(1.5, color),
        );
    } else if volume > f32::EPSILON {
        ui.painter().line_segment(
            [
                egui::pos2(center.x + 5.0, center.y - 4.0),
                egui::pos2(center.x + 8.0, center.y),
            ],
            Stroke::new(1.5, color),
        );
        ui.painter().line_segment(
            [
                egui::pos2(center.x + 8.0, center.y),
                egui::pos2(center.x + 5.0, center.y + 4.0),
            ],
            Stroke::new(1.5, color),
        );
        if volume > 0.5 {
            ui.painter().line_segment(
                [
                    egui::pos2(center.x + 8.0, center.y - 6.0),
                    egui::pos2(center.x + 12.0, center.y),
                ],
                Stroke::new(1.5, color),
            );
            ui.painter().line_segment(
                [
                    egui::pos2(center.x + 12.0, center.y),
                    egui::pos2(center.x + 8.0, center.y + 6.0),
                ],
                Stroke::new(1.5, color),
            );
        }
    }

    response
}

impl eframe::App for PlayerApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = root.ctx().clone();
        self.accept_dropped_files(&context);
        self.sync_inspection(&context);
        self.sync_decoder(&context);
        Self::draw_header(root);
        self.draw_source_sidebar(root);
        self.draw_transport(root);
        self.draw_scene(root);
        self.draw_bitstream_details_window(&context);
        self.draw_diagnostics_window(&context);

        if context.input(|input| !input.raw.hovered_files.is_empty()) {
            draw_drop_overlay(&context);
        }
    }
}

struct StatusLine {
    kind: StatusKind,
    text: String,
}

impl StatusLine {
    fn idle(text: impl Into<String>) -> Self {
        Self {
            kind: StatusKind::Idle,
            text: text.into(),
        }
    }

    fn ready(text: impl Into<String>) -> Self {
        Self {
            kind: StatusKind::Ready,
            text: text.into(),
        }
    }

    fn warning(text: impl Into<String>) -> Self {
        Self {
            kind: StatusKind::Warning,
            text: text.into(),
        }
    }

    const fn color(&self) -> Color32 {
        match self.kind {
            StatusKind::Idle => theme::MUTED,
            StatusKind::Ready => theme::SUCCESS,
            StatusKind::Warning => theme::WARNING,
        }
    }
}

enum StatusKind {
    Idle,
    Ready,
    Warning,
}

fn decoder_status_line(decoder: &DecoderSnapshot) -> StatusLine {
    let source = decoder
        .path()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("AC-4 source");
    match decoder.phase() {
        DecodePhase::Unavailable => StatusLine::idle(
            decoder
                .detail()
                .unwrap_or("The Windows decode worker is unavailable"),
        ),
        DecodePhase::Idle => StatusLine::idle("Add or select an AC-4 media file"),
        DecodePhase::Opening => StatusLine::idle(format!("Opening {source} with MacinDecode Core")),
        DecodePhase::Buffering => {
            let buffered = decoder
                .metrics()
                .map_or(0, DecodeMetrics::buffered_milliseconds);
            StatusLine::idle(format!(
                "MacinDecode Core buffered {buffered} / {PREBUFFER_MILLISECONDS} ms"
            ))
        }
        DecodePhase::Ready => {
            let metrics = decoder.metrics().expect("ready decode state has metrics");
            StatusLine::ready(format!(
                "MacinDecode Core ready: {} objects + {} LFE, {} ms buffered",
                metrics.object_count(),
                u8::from(metrics.has_lfe()),
                metrics.buffered_milliseconds()
            ))
        }
        DecodePhase::EndOfStream => {
            let metrics = decoder
                .metrics()
                .expect("end-of-stream decode state has metrics");
            StatusLine::ready(format!(
                "Decoded {source} to end: {} AUs, {} ms of scene PCM",
                metrics.decoded_access_units(),
                metrics.decoded_milliseconds()
            ))
        }
        DecodePhase::Failed => StatusLine::warning(format!(
            "MacinDecode Core failed for {source}: {}",
            decoder.detail().unwrap_or("unknown decode error")
        )),
    }
}

const fn decode_phase_label(phase: DecodePhase) -> &'static str {
    match phase {
        DecodePhase::Unavailable => "Unavailable",
        DecodePhase::Idle => "Idle",
        DecodePhase::Opening => "Opening",
        DecodePhase::Buffering => "Buffering",
        DecodePhase::Ready => "Ready",
        DecodePhase::EndOfStream => "End of stream",
        DecodePhase::Failed => "Failed",
    }
}

fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).size(11.0).strong().color(theme::MUTED));
    ui.add_space(4.0);
}

fn card<R>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::NONE
        .fill(theme::SURFACE)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .inner_margin(egui::Margin::same(14))
        .show(ui, contents)
        .inner
}

fn key_value(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).size(12.0).color(theme::MUTED));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).size(12.0).color(theme::TEXT));
        });
    });
}

fn decode_metric_values(decoder: &DecoderSnapshot) -> [(&'static str, String, String); 4] {
    let metrics = decoder.metrics();
    let phase = decode_phase_label(decoder.phase());
    [
        (
            "OBJECTS",
            metrics.map_or_else(|| "—".to_owned(), |value| value.object_count().to_string()),
            metrics.map_or_else(
                || phase.to_owned(),
                |value| format!("presentation {}", value.presentation_index()),
            ),
        ),
        (
            "LFE",
            metrics.map_or_else(
                || "—".to_owned(),
                |value| if value.has_lfe() { "1" } else { "0" }.to_owned(),
            ),
            "Native bed component".to_owned(),
        ),
        (
            "POSITION",
            metrics.map_or_else(
                || "—".to_owned(),
                |value| {
                    if value.state_complete() {
                        "READY"
                    } else {
                        "WAIT"
                    }
                    .to_owned()
                },
            ),
            metrics.map_or_else(
                || "OAMD pending".to_owned(),
                |value| format!("{} in-frame updates", value.metadata_updates()),
            ),
        ),
        (
            "BUFFER",
            metrics.map_or_else(
                || "—".to_owned(),
                |value| format!("{} ms", value.buffered_milliseconds()),
            ),
            metrics.map_or_else(
                || "Scene FIFO offline".to_owned(),
                |value| {
                    format!(
                        "{} / {} frames",
                        value.buffered_frames(),
                        value.buffer_capacity_frames()
                    )
                },
            ),
        ),
    ]
}

fn metric_strip(ui: &mut egui::Ui, decoder: &DecoderSnapshot) {
    let values = decode_metric_values(decoder);

    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 96.0), egui::Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, theme::SURFACE);
    let cell_width = rect.width() / 4.0;
    let first_cell = egui::Rect::from_min_size(rect.min, egui::vec2(cell_width, rect.height()));
    painter.rect_filled(first_cell, 0.0, theme::STAGE);
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, theme::BORDER),
        egui::StrokeKind::Inside,
    );

    let mut left = rect.left();
    let value_count = values.len();
    for (index, (title, value, detail)) in values.into_iter().enumerate() {
        let right = if index + 1 == value_count {
            rect.right()
        } else {
            left + cell_width
        };
        if index > 0 {
            painter.line_segment(
                [
                    egui::pos2(left, rect.top()),
                    egui::pos2(left, rect.bottom()),
                ],
                Stroke::new(1.0, theme::BORDER),
            );
        }
        let cell = egui::Rect::from_min_max(
            egui::pos2(left + 16.0, rect.top() + 12.0),
            egui::pos2(right - 12.0, rect.bottom() - 10.0),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(cell)
                .layout(Layout::top_down(Align::Min)),
            |ui| {
                ui.label(RichText::new(title).size(10.0).strong().color(theme::MUTED));
                ui.label(RichText::new(&value).size(21.0).strong().color(theme::TEXT));
                ui.label(RichText::new(&detail).size(10.0).color(theme::MUTED));
            },
        );
        left = right;
    }
}

fn scene_placeholder(ui: &mut egui::Ui, has_source: bool, decoder: &DecoderSnapshot) {
    let available_height = ui.available_height();
    let frame = egui::Frame::NONE
        .fill(theme::STAGE)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .inner_margin(egui::Margin::same(18));
    let margins = frame.total_margin();
    let content_height = (available_height - margins.top - margins.bottom).max(180.0);

    frame.show(ui, |ui| {
        ui.set_min_height(content_height);
        ui.vertical_centered(|ui| {
            ui.add_space(((content_height - 62.0) / 2.0).max(24.0));
            let (headline, detail) = match decoder.phase() {
                DecodePhase::Opening => (
                    "Opening AC-4 source",
                    "Reading the bounded access-unit timeline for MacinDecode Core.",
                ),
                DecodePhase::Buffering => (
                    "Decoding object scene",
                    "Core is producing normalized object/LFE PCM into the bounded Scene FIFO.",
                ),
                DecodePhase::Ready | DecodePhase::EndOfStream => (
                    "Decoded scene buffered",
                    "Full A-JOC PCM and OAMD are ready; Windows spatial submission is the next layer.",
                ),
                DecodePhase::Failed => (
                    "Scene decode failed",
                    decoder.detail().unwrap_or("MacinDecode Core reported an error."),
                ),
                DecodePhase::Unavailable => (
                    "Windows decoder path",
                    decoder
                        .detail()
                        .unwrap_or("Audio decoding is unavailable on this platform."),
                ),
                DecodePhase::Idle if has_source => (
                    "Waiting for decoder",
                    "The selected source has not entered the decode worker yet.",
                ),
                DecodePhase::Idle => ("No object scene", "Add an AC-4 source to begin inspection."),
            };
            ui.label(
                RichText::new(headline)
                    .size(19.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(detail).color(theme::MUTED),
            );
        });
    });
}

fn draw_diagnostics_content(
    root: &mut egui::Ui,
    backend: SpatialBackendKind,
    decoder: &DecoderSnapshot,
) {
    egui::CentralPanel::default()
        .frame(
            egui::Frame::NONE
                .fill(theme::BACKGROUND)
                .inner_margin(egui::Margin::same(22)),
        )
        .show(root, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.heading(RichText::new("Playback diagnostics").color(theme::TEXT));
                    ui.label(
                        RichText::new(
                            "Live status for the decoder and native spatial output path.",
                        )
                        .size(11.0)
                        .color(theme::MUTED),
                    );
                    ui.add_space(16.0);
                    section_title(ui, "SESSION");
                    card(ui, |ui| {
                        let metrics = decoder.metrics();
                        key_value(
                            ui,
                            "Container",
                            metrics.map_or("Not connected", |value| value.container().label()),
                        );
                        ui.separator();
                        key_value(ui, "Decoder session", decode_phase_label(decoder.phase()));
                        ui.separator();
                        key_value(
                            ui,
                            "Scene elements",
                            &metrics.map_or_else(
                                || "—".to_owned(),
                                |value| {
                                    format!(
                                        "{} objects + {} LFE",
                                        value.object_count(),
                                        u8::from(value.has_lfe())
                                    )
                                },
                            ),
                        );
                        ui.separator();
                        key_value(
                            ui,
                            "Decoded AUs / frames",
                            &metrics.map_or_else(
                                || "0 / 0".to_owned(),
                                |value| {
                                    format!(
                                        "{} / {}",
                                        value.decoded_access_units(),
                                        value.decoded_scene_frames()
                                    )
                                },
                            ),
                        );
                        ui.separator();
                        key_value(
                            ui,
                            "Scene buffer",
                            &metrics.map_or_else(
                                || "0 ms".to_owned(),
                                |value| format!("{} ms", value.buffered_milliseconds()),
                            ),
                        );
                        ui.separator();
                        key_value(ui, "Backend policy", backend.label());
                        ui.separator();
                        key_value(
                            ui,
                            "Native adapters",
                            &format!("{} planned", SpatialBackendKind::ALL.len() - 1),
                        );
                        ui.separator();
                        key_value(ui, "Spatial stream", "Not created");
                        ui.separator();
                        key_value(ui, "Underruns", "0");
                    });
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(format!(
                            "{} The decoder is connected independently of the output device.",
                            backend.availability()
                        ))
                        .size(10.0)
                        .color(theme::MUTED),
                    );
                });
        });
}

fn draw_drop_overlay(context: &egui::Context) {
    let painter = context.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("drop-overlay"),
    ));
    let rect = context.content_rect().shrink(24.0);
    painter.rect_filled(
        rect,
        0.0,
        Color32::from_rgba_unmultiplied(255, 253, 247, 244),
    );
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(2.0, theme::ACCENT),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "Drop AC-4 media files",
        egui::FontId::proportional(22.0),
        theme::TEXT,
    );
}
