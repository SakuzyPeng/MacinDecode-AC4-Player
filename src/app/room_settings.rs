//! The binaural page's renderer choice, and the room renderer's own controls.
//!
//! SAF HRTF and the room renderer write the same two channels to the same
//! stereo device, so they share the binaural mode and its HRTF page the way
//! SAF VBAP and Triple Balance share the Speakers page. What differs is what
//! each one is handed: a SOFA dataset for SAF, and for the room a room package,
//! an optional prepared HRIR package and a scale. Neither package is a single
//! file — each manifest names component files beside it — so they are picked
//! where they lie rather than imported into a managed folder.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use super::{Arc, Context, DialogWake, PlayerApp, Poll, Waker, egui, theme};
use crate::backend::{
    BinauralRenderer, OutputSettings, ROOM_METRES_PER_UNIT_DEFAULT, ROOM_METRES_PER_UNIT_RANGE,
};

/// Which of the room renderer's two packages a file dialog is choosing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RoomPackage {
    Room,
    Hrir,
}

pub(super) struct RoomPicker {
    pub(super) package: RoomPackage,
    pub(super) future: Pin<Box<dyn Future<Output = Option<PathBuf>>>>,
}

impl PlayerApp {
    pub(super) fn poll_room_picker(&mut self, context: &egui::Context) {
        let Some(picker) = self.room_picker.as_mut() else {
            return;
        };
        let waker = Waker::from(Arc::new(DialogWake(context.clone())));
        let Poll::Ready(path) = picker
            .future
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
        else {
            return;
        };
        let package = picker.package;
        self.room_picker = None;
        let Some(path) = path else {
            return;
        };
        let Some(path) = path.to_str() else {
            self.audio_settings_error =
                Some("This renderer requires a Unicode package path".into());
            return;
        };
        let mut settings = self.output.settings().clone();
        path.clone_into(match package {
            RoomPackage::Room => &mut settings.room,
            RoomPackage::Hrir => &mut settings.room_hrir,
        });
        self.change_output_settings(settings, context);
    }

    /// The binaural mode's renderer page: which renderer, then its controls.
    pub(super) fn draw_binaural_page(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut OutputSettings,
        context: &egui::Context,
    ) {
        ui.horizontal(|ui| {
            ui.label("Binaural renderer");
            egui::ComboBox::from_id_salt("binaural-renderer")
                .selected_text(settings.binaural_renderer.label())
                .show_ui(ui, |ui| {
                    for renderer in BinauralRenderer::ALL {
                        ui.selectable_value(
                            &mut settings.binaural_renderer,
                            renderer,
                            renderer.label(),
                        );
                    }
                });
        });
        match settings.binaural_renderer {
            BinauralRenderer::SafHrtf => self.draw_hrtf_page(ui, settings, context),
            BinauralRenderer::Room => self.draw_room_controls(ui, settings, context),
        }
    }

    fn draw_room_controls(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut OutputSettings,
        context: &egui::Context,
    ) {
        let room = ui.label(if settings.room.is_empty() {
            "Room: none chosen".to_owned()
        } else {
            format!("Room: {}", package_label(&settings.room))
        });
        if !settings.room.is_empty() {
            room.on_hover_text(&settings.room);
        }
        ui.horizontal(|ui| {
            if ui.button("Choose room package…").clicked() {
                self.pick_room_package(RoomPackage::Room, &settings.room, context);
            }
        });

        let hrir = ui.label(if settings.room_hrir.is_empty() {
            "HRIR: room package default".to_owned()
        } else {
            format!("HRIR: {}", package_label(&settings.room_hrir))
        });
        if settings.room_hrir.is_empty() {
            hrir.on_hover_text(
                "The HRIR the room package binds. A personal SOFA has to be prepared into an HRIR package before the room renderer can use it.",
            );
        } else {
            hrir.on_hover_text(&settings.room_hrir);
        }
        ui.horizontal(|ui| {
            if ui.button("Choose HRIR package…").clicked() {
                self.pick_room_package(RoomPackage::Hrir, &settings.room_hrir, context);
            }
            if ui
                .add_enabled(
                    !settings.room_hrir.is_empty(),
                    egui::Button::new("Use room default"),
                )
                .clicked()
            {
                settings.room_hrir.clear();
            }
        });

        ui.horizontal(|ui| {
            ui.label("Scene unit");
            ui.add(
                egui::DragValue::new(&mut settings.room_metres_per_unit)
                    .range(ROOM_METRES_PER_UNIT_RANGE)
                    .speed(0.01)
                    .max_decimals(2)
                    .suffix(" m"),
            )
            .on_hover_text(
                "How many metres one unit of Scene distance spans in the room. Sources are kept inside the walls along their own direction.",
            );
            if ui
                .add_enabled(
                    settings.room_metres_per_unit.to_bits()
                        != ROOM_METRES_PER_UNIT_DEFAULT.to_bits(),
                    egui::Button::new("Reset"),
                )
                .clicked()
            {
                settings.room_metres_per_unit = ROOM_METRES_PER_UNIT_DEFAULT;
            }
        });

        ui.colored_label(
            theme::MUTED,
            "Renders at 48 kHz around a fixed listening position. A new room takes over during playback; the HRIR and the scene unit apply to a new output.",
        );
        if let Some(blocker) = settings.playback_blocker() {
            ui.colored_label(theme::WARNING, blocker);
        }
    }

    fn pick_room_package(&mut self, package: RoomPackage, current: &str, context: &egui::Context) {
        let mut dialog = rfd::AsyncFileDialog::new().add_filter(
            match package {
                RoomPackage::Room => "Room package",
                RoomPackage::Hrir => "HRIR package",
            },
            &["json"],
        );
        // Packages sit side by side, so the next one is usually next door.
        if let Some(folder) = Path::new(current).parent().and_then(Path::parent)
            && folder.is_dir()
        {
            dialog = dialog.set_directory(folder);
        }
        let dialog = dialog.pick_file();
        self.room_picker = Some(RoomPicker {
            package,
            future: Box::pin(async move { dialog.await.map(|file| file.path().to_path_buf()) }),
        });
        context.request_repaint();
    }
}

/// A package's manifest is conventionally `room.json` or `hrir.json`, so its
/// own file name says nothing about which package it is: name the folder too.
fn package_label(path: &str) -> String {
    let path = Path::new(path);
    let file = path.file_name().unwrap_or_default().to_string_lossy();
    match path
        .parent()
        .and_then(Path::file_name)
        .map(|folder| folder.to_string_lossy())
    {
        Some(folder) => format!("{folder}/{file}"),
        None => file.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::package_label;

    #[test]
    fn a_package_is_named_by_its_folder_and_manifest() {
        let path = std::path::Path::new("packages")
            .join("studio-a")
            .join("room.json");
        assert_eq!(package_label(path.to_str().unwrap()), "studio-a/room.json");
        assert_eq!(package_label("room.json"), "room.json");
    }
}
