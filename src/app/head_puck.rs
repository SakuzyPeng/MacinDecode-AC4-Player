//! Where the listener is facing, as one small compass that is always on
//! screen, and the keys that turn it without opening a window.
//!
//! Orientation used to be reachable only through Audio settings → Head, which
//! put the reading that changes continuously behind a window drawn over the
//! scene it applies to. The puck goes in the scene header instead, beside the
//! picture it turns.
//!
//! It is drawn only where this program owns the pose. Under system spatial
//! audio the system spatializer holds the listener and the tracker publishes an
//! identity pose — `head_tracking`'s loop zeroes `goal` whenever head control
//! is not enabled — so a puck there would draw a confident zero over an
//! orientation nobody here knows.
//!
//! # One frame, not two
//!
//! The glyph is the listener seen from behind and above:
//!
//! - the **bearing dot** rides the ring where they are facing, front up. That
//!   is yaw and nothing else.
//! - the **eye line** across the middle rises when they look up and tips the
//!   way the head tips, which is pitch and roll.
//!
//! An artificial horizon would draw that line in the head's own frame, sliding
//! and tilting it *against* the motion. Half of this glyph cannot be head-fixed
//! — yaw has no meaning in a frame that turns with the head — so both halves
//! are world-fixed and the line follows the head rather than opposing it.
use super::{PlayerApp, RichText, Stroke, egui, theme};
use crate::head_tracking::{Confidence, HeadSnapshot, HeadSource};

/// The puck in the scene header, in points. One line of the heading beside it.
pub const HEADER_SIZE: f32 = 26.0;

/// The puck on the Head page, where it is also the drag target that replaced a
/// labelled grey box.
pub const PAGE_SIZE: f32 = 56.0;

/// Degrees per point of drag, on either puck.
///
/// The same rate the old drag box used. A drag is not bounded by the widget it
/// started in, so the smaller target costs reach, not speed.
pub const DRAG_DEGREES_PER_POINT: f32 = 0.35;

/// Degrees per arrow key, and with `Shift` held.
pub const STEP: f32 = 5.0;
pub const FINE_STEP: f32 = 1.0;

/// How much of the inner radius a full ±85° of pitch moves the eye line. Short
/// of all of it, so the line never shrinks to a point at the extremes.
const PITCH_SPAN: f32 = 0.78;

/// The disc the eye line is drawn in, as a fraction of the ring, leaving the
/// bearing dot a lane of its own.
const EYE_RADIUS: f32 = 0.62;

/// The bearing dot, as a fraction of the ring it is inscribed in.
const DOT_RADIUS: f32 = 0.19;

/// What the puck or a key asked the listener's head to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    Recenter,
    /// Face these angles, which is a manual pose by definition.
    Turn([f32; 3]),
    Source(HeadSource),
    /// Stop following whatever drives the pose, or start following it again.
    Hold,
}

/// Where the puck's three marks land, in screen coordinates.
pub struct Marks {
    pub centre: egui::Pos2,
    pub radius: f32,
    /// The bearing dot: inscribed in the ring, in the direction the listener is
    /// facing. Its own radius is here so that it cannot be sized separately
    /// from the reach that keeps it inside the puck.
    pub bearing: egui::Pos2,
    pub dot: f32,
    /// The eye line, already clipped to the inner disc.
    pub eye_line: [egui::Pos2; 2],
}

/// Place the marks for one pose.
///
/// Screen `y` runs down, so front is `-y` and a positive roll — tipping toward
/// the right shoulder, seen from behind — turns the eye line clockwise.
pub fn marks(rect: egui::Rect, [yaw, pitch, roll]: [f32; 3]) -> Marks {
    let centre = rect.center();
    let radius = rect.width().min(rect.height()) / 2.0 - 1.0;
    let (sin_yaw, cos_yaw) = yaw.to_radians().sin_cos();
    let dot = radius * DOT_RADIUS;
    let bearing = centre + egui::vec2(-sin_yaw, -cos_yaw) * (radius - dot);
    let inner = radius * EYE_RADIUS;
    let (sin_roll, cos_roll) = roll.to_radians().sin_cos();
    let along = egui::vec2(cos_roll, sin_roll);
    let across = egui::vec2(-sin_roll, cos_roll);
    // Looking up raises the line, so a positive pitch offsets against `across`.
    let offset = -(pitch.clamp(-85.0, 85.0) / 85.0) * inner * PITCH_SPAN;
    let half = (inner * inner - offset * offset).max(0.0).sqrt();
    let middle = centre + across * offset;
    Marks {
        centre,
        radius,
        bearing,
        dot,
        eye_line: [middle - along * half, middle + along * half],
    }
}

/// Where a turn of `[yaw, pitch]` degrees leaves the listener.
///
/// Yaw wraps rather than clamps: the pose is read back through
/// `Quaternion::euler`, which always answers in (-180°, 180°], so a clamp would
/// only stop repeated presses somewhere the readout never shows. Pitch clamps
/// to the same ±85° `Quaternion::from_euler` enforces, so the number on screen
/// is the one that was applied. Roll is not on any key: nothing turns a head
/// sideways by accident, and the sensor is the only thing that reports it.
pub fn turn([yaw, pitch, roll]: [f32; 3], [by_yaw, by_pitch]: [f32; 2]) -> [f32; 3] {
    [
        (yaw + by_yaw + 180.0).rem_euclid(360.0) - 180.0,
        (pitch + by_pitch).clamp(-85.0, 85.0),
        roll,
    ]
}

/// The colour the whole glyph is drawn in.
const fn tone(confidence: Confidence) -> egui::Color32 {
    match confidence {
        Confidence::Tracking => theme::SUCCESS,
        Confidence::Held => theme::MUTED,
        Confidence::Degraded => theme::WARNING,
    }
}

/// Read the keys the window answers to, wherever the pointer is.
///
/// Every one is a bare key, so they are read only when nothing holds keyboard
/// focus: `egui_wants_keyboard_input` is true for any focused widget, which
/// covers both a text field being typed into and a slider reached by `Tab`.
/// `consume_key` then takes the press, so a shortcut cannot also reach a widget
/// drawn later in the same pass. It matches extra `Shift` and `Alt` but not
/// `Ctrl` or `Cmd`, which is why the fine step is offered first — `Shift` +
/// arrow would otherwise answer to the coarse one.
pub fn shortcut(context: &egui::Context, angles: [f32; 3], source: HeadSource) -> Option<Command> {
    if context.egui_wants_keyboard_input() {
        return None;
    }
    context.input_mut(|input| {
        if input.consume_key(egui::Modifiers::NONE, egui::Key::R) {
            return Some(Command::Recenter);
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::H) {
            return Some(Command::Hold);
        }
        if !turnable(source) {
            return None;
        }
        for (key, direction) in [
            (egui::Key::ArrowLeft, [1.0, 0.0]),
            (egui::Key::ArrowRight, [-1.0, 0.0]),
            (egui::Key::ArrowUp, [0.0, 1.0]),
            (egui::Key::ArrowDown, [0.0, -1.0]),
        ] {
            for (modifiers, step) in [
                (egui::Modifiers::SHIFT, FINE_STEP),
                (egui::Modifiers::NONE, STEP),
            ] {
                if input.consume_key(modifiers, key) {
                    return Some(Command::Turn(turn(angles, direction.map(|v| v * step))));
                }
            }
        }
        None
    })
}

/// Whether a pose set here would survive the next sample.
///
/// A `PoseBridge` sensor overrules a manual pose immediately, so turning under
/// one would read as the key or the drag having done nothing. `AirPods` is not
/// excluded for the same reason the Head page does not exclude it: taking the
/// pose by hand is how you leave that source, and `manual_head` says so.
const fn turnable(source: HeadSource) -> bool {
    !matches!(source, HeadSource::PoseBridge)
}

/// Draw the puck and report what it was asked to do.
pub fn draw(
    ui: &mut egui::Ui,
    head: &HeadSnapshot,
    source: HeadSource,
    size: f32,
) -> Option<Command> {
    let mut command = None;
    let angles = head.pose.euler();
    let (rect, response) =
        ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::click_and_drag());
    let marks = marks(rect, angles);
    let tone = tone(head.status.confidence());
    let painter = ui.painter();
    painter.circle_filled(marks.centre, marks.radius, theme::SURFACE);
    painter.circle_stroke(
        marks.centre,
        marks.radius,
        Stroke::new(
            1.0,
            if response.hovered() {
                tone
            } else {
                theme::BORDER
            },
        ),
    );
    painter.line_segment(marks.eye_line, Stroke::new(marks.radius / 9.0, tone));
    painter.circle_filled(marks.bearing, marks.dot, tone);
    if turnable(source) && response.dragged() {
        let delta = response.drag_delta() * -DRAG_DEGREES_PER_POINT;
        command = Some(Command::Turn(turn(angles, [delta.x, delta.y])));
    }
    if response.double_clicked() {
        command = Some(Command::Recenter);
    }
    response.context_menu(|ui| {
        for option in HeadSource::ALL {
            if ui
                .add_enabled(
                    option.available(),
                    egui::Button::selectable(option == source, option.label()),
                )
                .clicked()
            {
                command = Some(Command::Source(option));
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Recenter").clicked() {
            command = Some(Command::Recenter);
            ui.close();
        }
    });
    response.on_hover_ui(|ui| {
        ui.label(RichText::new(head.status.label()).color(theme::TEXT));
        ui.label(
            RichText::new(format!(
                "Yaw {:.0}° · Pitch {:.0}° · Roll {:.0}°",
                angles[0], angles[1], angles[2]
            ))
            .color(theme::MUTED),
        );
        ui.separator();
        ui.small(if turnable(source) {
            "Drag to turn · double-click to recentre · right-click for the source\nR recentre · H hold · arrow keys turn, with Shift by 1°"
        } else {
            "Double-click to recentre the sensor · right-click for the source\nR recentre · H hold"
        });
    });
    command
}

impl PlayerApp {
    /// Whether this program, rather than the system spatializer, is holding the
    /// listener. The same condition the Head page splits on.
    pub(super) fn owns_head_orientation(&self) -> bool {
        self.output
            .settings()
            .mode
            .resolved()
            .carries_head_orientation()
    }

    /// The puck in the scene header, drawn where the pose is ours to show.
    pub(super) fn draw_head_puck(&mut self, ui: &mut egui::Ui) -> Option<Command> {
        if !self.owns_head_orientation() {
            return None;
        }
        let head = self.output.head_snapshot();
        draw(ui, &head, self.output.settings().head_source, HEADER_SIZE)
    }

    /// Read the window-wide keys, where the pose is ours to set.
    pub(super) fn head_shortcut(&mut self, context: &egui::Context) -> Option<Command> {
        if !self.owns_head_orientation() {
            return None;
        }
        let angles = self.output.head_snapshot().pose.euler();
        shortcut(context, angles, self.output.settings().head_source)
    }

    /// Apply one command from either the puck or a key.
    pub(super) fn apply_head_command(&mut self, command: Command, context: &egui::Context) {
        match command {
            Command::Recenter => self.recenter_listener(),
            Command::Turn(angles) => {
                // `manual_head` selects the manual source itself, which is what
                // makes turning by hand the way to leave a sensor behind.
                self.output.manual_head(angles);
                self.preferences.manual_head = angles;
            }
            Command::Source(source) => self.change_head_source(source, context),
            Command::Hold => {
                self.output.toggle_head_hold();
                context.request_repaint_after(std::time::Duration::from_millis(20));
            }
        }
    }

    /// Head source is not part of `needs_rebuild`, so this reconfigures the
    /// tracker without disturbing the stream.
    fn change_head_source(&mut self, source: HeadSource, context: &egui::Context) {
        let mut settings = self.output.settings().clone();
        settings.head_source = source;
        self.change_output_settings(settings, context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::SpatialBackendKind;
    use crate::head_tracking::{HeadStatus, Quaternion};

    const SIDE: f32 = 100.0;

    fn rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(SIDE))
    }

    fn head(angles: [f32; 3]) -> HeadSnapshot {
        HeadSnapshot {
            pose: Quaternion::from_euler(angles),
            status: HeadStatus::Manual,
            measurement: None,
        }
    }

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            )),
            events,
            ..Default::default()
        }
    }

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    /// Front is up and the dot stays on one circle, so the puck reads as a
    /// bearing rather than as a dot that also wanders.
    #[test]
    fn the_bearing_dot_says_which_way_the_listener_faces() {
        let centre = rect().center();
        let ahead = marks(rect(), [0.0; 3]);
        assert!((ahead.bearing.x - centre.x).abs() < 1e-3);
        assert!(ahead.bearing.y < centre.y, "front is up");
        let reach = (ahead.bearing - centre).length();
        assert!(
            reach + ahead.dot <= ahead.radius + 1e-3,
            "the dot is inscribed in the ring rather than poking out of the puck"
        );
        // Positive yaw turns left, so the dot goes left on a screen whose y
        // runs down. Anything else and the puck would mirror the scene.
        for (yaw, expected) in [
            (90.0, egui::vec2(-1.0, 0.0)),
            (-90.0, egui::vec2(1.0, 0.0)),
            (180.0, egui::vec2(0.0, 1.0)),
        ] {
            let turned = marks(rect(), [yaw, 0.0, 0.0]);
            let offset = turned.bearing - centre;
            assert!(
                (offset - expected * reach).length() < 1e-2,
                "yaw {yaw} put the dot at {offset:?}"
            );
        }
    }

    /// Pitch raises the eye line, roll tips it the way the head tips, and the
    /// line keeps its ends on the inner disc at every combination of the two.
    #[test]
    fn the_eye_line_rises_looking_up_and_tips_with_the_head() {
        let centre = rect().center();
        let level = marks(rect(), [0.0; 3]);
        let [left, right] = level.eye_line;
        assert!((left.y - centre.y).abs() < 1e-3 && (right.y - centre.y).abs() < 1e-3);
        assert!(left.x < centre.x && right.x > centre.x);

        let middle = |angles: [f32; 3]| {
            let [a, b] = marks(rect(), angles).eye_line;
            a + (b - a) / 2.0
        };
        assert!(
            middle([0.0, 40.0, 0.0]).y < centre.y,
            "looking up raises it"
        );
        assert!(
            middle([0.0, -40.0, 0.0]).y > centre.y,
            "looking down lowers it"
        );

        // Seen from behind, tipping toward the right shoulder puts the right
        // ear down, which is clockwise.
        let [a, b] = marks(rect(), [0.0, 0.0, 30.0]).eye_line;
        assert!(b.x > a.x && b.y > a.y, "positive roll turns it clockwise");
        let [a, b] = marks(rect(), [0.0, 0.0, -30.0]).eye_line;
        assert!(
            b.x > a.x && b.y < a.y,
            "negative roll turns it the other way"
        );

        let inner = level.radius * EYE_RADIUS;
        for pitch in [-85.0, -30.0, 0.0, 30.0, 85.0] {
            for roll in [-170.0, -60.0, 0.0, 60.0, 170.0] {
                let marks = marks(rect(), [0.0, pitch, roll]);
                for end in marks.eye_line {
                    let reach = (end - marks.centre).length();
                    assert!(
                        reach <= inner + 1e-3,
                        "pitch {pitch} roll {roll} put an end {reach} out"
                    );
                }
                // A line, not a point: the extremes stay legible.
                let [a, b] = marks.eye_line;
                assert!(
                    (b - a).length() > inner,
                    "pitch {pitch} roll {roll} shrank it"
                );
            }
        }
    }

    /// Yaw wraps because the pose reports it back wrapped; pitch clamps because
    /// `from_euler` clamps it, so the readout is always the angle applied.
    #[test]
    fn a_turn_wraps_yaw_and_clamps_pitch() {
        #[track_caller]
        fn turns(from: [f32; 3], by: [f32; 2], expected: [f32; 3]) {
            let actual = turn(from, by);
            let apart = actual
                .into_iter()
                .zip(expected)
                .fold(0.0_f32, |worst, (a, b)| worst.max((a - b).abs()));
            assert!(apart < 1e-3, "{from:?} by {by:?} gave {actual:?}");
        }
        turns([179.0, 0.0, 0.0], [5.0, 0.0], [-176.0, 0.0, 0.0]);
        turns([-179.0, 0.0, 0.0], [-5.0, 0.0], [176.0, 0.0, 0.0]);
        turns([0.0, 84.0, 0.0], [0.0, 5.0], [0.0, 85.0, 0.0]);
        turns([0.0, -84.0, 0.0], [0.0, -5.0], [0.0, -85.0, 0.0]);
        // Roll is on no key and no drag axis; only a sensor reports it.
        turns([0.0, 0.0, 12.0], [5.0, 5.0], [5.0, 5.0, 12.0]);
        // Every wrapped yaw is one `euler` reports, so the number on the page
        // is the number that was applied.
        let mut yaw = 0.0;
        for _ in 0..200 {
            yaw = turn([yaw, 0.0, 0.0], [STEP, 0.0])[0];
            let read = Quaternion::from_euler([yaw, 0.0, 0.0]).euler()[0];
            assert!((read - yaw).abs() < 0.05, "{yaw} read back as {read}");
        }
    }

    /// A pose nobody here holds gets no puck: under system spatial audio the
    /// tracker publishes an identity pose, and drawing it would be a confident
    /// zero over an orientation this program cannot see.
    #[test]
    fn the_puck_follows_the_tracker_rather_than_the_mode_name() {
        assert!(SpatialBackendKind::SafBinaural.carries_head_orientation());
        assert!(SpatialBackendKind::WindowsSpatialAudio.carries_head_orientation());
        assert!(!SpatialBackendKind::SystemSpatial.carries_head_orientation());
        // Automatic is not an answer; it has to be resolved first.
        assert!(!SpatialBackendKind::Automatic.carries_head_orientation());
    }

    fn pressed(
        context: &egui::Context,
        events: Vec<egui::Event>,
        source: HeadSource,
    ) -> (Option<Command>, bool) {
        let mut command = None;
        let mut left = false;
        let mut output = context.run_ui(input(events), |root| {
            command = shortcut(root.ctx(), [0.0; 3], source);
            left = root.ctx().input(|input| {
                input.key_pressed(egui::Key::R) || input.key_pressed(egui::Key::ArrowLeft)
            });
        });
        output.textures_delta.clear();
        (command, left)
    }

    #[test]
    fn the_listener_keys_are_read_and_consumed() {
        let context = egui::Context::default();
        let (command, left) = pressed(
            &context,
            vec![key(egui::Key::R, egui::Modifiers::NONE)],
            HeadSource::Manual,
        );
        assert_eq!(command, Some(Command::Recenter));
        assert!(!left, "a handled key must not also reach a widget");
        assert_eq!(
            pressed(
                &context,
                vec![key(egui::Key::H, egui::Modifiers::NONE)],
                HeadSource::Manual
            )
            .0,
            Some(Command::Hold)
        );
        // Ctrl and Cmd are somebody else's; extra Shift is the fine step.
        assert_eq!(
            pressed(
                &context,
                vec![key(egui::Key::R, egui::Modifiers::COMMAND)],
                HeadSource::Manual
            )
            .0,
            None
        );
    }

    #[test]
    fn arrows_turn_the_listener_and_shift_takes_the_fine_step() {
        let context = egui::Context::default();
        for (pressed_key, coarse) in [
            (egui::Key::ArrowLeft, [STEP, 0.0, 0.0]),
            (egui::Key::ArrowRight, [-STEP, 0.0, 0.0]),
            (egui::Key::ArrowUp, [0.0, STEP, 0.0]),
            (egui::Key::ArrowDown, [0.0, -STEP, 0.0]),
        ] {
            assert_eq!(
                pressed(
                    &context,
                    vec![key(pressed_key, egui::Modifiers::NONE)],
                    HeadSource::Manual
                )
                .0,
                Some(Command::Turn(coarse))
            );
            // `consume_key` matches a pattern with extra Shift, so the fine
            // step has to be offered first or Shift would answer as 5°.
            let fine = coarse.map(|v| v / STEP * FINE_STEP);
            assert_eq!(
                pressed(
                    &context,
                    vec![key(pressed_key, egui::Modifiers::SHIFT)],
                    HeadSource::Manual
                )
                .0,
                Some(Command::Turn(fine))
            );
        }
    }

    /// A sensor overwrites a manual pose on its next sample, so an arrow key
    /// under one would read as the key having done nothing. Recentring is the
    /// opposite: that is what re-references the sensor.
    #[test]
    fn a_sensor_pose_is_recentred_by_key_but_never_nudged() {
        let context = egui::Context::default();
        assert_eq!(
            pressed(
                &context,
                vec![key(egui::Key::ArrowLeft, egui::Modifiers::NONE)],
                HeadSource::PoseBridge
            )
            .0,
            None
        );
        assert_eq!(
            pressed(
                &context,
                vec![key(egui::Key::R, egui::Modifiers::NONE)],
                HeadSource::PoseBridge
            )
            .0,
            Some(Command::Recenter)
        );
    }

    /// Bare letters are shortcuts only while nothing is being typed into.
    #[test]
    fn keys_belong_to_a_focused_widget_first() {
        let context = egui::Context::default();
        let mut text = String::new();
        // Focus is resolved at the end of a pass, so taking it needs its own.
        context
            .run_ui(input(vec![]), |root| {
                root.text_edit_singleline(&mut text).request_focus();
            })
            .textures_delta
            .clear();
        let mut command = Some(Command::Hold);
        context
            .run_ui(
                input(vec![key(egui::Key::R, egui::Modifiers::NONE)]),
                |root| {
                    root.text_edit_singleline(&mut text);
                    command = shortcut(root.ctx(), [0.0; 3], HeadSource::Manual);
                },
            )
            .textures_delta
            .clear();
        assert_eq!(command, None);
        context
            .run_ui(input(vec![]), |root| {
                root.ctx().memory_mut(egui::Memory::stop_text_input);
                let _ = root.label("");
            })
            .textures_delta
            .clear();
        let mut command = None;
        context
            .run_ui(
                input(vec![key(egui::Key::R, egui::Modifiers::NONE)]),
                |root| {
                    command = shortcut(root.ctx(), [0.0; 3], HeadSource::Manual);
                },
            )
            .textures_delta
            .clear();
        assert_eq!(command, Some(Command::Recenter), "focus was surrendered");
    }

    /// Dragging the puck turns the listener at the rate the old drag box used,
    /// and it is the pointer's travel that turns them, not where it landed.
    #[test]
    fn dragging_the_puck_turns_the_listener_by_how_far_it_moved() {
        fn pass(
            context: &egui::Context,
            source: HeadSource,
            events: Vec<egui::Event>,
        ) -> Option<Command> {
            let mut command = None;
            let mut output = context.run_ui(input(events), |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    command = draw(ui, &head([0.0; 3]), source, PAGE_SIZE);
                });
            });
            output.textures_delta.clear();
            command
        }
        for (source, expected) in [
            (HeadSource::Manual, Some(Command::Turn([-7.0, -3.5, 0.0]))),
            // A sensor writes the pose back on its next sample.
            (HeadSource::PoseBridge, None),
        ] {
            let context = egui::Context::default();
            let grab = egui::pos2(20.0, 20.0);
            // egui resolves a press against the widget rects of the previous
            // pass, so the puck has to have been drawn once before it can be
            // grabbed at all.
            assert_eq!(pass(&context, source, vec![]), None);
            assert_eq!(
                pass(
                    &context,
                    source,
                    vec![
                        egui::Event::PointerMoved(grab),
                        egui::Event::PointerButton {
                            pos: grab,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                ),
                None,
                "pressing without moving is not a turn"
            );
            assert_eq!(
                pass(
                    &context,
                    source,
                    vec![egui::Event::PointerMoved(grab + egui::vec2(20.0, 10.0))],
                ),
                expected,
                "{source:?}"
            );
        }
    }

    /// The painted dot is the one `marks` places, so the geometry the tests
    /// check is the geometry on screen.
    #[test]
    fn the_puck_paints_the_marks_it_reports() {
        fn dots(shape: &egui::Shape, found: &mut Vec<egui::Pos2>) {
            match shape {
                egui::Shape::Circle(circle) if circle.fill != egui::Color32::TRANSPARENT => {
                    found.push(circle.center);
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        dots(shape, found);
                    }
                }
                _ => {}
            }
        }
        let context = egui::Context::default();
        let snapshot = head([35.0, 20.0, -10.0]);
        let mut placed = None;
        let mut output = context.run_ui(input(vec![]), |root| {
            egui::CentralPanel::default().show(root, |ui| {
                let before = ui.cursor().min;
                assert_eq!(draw(ui, &snapshot, HeadSource::Manual, PAGE_SIZE), None);
                placed = Some(marks(
                    egui::Rect::from_min_size(before, egui::Vec2::splat(PAGE_SIZE)),
                    snapshot.pose.euler(),
                ));
            });
        });
        let expected = placed.expect("the puck was drawn");
        let mut found = Vec::new();
        for clipped in &output.shapes {
            dots(&clipped.shape, &mut found);
        }
        assert!(
            found
                .iter()
                .any(|dot| (*dot - expected.bearing).length() < 0.5),
            "no bearing dot near {:?} among {found:?}",
            expected.bearing
        );
        output.textures_delta.clear();
    }
}
