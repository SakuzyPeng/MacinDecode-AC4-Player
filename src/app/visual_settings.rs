use super::{Arc, Context, DialogWake, PlayerApp, Poll, RichText, Waker, egui, theme};
use crate::scene3d::skin::BodyType;

impl PlayerApp {
    pub(super) fn restore_skin(&mut self, context: &egui::Context) {
        if let Some(id) = &self.preferences.skins.selected {
            if let Some(entry) = self
                .preferences
                .skins
                .entries
                .iter()
                .find(|entry| &entry.id == id)
            {
                self.skins.select(entry.clone(), context);
            } else {
                self.skins.error = Some("The saved skin is missing from the import list".into());
            }
        }
    }

    pub(super) fn poll_skin_picker(&mut self, context: &egui::Context) {
        if let Some(picker) = self.skin_picker.as_mut() {
            let waker = Waker::from(Arc::new(DialogWake(context.clone())));
            if let Poll::Ready(path) = picker.as_mut().poll(&mut Context::from_waker(&waker)) {
                self.skin_picker = None;
                if let Some(path) = path {
                    self.skins.import(path.path().to_path_buf(), context);
                }
            }
        }
        if let Some(mut entry) = self.skins.poll() {
            let saved = &mut self.preferences.skins;
            if let Some(existing) = saved
                .entries
                .iter()
                .find(|existing| existing.id == entry.id)
            {
                // Reimporting the same PNG keeps the user's body override.
                if let Some(skin) = &mut self.skins.active {
                    skin.set_model(existing.model);
                }
            } else {
                let name = entry.name.clone();
                let mut suffix = 2;
                while saved
                    .entries
                    .iter()
                    .any(|existing| existing.name == entry.name)
                {
                    entry.name = format!("{name} ({suffix})");
                    suffix += 1;
                }
                saved.entries.push(entry.clone());
            }
            saved.selected = Some(entry.id);
        }
    }

    pub(super) fn draw_visual_settings(&mut self, context: &egui::Context) {
        if !self.visual_settings_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Visual settings")
            .open(&mut open)
            .resizable(false)
            .default_width(360.0)
            .default_height(560.0)
            .show(context, |ui| {
                ui.set_max_width(360.0);
                egui::ScrollArea::vertical()
                    .max_height((context.content_rect().height() - 120.0).max(180.0))
                    .show(ui, |ui| {
                        self.draw_skin_settings(ui, context);
                        ui.separator();
                        ui.strong("Audio objects");
                        ui.checkbox(&mut self.object_numbers_visible, "Element numbers (IDs)")
                            .on_hover_text("Show numbers on scene objects and the LFE.");
                        ui.checkbox(&mut self.object_loudness_visible, "Object loudness (LVL)")
                            .on_hover_text("Show measured levels above objects, in their footprints and along their trails.");
                        ui.separator();
                        ui.checkbox(&mut self.fade_silent_objects, "Fade persistently silent objects");
                        ui.label(RichText::new("Quiet objects fade after about two seconds and return when sound resumes.").small().color(theme::MUTED));
                        ui.separator();
                        ui.checkbox(&mut self.meter_bank_open, "Meter bank (side panel)")
                            .on_hover_text("A row per object beside the scene: level, metadata gain, peak and clipping.");
                        ui.label(RichText::new("Off by default because it takes width from the scene; the measurement runs either way.").small().color(theme::MUTED));
                    });
            });
        self.visual_settings_open = open;
    }

    fn draw_skin_settings(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        ui.strong("Listener skin");
        let busy = self.skins.busy() || self.skin_picker.is_some();
        ui.add_enabled_ui(self.library.ready && !busy, |ui| {
            let saved = &self.preferences.skins;
            let selected_name = saved.selected.as_ref().map_or("Default figure", |id| {
                saved
                    .entries
                    .iter()
                    .find(|entry| &entry.id == id)
                    .map_or("Missing skin", |entry| entry.name.as_str())
            });
            // Also allow clicking the current choice to retry a missing file.
            let mut choice = None;
            egui::ComboBox::from_id_salt("listener_skin")
                .selected_text(selected_name)
                .width(330.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(saved.selected.is_none(), "Default figure")
                        .clicked()
                    {
                        choice = Some(None);
                        ui.close();
                    }
                    for entry in &saved.entries {
                        ui.push_id(&entry.id, |ui| {
                            if ui
                                .selectable_label(
                                    saved.selected.as_ref() == Some(&entry.id),
                                    &entry.name,
                                )
                                .clicked()
                            {
                                choice = Some(Some(entry.clone()));
                                ui.close();
                            }
                        });
                    }
                });
            if let Some(choice) = choice {
                if let Some(entry) = choice {
                    self.skins.select(entry, context);
                } else {
                    self.preferences.skins.selected = None;
                    self.skins.clear();
                }
            }
            if ui.button("Import skin PNG…").clicked() {
                self.skin_picker = Some(Box::pin(
                    rfd::AsyncFileDialog::new()
                        .set_title("Import Minecraft skin")
                        .add_filter("Minecraft skin PNG", &["png"])
                        .pick_file(),
                ));
            }
            self.draw_skin_body_type(ui);
        });
        ui.label(
            RichText::new(
                "64×64 PNG · Steve / Alex detected automatically. Legacy 64×32 skins use Steve.",
            )
            .small()
            .color(theme::MUTED),
        );
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(if self.skin_picker.is_some() {
                    "Choosing skin…"
                } else {
                    "Loading skin…"
                });
            });
        }
        if let Some(error) = &self.skins.error {
            ui.colored_label(theme::WARNING, error);
        }
    }

    fn draw_skin_body_type(&mut self, ui: &mut egui::Ui) {
        let Some(skin) = &mut self.skins.active else {
            return;
        };
        let saved = &mut self.preferences.skins;
        let Some(entry) = saved
            .entries
            .iter_mut()
            .find(|entry| Some(&entry.id) == saved.selected.as_ref())
        else {
            return;
        };
        let auto = format!("Auto: {}", skin.detected.label());
        let previous = entry.model;
        ui.add_enabled_ui(!skin.legacy(), |ui| {
            egui::ComboBox::from_id_salt("listener_skin_body")
                .selected_text(entry.model.map_or_else(|| auto.clone(), |model| model.label().to_owned()))
                .width(330.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut entry.model, None, &auto).clicked() {
                        ui.close();
                    }
                    for model in [BodyType::Steve, BodyType::Alex] {
                        if ui.selectable_value(&mut entry.model, Some(model), model.label()).clicked() {
                            ui.close();
                        }
                    }
                }).response.on_hover_text("Automatic detection uses transparent arm margins. Choose a body type if your editor filled those margins.");
        });
        if previous != entry.model {
            skin.set_model(entry.model);
        }
    }
}
