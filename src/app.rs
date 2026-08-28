use std::path::PathBuf;

use eframe::egui::{self, Align, Color32, Layout, RichText, Stroke};

use crate::backend::SpatialBackendKind;
use crate::model::SelectedSource;
use crate::theme;

pub struct PlayerApp {
    source: Option<SelectedSource>,
    backend: SpatialBackendKind,
    status: StatusLine,
    timeline_preview: f32,
    volume: f32,
    muted: bool,
    diagnostics_open: bool,
}

impl PlayerApp {
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        theme::install(&creation_context.egui_ctx);
        Self {
            source: None,
            backend: SpatialBackendKind::Automatic,
            status: StatusLine::idle("Choose or drop an AC-4 media file"),
            timeline_preview: 0.0,
            volume: 0.8,
            muted: false,
            diagnostics_open: false,
        }
    }

    fn choose_source(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Choose AC-4 media")
            .add_filter("AC-4 media", &["m4a", "mp4", "ac4"])
            .pick_file()
        {
            self.set_source(path);
        }
    }

    fn set_source(&mut self, path: PathBuf) {
        match SelectedSource::from_path(path) {
            Ok(source) => {
                self.status = StatusLine::ready("Source selected; decoder is not connected yet");
                self.source = Some(source);
            }
            Err(error) => {
                self.status = StatusLine::warning(error.to_string());
            }
        }
    }

    fn accept_dropped_files(&mut self, context: &egui::Context) {
        let dropped = context.input(|input| input.raw.dropped_files.clone());
        if let Some(path) = dropped.into_iter().find_map(|file| {
            let path = file.path();
            (!path.as_os_str().is_empty()).then(|| path.to_path_buf())
        }) {
            self.set_source(path);
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
                section_title(ui, "SOURCE");
                card(ui, |ui| self.draw_source_card(ui));

                ui.add_space(18.0);
                section_title(ui, "PRESENTATION");
                card(ui, |ui| {
                    key_value(ui, "Selection", "Automatic");
                    key_value(ui, "Decode mode", "Full A-JOC");
                    key_value(ui, "Status", "Not inspected");
                });

                ui.add_space(18.0);
                section_title(ui, "SPATIAL OUTPUT");
                card(ui, |ui| self.draw_backend_card(ui));
            });
    }

    fn draw_source_card(&mut self, ui: &mut egui::Ui) {
        if let Some(source) = &self.source {
            ui.label(
                RichText::new(source.display_name())
                    .strong()
                    .color(theme::TEXT),
            );
            ui.label(
                RichText::new(source.path().display().to_string())
                    .size(11.0)
                    .color(theme::MUTED),
            );
        } else {
            ui.label(RichText::new("No media selected").color(theme::TEXT));
            ui.label(
                RichText::new("Drop an .m4a, .mp4, or .ac4 file here")
                    .size(12.0)
                    .color(theme::MUTED),
            );
        }
        ui.add_space(8.0);
        if ui
            .add_sized(
                [ui.available_width(), 38.0],
                egui::Button::new(
                    RichText::new("Choose media file")
                        .strong()
                        .color(Color32::WHITE),
                )
                .fill(theme::ACCENT)
                .stroke(Stroke::NONE),
            )
            .clicked()
        {
            self.choose_source();
        }
    }

    fn draw_backend_card(&mut self, ui: &mut egui::Ui) {
        egui::ComboBox::from_id_salt("spatial-backend")
            .selected_text(self.backend.label())
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for backend in SpatialBackendKind::ALL {
                    ui.selectable_value(&mut self.backend, backend, backend.label());
                }
            });
        ui.label(
            RichText::new(self.backend.availability())
                .size(11.0)
                .color(theme::MUTED),
        );
        ui.separator();
        key_value(ui, "Stream", "Not created");
        key_value(ui, "Object capacity", "Unknown");
    }

    fn draw_scene(&mut self, root: &mut egui::Ui) {
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
                        ui.label(
                            RichText::new("48 kHz · planar f32")
                                .size(11.0)
                                .color(theme::MUTED),
                        );
                    });
                });
                ui.add_space(10.0);

                metric_strip(ui);

                ui.add_space(16.0);
                scene_placeholder(ui, self.source.is_some());
            });
    }

    fn draw_diagnostics_window(&mut self, context: &egui::Context) {
        if !self.diagnostics_open {
            return;
        }

        let remains_open = context.show_viewport_immediate(
            egui::ViewportId::from_hash_of("playback-diagnostics"),
            egui::ViewportBuilder::default()
                .with_title("MacinDecode AC-4 Diagnostics")
                .with_inner_size([460.0, 390.0])
                .with_min_inner_size([400.0, 300.0]),
            |root, _class| {
                let close_requested = root.ctx().input(|input| input.viewport().close_requested());
                draw_diagnostics_content(root);
                !close_requested
            },
        );
        self.diagnostics_open = remains_open;
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
        Self::draw_header(root);
        self.draw_source_sidebar(root);
        self.draw_transport(root);
        self.draw_scene(root);
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

fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).size(11.0).strong().color(theme::MUTED));
    ui.add_space(4.0);
}

fn card(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(theme::SURFACE)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .inner_margin(egui::Margin::same(14))
        .show(ui, contents);
}

fn key_value(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).size(12.0).color(theme::MUTED));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).size(12.0).color(theme::TEXT));
        });
    });
}

fn metric_strip(ui: &mut egui::Ui) {
    const METRICS: [(&str, &str, &str); 4] = [
        ("OBJECTS", "—", "Decoder pending"),
        ("LFE", "—", "Single native bed"),
        ("POSITION", "—", "OAMD pending"),
        ("BUFFER", "—", "Render queue offline"),
    ];

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
    for (index, (title, value, detail)) in METRICS.into_iter().enumerate() {
        let right = if index + 1 == METRICS.len() {
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
                ui.label(RichText::new(value).size(21.0).strong().color(theme::TEXT));
                ui.label(RichText::new(detail).size(10.0).color(theme::MUTED));
            },
        );
        left = right;
    }
}

fn scene_placeholder(ui: &mut egui::Ui, has_source: bool) {
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
            let headline = if has_source {
                "Ready for decoder integration"
            } else {
                "No object scene"
            };
            ui.label(
                RichText::new(headline)
                    .size(19.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "This shell does not parse metadata, decode PCM, or open an audio device.",
                )
                .color(theme::MUTED),
            );
        });
    });
}

fn draw_diagnostics_content(root: &mut egui::Ui) {
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
                        key_value(ui, "Container", "Not connected");
                        ui.separator();
                        key_value(ui, "Decoder session", "Not created");
                        ui.separator();
                        key_value(ui, "Spatial stream", "Not created");
                        ui.separator();
                        key_value(ui, "Underruns", "0");
                    });
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new("This shell does not open an audio device.")
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
        "Drop AC-4 media",
        egui::FontId::proportional(22.0),
        theme::TEXT,
    );
}
