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
    ScreenCoordinates, ScreenGeometry,
};
use crate::room_package::{REVERBERATION_TOLERANCE, RoomGeometry, Surface};

/// A room manifest read once for the page, keyed by its path.
pub(super) type RoomGeometrySlot = Option<(String, Result<RoomGeometry, String>)>;

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
        if package == RoomPackage::Room {
            // Choosing the same package again is how an edited one is re-read.
            self.room_geometry = None;
        }
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
                "The HRIR the room package binds. Choose a SOFA file or a prepared HRIR package to listen through your own.",
            );
        } else {
            hrir.on_hover_text(&settings.room_hrir);
        }
        ui.horizontal(|ui| {
            if ui.button("Choose HRIR…").clicked() {
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

        self.draw_room_geometry(ui, settings);

        ui.colored_label(
            theme::MUTED,
            "Renders at 48 kHz around a fixed listening position. A new room takes over during playback; the HRIR and the scene unit apply to a new output.",
        );
        if let Some(blocker) = settings.playback_blocker() {
            ui.colored_label(theme::WARNING, blocker);
        }
        egui::CollapsingHeader::new(if settings.room_screen.maps() {
            "Screen mapping · on"
        } else {
            "Screen mapping · off"
        })
        .id_salt("room-screen-mapping")
        .show(ui, |ui| self.draw_room_screens(ui, settings));
    }

    /// The two screens, edited as a draft and applied as one change.
    ///
    /// A room session fixes its screens when it is created, so every applied
    /// edit prepares a new output: dragging a value straight into the settings
    /// would rebuild the output on every frame of the drag, and would hand the
    /// renderer every half-typed geometry on the way.
    pub(super) fn draw_room_screens(&mut self, ui: &mut egui::Ui, settings: &mut OutputSettings) {
        let mut draft = self.room_screen_draft.unwrap_or(settings.room_screen);
        ui.checkbox(&mut draft.reproduction_enabled, "Reproduction screen")
            .on_hover_text(
                "The screen this programme is watched on. Screen-referenced objects move from the reference screen onto it; everything else stays where it is.",
            );
        if draft.reproduction_enabled {
            ui.indent("room-reproduction-screen", |ui| {
                draw_screen_geometry(ui, "room-reproduction-grid", &mut draft.reproduction);
            });
        }
        ui.horizontal(|ui| {
            ui.label("Reference screen").on_hover_text(
                "The screen the programme was mixed against. Standard is the ITU-R reference: straight ahead, 58° wide, 1.78:1.",
            );
            ui.selectable_value(&mut draft.reference_custom, false, "Standard");
            ui.selectable_value(&mut draft.reference_custom, true, "Custom");
        });
        if draft.reference_custom {
            ui.indent("room-reference-screen", |ui| {
                draw_screen_geometry(ui, "room-reference-grid", &mut draft.reference);
            });
        }

        let changed = draft != settings.room_screen;
        let problem = draft.problem();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(changed && problem.is_none(), egui::Button::new("Apply"))
                .clicked()
            {
                settings.room_screen = draft;
            }
            if ui
                .add_enabled(changed, egui::Button::new("Revert"))
                .clicked()
            {
                draft = settings.room_screen;
            }
            if ui
                .add_enabled(problem.is_none(), egui::Button::new("Copy as JSON"))
                .on_hover_text(
                    "mradm.screen-config v1, the form MacinRender's --screen-config option reads",
                )
                .clicked()
            {
                ui.ctx().copy_text(draft.config_json());
            }
        });
        if let Some(problem) = &problem {
            ui.colored_label(theme::WARNING, problem);
        } else if changed {
            ui.colored_label(theme::MUTED, "Not applied yet.");
        }
        if draft.reproduction_enabled && !draft.maps() {
            ui.colored_label(
                theme::MUTED,
                "The reproduction screen matches the reference, so nothing moves.",
            );
        }
        ui.colored_label(
            theme::MUTED,
            "Applies only to screen-referenced objects. Screens are fixed for an output, so applying prepares a new one.",
        );
        self.room_screen_draft = (draft != settings.room_screen).then_some(draft);
    }

    /// What the chosen room is, held against the listening-room geometry
    /// recommendations, and how the current scene unit sits inside it.
    pub(super) fn draw_room_geometry(&mut self, ui: &mut egui::Ui, settings: &OutputSettings) {
        if settings.room.is_empty() {
            return;
        }
        let geometry = room_geometry(&mut self.room_geometry, &settings.room);
        egui::CollapsingHeader::new("Room geometry")
            .id_salt("room-geometry")
            .default_open(true)
            .show(ui, |ui| match geometry {
                Ok(room) => draw_room_card(ui, room, settings.room_metres_per_unit),
                Err(error) => {
                    ui.colored_label(theme::WARNING, error);
                }
            });
    }

    fn pick_room_package(&mut self, package: RoomPackage, current: &str, context: &egui::Context) {
        let mut dialog = rfd::AsyncFileDialog::new();
        dialog = match package {
            RoomPackage::Room => dialog.add_filter("Room package", &["json"]),
            // The renderer prepares a SOFA itself when the session is created.
            RoomPackage::Hrir => dialog.add_filter("SOFA or HRIR package", &["sofa", "json"]),
        };
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

fn room_geometry<'a>(
    slot: &'a mut RoomGeometrySlot,
    path: &str,
) -> &'a Result<RoomGeometry, String> {
    // Read once per path: the page is drawn every frame, the file is not
    // edited that often.
    if slot.as_ref().is_none_or(|(read, _)| read != path) {
        *slot = Some((path.to_owned(), RoomGeometry::read(Path::new(path))));
    }
    &slot.as_ref().expect("filled above").1
}

fn mark(ui: &mut egui::Ui, met: bool) {
    if met {
        ui.colored_label(theme::SUCCESS, "✓");
    } else {
        ui.colored_label(theme::WARNING, "×");
    }
}

fn elevation_label(elevation: i32) -> String {
    match elevation {
        0 => "ear level".into(),
        90 => "overhead".into(),
        _ => format!("{elevation:+}°"),
    }
}

fn draw_room_card(ui: &mut egui::Ui, room: &RoomGeometry, metres_per_unit: f64) {
    draw_room_facts(ui, room);
    ui.add_space(4.0);
    ui.strong("BS.1116-3 · EBU Tech 3276 room geometry");
    for criterion in room.criteria() {
        ui.horizontal_wrapped(|ui| {
            mark(ui, criterion.met);
            ui.label(criterion.label);
            ui.colored_label(theme::MUTED, criterion.detail);
        });
    }
    ui.colored_label(
        theme::MUTED,
        "Geometry only. Decay and early reflections are for the package's acoustic report to state; background noise, room response and listening level belong to the headphones and the room you listen in.",
    );

    ui.add_space(4.0);
    ui.strong("Scene unit in this room");
    let reach = room.reach();
    let pulled = reach
        .iter()
        .filter(|reach| metres_per_unit > reach.metres)
        .map(|reach| elevation_label(reach.elevation))
        .collect::<Vec<_>>();
    ui.horizontal_wrapped(|ui| {
        mark(ui, pulled.is_empty());
        ui.label(if pulled.is_empty() {
            format!("At {metres_per_unit:.2} m, unit distance clears every wall")
        } else {
            format!(
                "At {metres_per_unit:.2} m, the walls pull in unit distance at {}",
                pulled.join(", ")
            )
        });
    });
    ui.colored_label(
        theme::MUTED,
        format!(
            "Clear up to {}",
            reach
                .iter()
                .map(|reach| format!("{} {:.2} m", elevation_label(reach.elevation), reach.metres))
                .collect::<Vec<_>>()
                .join(" · ")
        ),
    )
    .on_hover_text(
        "The farthest a source can sit in that direction, at its worst azimuth, before the renderer moves it inside the walls along its own direction.",
    );
    ui.colored_label(
        theme::MUTED,
        format!(
            "Unit distance plays {:+.1} dB against the room's {:.2} m reference",
            room.unit_distance_gain_db(metres_per_unit),
            room.reference_distance
        ),
    );
}

/// Which package this is, and the room it describes.
fn draw_room_facts(ui: &mut egui::Ui, room: &RoomGeometry) {
    let [front, side, height] = room.dimensions;
    let [x, y, z] = room.listener;
    let nominal = room.nominal_reverberation();
    egui::Grid::new("room-geometry-facts")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("Room");
            ui.label(if room.name.is_empty() {
                &room.id
            } else {
                &room.name
            })
            .on_hover_text(&room.id);
            ui.end_row();
            ui.label("Package");
            match room.known_preset() {
                Some(id) => ui.colored_label(theme::SUCCESS, format!("✓ Published preset {id}")),
                None => ui.colored_label(theme::MUTED, "Not a published preset"),
            }
            .on_hover_text(
                "Recognised by the SHA-256 of room.json, byte for byte. The manifest pins the hash of every component, and the renderer checks them when it loads the room.",
            );
            ui.end_row();
            ui.label("SHA-256");
            ui.horizontal(|ui| {
                ui.monospace(format!("{}…", &room.manifest_sha256[..16]))
                    .on_hover_text(&room.manifest_sha256);
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(room.manifest_sha256.clone());
                }
            });
            ui.end_row();
            ui.label("Size");
            ui.label(format!("{front:.2} × {side:.2} × {height:.2} m"))
                .on_hover_text("Front–back × left–right × height");
            ui.end_row();
            ui.label("Listener");
            ui.label(format!("({x:.2}, {y:.2}, {z:.2}) m"))
                .on_hover_text("From the room's corner: front–back, left–right, height");
            ui.end_row();
            ui.label("Floor · volume");
            ui.label(format!(
                "{:.1} m² · {:.1} m³",
                room.floor_area(),
                room.volume()
            ));
            ui.end_row();
            ui.label("Absorption");
            ui.label(if room.uniform_absorption() {
                format!("{:.2} on every surface and band", room.absorption[0][0])
            } else {
                Surface::ALL
                    .map(|surface| {
                        format!("{} {:.2}", surface.label(), room.mean_absorption(surface))
                    })
                    .join(" · ")
            })
            .on_hover_text(
                "The share of early-reflection energy each surface absorbs in the room model, averaged over its eight bands. A parameter of the hybrid model, not a building material.",
            );
            ui.end_row();
            ui.label("Nominal Tm");
            ui.label(format!(
                "{nominal:.3} s ({:.3}–{:.3} s)",
                nominal - REVERBERATION_TOLERANCE,
                nominal + REVERBERATION_TOLERANCE
            ))
            .on_hover_text(
                "0.25 · (V / 100 m³)^⅓, the centre of the BS.1116-3 / EBU Tech 3276 tolerance mask; the range is its 200 Hz – 4 kHz part. A target for the package's decay, not a measurement of it.",
            );
            ui.end_row();
        });
}

/// Which number of a [`ScreenGeometry`] a row of the screen form edits.
#[derive(Clone, Copy)]
enum ScreenField {
    Centre(usize),
    Width,
    AspectRatio,
}

struct ScreenRow {
    label: &'static str,
    hover: &'static str,
    field: ScreenField,
    range: (f64, f64),
    speed: f64,
    suffix: &'static str,
}

const fn row(
    label: &'static str,
    hover: &'static str,
    field: ScreenField,
    range: (f64, f64),
    speed: f64,
    suffix: &'static str,
) -> ScreenRow {
    ScreenRow {
        label,
        hover,
        field,
        range,
        speed,
        suffix,
    }
}

const ASPECT_RATIO: ScreenRow = row(
    "Aspect ratio",
    "Width over height.",
    ScreenField::AspectRatio,
    (0.1, 10.0),
    0.01,
    "",
);
const POLAR_ROWS: [ScreenRow; 5] = [
    row(
        "Azimuth",
        "Of the screen's centre; positive is to the left.",
        ScreenField::Centre(0),
        (-180.0, 180.0),
        0.5,
        "°",
    ),
    row(
        "Elevation",
        "Of the screen's centre; positive is up.",
        ScreenField::Centre(1),
        (-89.9, 89.9),
        0.5,
        "°",
    ),
    row(
        "Distance",
        "From the listener to the screen's centre, in Scene units.",
        ScreenField::Centre(2),
        (0.01, 100.0),
        0.01,
        "",
    ),
    row(
        "Width",
        "The angle the screen spans, seen from the listener.",
        ScreenField::Width,
        (0.1, 179.9),
        0.5,
        "°",
    ),
    ASPECT_RATIO,
];
const CARTESIAN_ROWS: [ScreenRow; 5] = [
    row(
        "X",
        "Of the screen's centre; positive is to the right.",
        ScreenField::Centre(0),
        (-100.0, 100.0),
        0.01,
        "",
    ),
    row(
        "Y",
        "Of the screen's centre; positive is to the front.",
        ScreenField::Centre(1),
        (-100.0, 100.0),
        0.01,
        "",
    ),
    row(
        "Z",
        "Of the screen's centre; positive is up.",
        ScreenField::Centre(2),
        (-100.0, 100.0),
        0.01,
        "",
    ),
    row(
        "Width",
        "Along X, in Scene units.",
        ScreenField::Width,
        (0.001, 100.0),
        0.01,
        "",
    ),
    ASPECT_RATIO,
];

/// One screen's form: its coordinate system, where it is, how big, and the
/// edges that come out of those, or why there are none.
fn draw_screen_geometry(ui: &mut egui::Ui, id: &str, screen: &mut ScreenGeometry) {
    ui.horizontal(|ui| {
        ui.label("Coordinates");
        let mut coordinates = screen.coordinates;
        ui.selectable_value(&mut coordinates, ScreenCoordinates::Polar, "Polar");
        ui.selectable_value(&mut coordinates, ScreenCoordinates::Cartesian, "Cartesian");
        if coordinates != screen.coordinates {
            *screen = screen.converted(coordinates);
        }
    });
    let rows = match screen.coordinates {
        ScreenCoordinates::Polar => &POLAR_ROWS,
        ScreenCoordinates::Cartesian => &CARTESIAN_ROWS,
    };
    egui::Grid::new(id).num_columns(2).show(ui, |ui| {
        for row in rows {
            ui.label(row.label).on_hover_text(row.hover);
            let value = match row.field {
                ScreenField::Centre(index) => &mut screen.centre[index],
                ScreenField::Width => &mut screen.width,
                ScreenField::AspectRatio => &mut screen.aspect_ratio,
            };
            ui.add(
                egui::DragValue::new(value)
                    .range(row.range.0..=row.range.1)
                    .speed(row.speed)
                    .max_decimals(3)
                    .suffix(row.suffix),
            );
            ui.end_row();
        }
    });
    match screen.edges() {
        Ok(edges) => {
            ui.colored_label(
                theme::MUTED,
                format!(
                    "Spans azimuth {:+.1}° to {:+.1}°, elevation {:+.1}° to {:+.1}°",
                    edges.right, edges.left, edges.bottom, edges.top
                ),
            );
        }
        Err(error) => {
            ui.colored_label(theme::WARNING, error);
        }
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
