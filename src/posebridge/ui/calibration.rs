//! The magnetic calibration page: one `PoseBridge` magnetic session in the
//! stages `docs/POSEBRIDGE-MAGNETIC.md` walks through, the coverage grid each
//! sweep is steered by, and the three commands a session takes.
//!
//! Every number drawn here is the player's own reading of the field the sensor
//! reports, never a verdict on the sensor's calibration. Nothing here reaches
//! the listener or a renderer, and drawing the page opens nothing: a session
//! starts from its own button.
use super::{
    Command, Panel, Phase, Preferences, Service, Tab, View, configuration, egui, mounting_words,
    operation_lines, pb, section, wide,
};
use crate::posebridge::coverage::{self, CELLS, COLUMNS, ROWS};
use crate::posebridge::magnetic::{self, Sweep, Units};
use crate::theme;
use std::time::{Duration, Instant};

/// Readings at which a cell's fill stops deepening. Past it a cell looks no
/// further along, so the grid cannot be read as a progress bar.
const FULL_AT: u16 = 6;
/// Where the spread readout changes tone. Provisional, like the fit's own
/// thresholds, until a real headset has been measured.
const LOW_SPREAD: f64 = 0.03;
const HIGH_SPREAD: f64 = 0.08;
/// The spread the scale beside the readout reaches across.
const SPREAD_SCALE: f64 = 0.12;
/// How long an instruction stays before a different one replaces it, unless
/// its target is read first. Two nearly equal components of the turn would
/// otherwise swap the words at the rate readings arrive.
const HOLD: Duration = Duration::from_millis(1200);
/// Room left of the grid for the row labels, below it for the columns', and
/// above it for the half of the top label that stands over its edge.
const ROW_LABELS: f32 = 44.;
const COLUMN_LABELS: f32 = 20.;
const EDGE: f32 = 8.;
/// The labels' size, the latest reading's marker, and the instruction's size:
/// the one line on the page meant to be read at arm's length.
const LABEL_SIZE: f32 = 11.;
const MARKER: f32 = 7.;
const INSTRUCTION_SIZE: f32 = 17.;

/// `MUTED`, `ACCENT` and `SUCCESS` deepened in the same hue to 4.8, 4.7 and
/// 5.5 : 1 on `BACKGROUND`, for small text and the grid's centre axes; the
/// originals reach only 3.0 to 3.7 : 1.
const MUTED_DEEP: egui::Color32 = egui::Color32::from_rgb(0x7a, 0x6b, 0x57);
const ACCENT_DEEP: egui::Color32 = egui::Color32::from_rgb(0xa8, 0x5a, 0x22);
const SUCCESS_DEEP: egui::Color32 = egui::Color32::from_rgb(0x4e, 0x6b, 0x51);

/// Where a session stands, as the page draws it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// No session: what one does, how to prepare, and the button that opens it.
    Intro,
    /// Asked for, and not reading yet.
    Opening,
    /// Reading, with no calibration running: the sweep before.
    Before,
    /// The sensor is calibrating, and not because this session asked it to.
    External,
    /// A calibration this session started, from its start to its verified end.
    During,
    /// After the end: the sweep after, and saving.
    After,
    /// Closing, closed or failed, and what that left behind.
    Ended,
}

impl Stage {
    fn of(view: &View) -> Self {
        let Some(session) = &view.magnetic_session else {
            return if view.phase == Phase::Magnetic {
                Self::Opening
            } else {
                Self::Intro
            };
        };
        if view.phase != Phase::Magnetic {
            return Self::Ended;
        }
        match session.phase {
            pb::MagneticPhase::Idle | pb::MagneticPhase::Opening => Self::Opening,
            pb::MagneticPhase::Monitoring => Self::Before,
            pb::MagneticPhase::ExternalCalibration => Self::External,
            pb::MagneticPhase::Starting
            | pb::MagneticPhase::Calibrating
            | pb::MagneticPhase::Stopping => Self::During,
            // The page offers a save only after an end.
            pb::MagneticPhase::Calibrated | pb::MagneticPhase::Saving => Self::After,
            pb::MagneticPhase::Closed | pb::MagneticPhase::Failed => Self::Ended,
        }
    }

    /// The sweep a stage steers, if it steers one.
    const fn sweep(self) -> Option<Sweep> {
        match self {
            Self::Before => Some(Sweep::Before),
            Self::During => Some(Sweep::During),
            Self::After => Some(Sweep::After),
            Self::Intro | Self::Opening | Self::External | Self::Ended => None,
        }
    }
}

/// The instruction on screen, the sweep it steers, and since when.
#[derive(Default)]
pub(super) struct Instruction {
    shown: Option<(Sweep, coverage::Next, Instant)>,
}

impl Instruction {
    /// What to show for `sweep` at `now`. A different instruction replaces the
    /// one on screen once that has been up for [`HOLD`], or at once when its
    /// target has been read or the sweep is another one.
    fn hold(
        &mut self,
        sweep: Sweep,
        snapshot: &coverage::Snapshot,
        now: Instant,
    ) -> Option<coverage::Next> {
        let latest = snapshot.next;
        if let Some((held, shown, since)) = self.shown
            && held == sweep
            && latest.is_some()
        {
            if latest == Some(shown) {
                return latest;
            }
            if snapshot.counts[shown.cell] == 0 && now.saturating_duration_since(since) < HOLD {
                return Some(shown);
            }
        }
        self.shown = latest.map(|next| (sweep, next, now));
        latest
    }
}

/// What a button in a session's row does.
enum Action {
    /// Send a command into the session at once.
    Send(pb::DeviceCommand),
    /// Send one through the confirmation dialog, which says what it does.
    Confirm(pb::DeviceCommand),
    /// Close the session.
    Close,
}

impl Panel {
    pub(super) fn calibration(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &Preferences,
        service: &Service,
        view: &View,
    ) {
        let stage = Stage::of(view);
        let session = view.magnetic_session.as_ref();
        if let (Some(sweep), Some(session)) = (stage.sweep(), session) {
            if stage == Stage::During {
                self.calibrating(ui, service, view, session);
            }
            let snapshot = session.sweep(sweep);
            let next = self.instruction.hold(sweep, snapshot, Instant::now());
            let title = match sweep {
                Sweep::Before => "Before calibration",
                Sweep::During => "During calibration",
                Sweep::After => "After calibration",
            };
            pair(
                ui,
                (title, |ui: &mut egui::Ui| sweep_view(ui, snapshot, next)),
                ("Magnetic session", |ui: &mut egui::Ui| {
                    self.session(ui, service, view, stage, session);
                }),
            );
            return;
        }
        match (stage, session) {
            (Stage::External, Some(session)) => {
                self.external(ui, service, view, session);
            }
            (Stage::Ended, Some(session)) => {
                section(ui, "Magnetic session", |ui| {
                    self.ended(ui, prefs, service, view, session);
                });
            }
            (Stage::Opening, _) => {
                section(ui, "Magnetic session", |ui| {
                    ui.strong(device_name(view));
                    ui.colored_label(MUTED_DEEP, "Opening the session and reading the sensor…");
                    if action(ui, true, "Close session") {
                        self.send(service, Command::Stop);
                    }
                });
            }
            _ => pair(
                ui,
                ("Before you start", introduction),
                ("Magnetic session", |ui: &mut egui::Ui| {
                    self.intro(ui, prefs, service, view);
                }),
            ),
        }
    }

    /// The banner a running calibration keeps above everything: which device,
    /// for how long, and the one operation it takes.
    fn calibrating(
        &mut self,
        ui: &mut egui::Ui,
        service: &Service,
        view: &View,
        session: &magnetic::Session,
    ) {
        let headline = match session.phase {
            pb::MagneticPhase::Starting => "Starting calibration · verifying by readback…",
            pb::MagneticPhase::Stopping => "Ending calibration · verifying by readback…",
            _ => "Calibrating",
        };
        banner(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(theme::WARNING, egui::RichText::new(headline).strong());
                ui.label(device_name(view));
                // The statistics window restarts at the verified start.
                if session.phase != pb::MagneticPhase::Starting {
                    ui.label(clock(session.statistics.elapsed_ns));
                }
            });
            self.session_buttons(
                ui,
                service,
                view,
                session,
                &[("End calibration", Action::Send(pb::DeviceCommand::MagStop))],
            );
            ui.small(
                "Ending is the only operation while it runs, and it never times out: quitting waits for an end verified by readback.",
            );
        });
    }

    /// The sensor calibrating on someone else's account.
    fn external(
        &mut self,
        ui: &mut egui::Ui,
        service: &Service,
        view: &View,
        session: &magnetic::Session,
    ) {
        banner(ui, |ui| {
            ui.colored_label(
                theme::WARNING,
                egui::RichText::new("The sensor is calibrating, and this session did not start it")
                    .strong(),
            );
            ui.label(
                "Its own procedure or another program is changing its calibration, so every sweep so far was discarded: they describe the device from before it.",
            );
            self.session_buttons(
                ui,
                service,
                view,
                session,
                &[
                    ("End calibration", Action::Send(pb::DeviceCommand::MagStop)),
                    ("Close session", Action::Close),
                ],
            );
            ui.small("Ending it here ends that calibration too.");
        });
        section(ui, "Magnetic session", |ui| details(ui, session));
    }

    /// The session beside a sweep: what the readings say, and what can be done
    /// next.
    fn session(
        &mut self,
        ui: &mut egui::Ui,
        service: &Service,
        view: &View,
        stage: Stage,
        session: &magnetic::Session,
    ) {
        let units = session.units;
        // Verifying a command pauses the reads for about as long as a reading
        // stays fresh; that is not a stall.
        if !session.active
            && !matches!(
                session.phase,
                pb::MagneticPhase::Starting
                    | pb::MagneticPhase::Stopping
                    | pb::MagneticPhase::Saving
            )
        {
            ui.colored_label(theme::WARNING, "No fresh readings are arriving.");
        }
        match stage {
            Stage::Before => {
                ui.label("Reading the field · no calibration running");
                readings(ui, session.sweep(Sweep::Before), units);
                ui.add_space(6.);
                ui.label(
                    "Turn the headset until the grid fills. What a calibration changes is judged against this sweep.",
                );
                ui.small("Starting without it leaves the sweep after with nothing to compare.");
                self.session_buttons(
                    ui,
                    service,
                    view,
                    session,
                    &[
                        (
                            "Start calibration…",
                            Action::Confirm(pb::DeviceCommand::MagStart),
                        ),
                        ("Close session", Action::Close),
                    ],
                );
                // A start the sensor refused says why only here.
                if let Some(operation) = &session.operation {
                    ui.separator();
                    operation_lines(ui, operation);
                }
            }
            Stage::During => {
                readings(ui, session.sweep(Sweep::During), units);
                ui.small(
                    "Turn it slowly through every direction, with the sensor on the headset and a metre clear of steel and electronics.",
                );
                if let Some(operation) = &session.operation {
                    ui.separator();
                    operation_lines(ui, operation);
                }
            }
            Stage::After => self.after(ui, service, view, session),
            _ => {}
        }
        details(ui, session);
    }

    /// After an end: whether it was verified, the sweep after against the one
    /// before, and saving.
    fn after(
        &mut self,
        ui: &mut egui::Ui,
        service: &Service,
        view: &View,
        session: &magnetic::Session,
    ) {
        let operation = session.operation.as_ref();
        if session.end_verified {
            ui.colored_label(SUCCESS_DEEP, "End verified by readback");
        } else {
            ui.label(
                "The sensor reports the calibration over, but not through an end this session verified.",
            );
        }
        if let Some(operation) = operation {
            operation_lines(ui, operation);
        }
        ui.separator();
        ui.label(
            "Sweep the grid once more: only readings taken after the end can say what it changed.",
        );
        comparison(
            ui,
            session.sweep(Sweep::Before),
            session.sweep(Sweep::After),
            session.units,
        );
        ui.separator();
        if operation.is_some_and(|operation| operation.action == "save" && operation.command_sent) {
            ui.colored_label(
                MUTED_DEEP,
                "SAVE sent. Whether it survives a power cycle is not verified.",
            );
        } else {
            ui.colored_label(
                theme::WARNING,
                egui::RichText::new("Nothing has been saved.").strong(),
            );
        }
        let mut actions = vec![
            (
                "Save current device settings…",
                Action::Confirm(pb::DeviceCommand::Save),
            ),
            (
                "Calibrate again…",
                Action::Confirm(pb::DeviceCommand::MagStart),
            ),
            ("Close session", Action::Close),
        ];
        if view.magnetic.is_some() {
            // Only a verified end clears the warning, whatever the sensor says.
            actions.insert(
                0,
                ("End calibration", Action::Send(pb::DeviceCommand::MagStop)),
            );
        }
        self.session_buttons(ui, service, view, session, &actions);
        ui.small(
            "Then connect and check that the listening direction holds on Tracking. The spread says only that the readings agree with one another.",
        );
    }

    /// No session yet: which device, whether its mounting can carry the
    /// guidance, and the button that opens one.
    fn intro(&mut self, ui: &mut egui::Ui, prefs: &Preferences, service: &Service, view: &View) {
        ui.strong(if prefs.device.id.is_empty() {
            "No device selected"
        } else if prefs.device.name.is_empty() {
            &prefs.device.id
        } else {
            &prefs.device.name
        });
        if coverage::proper(prefs.device.mounting) {
            ui.label(format!(
                "Mounting: {}",
                mounting_words(prefs.device.mounting).join(" · ")
            ));
        }
        ui.small(
            "The grid and every instruction are drawn in headset axes, so a wrong mounting points each one the wrong way.",
        );
        if action(ui, true, "Check the mounting on Tracking") {
            self.tab = Tab::Tracking;
        }
        if let Some(device) = &view.magnetic {
            ui.add_space(6.);
            ui.colored_label(
                theme::WARNING,
                format!(
                    "Calibration may still be active on {}. Open a session to watch it and end it there, or end it in the bar above.",
                    if device.name.is_empty() { &device.id } else { &device.name }
                ),
            );
        }
        ui.add_space(6.);
        self.open_button(ui, prefs, service, view);
    }

    /// A closing, closed or failed session, and what it left the device in.
    fn ended(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &Preferences,
        service: &Service,
        view: &View,
        session: &magnetic::Session,
    ) {
        if view.phase == Phase::Stopping {
            ui.label("Closing the session…");
            ui.small(
                "PoseBridge ends a calibration this session started before it lets go of the sensor.",
            );
        } else if session.phase == pb::MagneticPhase::Failed {
            ui.colored_label(theme::WARNING, "The session failed.");
        } else {
            ui.label("The session is closed.");
        }
        if let Some(error) = &session.last_error {
            ui.colored_label(theme::WARNING, error);
        }
        if let Some(cleanup) = &session.cleanup {
            ui.separator();
            ui.label(if cleanup.outcome == pb::OperationOutcome::Succeeded {
                "Closing ended the calibration this session had started:"
            } else {
                "Closing tried to end the calibration this session had started:"
            });
            operation_lines(ui, cleanup);
        }
        if view.magnetic.is_some() {
            ui.colored_label(
                theme::WARNING,
                "End calibration once more, in the bar above: only an end verified by readback clears the warning.",
            );
        }
        let units = unit(session.units);
        for sweep in Sweep::ALL {
            let snapshot = session.sweep(sweep);
            if snapshot.readings == 0 {
                continue;
            }
            ui.label(format!(
                "{} · {} / {CELLS} directions · spread {}{}",
                match sweep {
                    Sweep::Before => "Before",
                    Sweep::During => "During",
                    Sweep::After => "After",
                },
                snapshot.covered(),
                snapshot
                    .spread
                    .map_or_else(|| "—".into(), |spread| format!("{:.1} %", spread * 100.)),
                match snapshot.centre {
                    Some(coverage::Centre::Fitted(point)) =>
                        format!(" · centre offset {:.1}{units}", length(point)),
                    _ => String::new(),
                },
            ));
        }
        details(ui, session);
        ui.add_space(6.);
        self.open_button(ui, prefs, service, view);
    }

    fn open_button(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &Preferences,
        service: &Service,
        view: &View,
    ) {
        let open = Command::Magnetic(prefs.device.clone());
        let refusal = open_refusal(prefs, view, &open);
        if action(ui, refusal.is_none(), "Open magnetic session") {
            self.send(service, open);
        }
        if let Some(reason) = refusal {
            ui.small(reason);
        }
    }

    /// One row of buttons for the open session. A command is disabled while
    /// the service would refuse it or the session's last operation is still
    /// running, and the reasons are written under the row, each once.
    fn session_buttons(
        &mut self,
        ui: &mut egui::Ui,
        service: &Service,
        view: &View,
        session: &magnetic::Session,
        actions: &[(&str, Action)],
    ) {
        let Some(device) = view.device.clone() else {
            return;
        };
        let running = session
            .operation
            .as_ref()
            .is_some_and(|operation| operation.outcome == pb::OperationOutcome::Running);
        let mut reasons = Vec::new();
        ui.horizontal_wrapped(|ui| {
            for (label, action) in actions {
                let (Action::Send(command) | Action::Confirm(command)) = action else {
                    if ui.button(*label).clicked() {
                        self.send(service, Command::Stop);
                    }
                    continue;
                };
                let write = Command::Write(device.clone(), command.clone());
                let refusal = view.refusal(&write).or_else(|| {
                    running.then(|| "Waiting for the last operation to finish.".to_owned())
                });
                if ui
                    .add_enabled(refusal.is_none(), egui::Button::new(*label))
                    .clicked()
                {
                    if matches!(action, Action::Confirm(_)) {
                        self.confirmation = Some((device.clone(), command.clone()));
                    } else {
                        self.send(service, write);
                    }
                }
                if let Some(reason) = refusal
                    && !reasons.contains(&reason)
                {
                    reasons.push(reason);
                }
            }
        });
        for reason in reasons {
            ui.small(reason);
        }
    }
}

/// Why the session cannot be opened, in the order a user would fix it.
fn open_refusal(prefs: &Preferences, view: &View, open: &Command) -> Option<String> {
    if prefs.device.id.is_empty() {
        return Some("Choose a sensor on the Device page.".into());
    }
    if let Err(error) = magnetic::check(&prefs.device) {
        return Some(error);
    }
    if let Err(error) = configuration(&prefs.device, false) {
        return Some(
            error
                .strip_prefix("invalid argument: ")
                .unwrap_or(&error)
                .to_owned(),
        );
    }
    view.refusal(open)
}

/// What the page says before anything is opened.
fn introduction(ui: &mut egui::Ui) {
    ui.label(
        "A magnetic session reads the field the sensor reports while you turn the headset, and draws which directions it has come from.",
    );
    ui.add_space(6.);
    for line in [
        "Head tracking stops for the whole session. The listening direction holds where it was.",
        "A calibration runs until you end it. Nothing times out, and quitting waits for an end verified by readback.",
        "Nothing is saved on its own. Saving is a separate step after the end, and yours to confirm.",
    ] {
        ui.label(format!("• {line}"));
    }
    ui.add_space(6.);
    ui.strong("To prepare");
    for line in [
        "Leave the sensor on the headset. The drivers' magnets are calibrated in with it; calibrated alone, it is wrong again once remounted.",
        "Take the headset off and turn it slowly in your hands. A neck reaches too few directions.",
        "Keep a metre from steel furniture, monitor arms, speakers, phones and laptops, or they are calibrated in as if they were the Earth's field.",
    ] {
        ui.label(format!("• {line}"));
    }
}

/// One sweep: its grid, the instruction toward the outlined cell, and how
/// much of it has been read.
fn sweep_view(ui: &mut egui::Ui, snapshot: &coverage::Snapshot, next: Option<coverage::Next>) {
    grid(ui, snapshot, next.map(|next| next.cell));
    ui.add_space(4.);
    if let Some(next) = next {
        ui.label(egui::RichText::new(next.instruction()).size(INSTRUCTION_SIZE));
        ui.small(
            "The marker is the field as the headset sees it, so it moves against the headset. Follow the words to bring it into the outlined cell.",
        );
    } else if snapshot.complete() {
        ui.label("Every direction has been read in this sweep.");
    } else if snapshot.centre.is_none() {
        ui.label(format!(
            "Collecting readings: {} so far. Turn slowly.",
            snapshot.readings
        ));
    }
    ui.label(format!(
        "{} / {CELLS} directions · {} readings",
        snapshot.covered(),
        snapshot.readings
    ));
    ui.small(
        "Coverage is not progress: a full grid says every direction was read, not that the sensor is calibrated.",
    );
}

/// What one sweep's readings say, in their own unit.
fn readings(ui: &mut egui::Ui, snapshot: &coverage::Snapshot, units: Option<Units>) {
    spreads(ui, &[("Spread", snapshot.spread)]);
    match snapshot.centre {
        Some(coverage::Centre::Fitted(point)) => {
            ui.label(format!("Centre offset {:.1}{}", length(point), unit(units)));
        }
        Some(coverage::Centre::Mean(_)) => {
            ui.colored_label(
                ACCENT_DEEP,
                "No centre yet: the readings lie close to one plane. Turn about another axis as well.",
            );
        }
        None => {}
    }
}

/// The sweep after against the sweep before once both allow it, and the sweep
/// after on its own until then.
fn comparison(
    ui: &mut egui::Ui,
    before: &coverage::Snapshot,
    after: &coverage::Snapshot,
    units: Option<Units>,
) {
    if before.readings == 0 {
        readings(ui, after, units);
        ui.small("There was no sweep before this calibration, so this one has nothing to be compared with.");
        return;
    }
    if !(before.complete() && after.complete()) {
        // Sweeps that cover different directions have spreads that do not compare.
        readings(ui, after, units);
        ui.small(format!(
            "Before and after compare once both have read every direction: before {} / {CELLS}, after {} / {CELLS}.",
            before.covered(),
            after.covered()
        ));
        return;
    }
    spreads(
        ui,
        &[
            ("Spread before", before.spread),
            ("Spread after", after.spread),
        ],
    );
    if let (Some(coverage::Centre::Fitted(was)), Some(coverage::Centre::Fitted(is))) =
        (before.centre, after.centre)
    {
        ui.label(format!(
            "Centre offset {:.1} → {:.1}{}",
            length(was),
            length(is),
            unit(units)
        ));
        ui.small(
            "The centres say whether the device's own output changed: an offset that fell means it did. The spreads compare only if it did.",
        );
    }
}

/// The session's own state, below everything else it reports.
fn details(ui: &mut egui::Ui, session: &magnetic::Session) {
    if let Some(error) = &session.type_error {
        ui.small(format!(
            "Units unavailable ({error}): readings are in counts. Directions and spread are unaffected."
        ));
    }
    if session.overrun > 0 {
        ui.small(format!(
            "{} readings were dropped before the player read them.",
            session.overrun
        ));
    }
    let mut parts = vec![format!("{:.1} Hz", session.statistics.actual_rate_hz)];
    if let Some(latest) = session.latest {
        parts.push(format!(
            "latest |B| {:.1}{}",
            length(latest),
            unit(session.units)
        ));
    }
    if let Some(kind) = session.sensor_type {
        parts.push(format!("magnetometer type {kind}"));
    }
    if let Some(calsw) = session.calsw {
        parts.push(format!("CALSW {calsw}"));
    }
    ui.colored_label(MUTED_DEEP, parts.join(" · "));
}

/// A button at its own width. Columns stretch a lone button across them, and
/// the page keeps every action the size of its words.
fn action(ui: &mut egui::Ui, enabled: bool, label: &str) -> bool {
    ui.horizontal(|ui| ui.add_enabled(enabled, egui::Button::new(label)).clicked())
        .inner
}

/// Two sections side by side when there is room, one above the other when not.
fn pair(
    ui: &mut egui::Ui,
    left: (&str, impl FnOnce(&mut egui::Ui)),
    right: (&str, impl FnOnce(&mut egui::Ui)),
) {
    if wide(ui) {
        ui.columns(2, |columns| {
            section(&mut columns[0], left.0, left.1);
            section(&mut columns[1], right.0, right.1);
        });
    } else {
        section(ui, left.0, left.1);
        ui.add_space(12.);
        section(ui, right.0, right.1);
    }
}

/// A section edged in the warning colour, for what must stay in view.
fn banner(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(theme::SURFACE)
        .stroke(egui::Stroke::new(1., theme::WARNING))
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            contents(ui);
        });
    ui.add_space(12.);
}

fn device_name(view: &View) -> &str {
    view.device.as_ref().map_or("No device", |device| {
        if device.name.is_empty() {
            &device.id
        } else {
            &device.name
        }
    })
}

fn unit(units: Option<Units>) -> &'static str {
    match units {
        Some(Units::Microtesla) => " µT",
        Some(Units::Counts) => " counts",
        None => "",
    }
}

fn length(field: coverage::Field) -> f64 {
    field.iter().map(|value| value * value).sum::<f64>().sqrt()
}

/// Minutes and seconds.
fn clock(nanoseconds: u64) -> String {
    let seconds = nanoseconds / 1_000_000_000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Spreads as numbers, toned by the provisional thresholds, each beside its
/// scale.
fn spreads(ui: &mut egui::Ui, rows: &[(&str, Option<f64>)]) {
    // One grid, so the numbers and scales of a before and an after line up.
    egui::Grid::new(rows.first().map_or("", |row| row.0))
        .num_columns(3)
        .show(ui, |ui| {
            for &(label, spread) in rows {
                ui.label(label);
                match spread {
                    Some(spread) => {
                        ui.colored_label(spread_tone(spread), format!("{:.1} %", spread * 100.));
                        spread_scale(ui, spread);
                    }
                    None => {
                        ui.colored_label(MUTED_DEEP, "—");
                    }
                }
                ui.end_row();
            }
        });
}

/// A short scale from nothing to [`SPREAD_SCALE`], filled to `spread`, with
/// both thresholds marked on it.
fn spread_scale(ui: &mut egui::Ui, spread: f64) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(120., 10.), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::ZERO, theme::STAGE);
    painter.rect_filled(
        egui::Rect::from_min_max(
            rect.min,
            egui::pos2(
                rect.left() + rect.width() * fraction(spread / SPREAD_SCALE),
                rect.bottom(),
            ),
        ),
        egui::CornerRadius::ZERO,
        theme::ACCENT_SOFT,
    );
    for threshold in [LOW_SPREAD, HIGH_SPREAD] {
        painter.vline(
            rect.left() + rect.width() * fraction(threshold / SPREAD_SCALE),
            rect.y_range(),
            egui::Stroke::new(1., MUTED_DEEP),
        );
    }
    painter.rect_stroke(
        rect,
        egui::CornerRadius::ZERO,
        egui::Stroke::new(1., theme::BORDER),
        egui::StrokeKind::Inside,
    );
}

fn spread_tone(spread: f64) -> egui::Color32 {
    if spread <= LOW_SPREAD {
        SUCCESS_DEEP
    } else if spread <= HIGH_SPREAD {
        theme::TEXT
    } else {
        ACCENT_DEEP
    }
}

/// A ratio clamped to the unit interval, as a drawing fraction.
#[allow(
    clippy::cast_possible_truncation,
    reason = "Clamped to 0..=1 first, where f32 keeps more precision than a pixel needs"
)]
fn fraction(ratio: f64) -> f32 {
    ratio.clamp(0.0, 1.0) as f32
}

#[allow(clippy::cast_precision_loss, reason = "Grid indices stay below 48")]
fn at(index: usize) -> f32 {
    index as f32
}

/// A cell's fill: the stage's own colour until read, then deepening toward
/// the accent with each reading up to [`FULL_AT`], and no further.
fn fill(held: u32) -> egui::Color32 {
    if held == 0 {
        return theme::STAGE;
    }
    let steps = u16::try_from(held).unwrap_or(u16::MAX).min(FULL_AT);
    theme::ACCENT_SOFT.lerp_to_gamma(theme::ACCENT, f32::from(steps) / f32::from(FULL_AT))
}

/// Where cell `index` is drawn in a grid occupying `area`.
fn cell_rect(area: egui::Rect, index: usize) -> egui::Rect {
    let size = egui::vec2(area.width() / at(COLUMNS), area.height() / at(ROWS));
    egui::Rect::from_min_size(
        area.min + egui::vec2(size.x * at(index % COLUMNS), size.y * at(index / COLUMNS)),
        size,
    )
}

/// Where a [`coverage::place`] is drawn in a grid occupying `area`.
fn marker(area: egui::Rect, place: [f64; 2]) -> egui::Pos2 {
    let [column, row] = place;
    egui::pos2(
        area.left() + area.width() * fraction(column / f64::from(at(COLUMNS))),
        area.top() + area.height() * fraction(row / f64::from(at(ROWS))),
    )
}

/// One sweep's grid: cells filled by readings, the target outlined, and the
/// latest reading marked where it falls.
///
/// Square cells with square corners, as everything in `theme.rs` is. Lines in
/// three weights, as the floor grid: the cells' own, the forward column and
/// the horizon heavier, and the frame in the border colour.
fn grid(ui: &mut egui::Ui, snapshot: &coverage::Snapshot, target: Option<usize>) {
    let cell = ((ui.available_width() - ROW_LABELS) / at(COLUMNS))
        .floor()
        .clamp(12., 40.);
    let size = egui::vec2(
        ROW_LABELS + cell * at(COLUMNS),
        EDGE + cell * at(ROWS) + COLUMN_LABELS,
    );
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let area = egui::Rect::from_min_size(
        egui::pos2(rect.left() + ROW_LABELS, rect.top() + EDGE),
        egui::vec2(cell * at(COLUMNS), cell * at(ROWS)),
    );
    let painter = ui.painter();
    for (index, &held) in snapshot.counts.iter().enumerate() {
        painter.rect_filled(cell_rect(area, index), egui::CornerRadius::ZERO, fill(held));
    }
    let line =
        theme::BORDER.lerp_to_gamma(theme::MUTED, crate::scene3d::params::FLOOR_GRID_CONTRAST);
    for column in (1..COLUMNS).filter(|&column| column != COLUMNS / 2) {
        painter.vline(
            area.left() + cell * at(column),
            area.y_range(),
            egui::Stroke::new(1., line),
        );
    }
    for row in (1..ROWS).filter(|&row| row != ROWS / 2) {
        painter.hline(
            area.x_range(),
            area.top() + cell * at(row),
            egui::Stroke::new(1., line),
        );
    }
    painter.vline(
        area.center().x,
        area.y_range(),
        egui::Stroke::new(1., MUTED_DEEP),
    );
    painter.hline(
        area.x_range(),
        area.center().y,
        egui::Stroke::new(1., MUTED_DEEP),
    );
    painter.rect_stroke(
        area,
        egui::CornerRadius::ZERO,
        egui::Stroke::new(1., theme::BORDER),
        egui::StrokeKind::Outside,
    );
    if let Some(target) = target {
        painter.rect_stroke(
            cell_rect(area, target),
            egui::CornerRadius::ZERO,
            egui::Stroke::new(2., theme::INK),
            egui::StrokeKind::Inside,
        );
    }
    if let Some(place) = snapshot.latest {
        painter.rect(
            egui::Rect::from_center_size(marker(area, place), egui::Vec2::splat(MARKER)),
            egui::CornerRadius::ZERO,
            theme::INK,
            egui::Stroke::new(1., theme::SURFACE),
            egui::StrokeKind::Outside,
        );
    }
    // Both axes are labelled at their lines: rows at the elevations between
    // them, columns at the directions.
    let font = egui::FontId::proportional(LABEL_SIZE);
    for (line, label) in ["+90°", "+30°", "0°", "−30°", "−90°"]
        .into_iter()
        .enumerate()
    {
        painter.text(
            egui::pos2(area.left() - 8., area.top() + cell * at(line)),
            egui::Align2::RIGHT_CENTER,
            label,
            font.clone(),
            MUTED_DEEP,
        );
    }
    let below = area.bottom() + 4.;
    for (column, label, anchor) in [
        (0, "Back", egui::Align2::LEFT_TOP),
        (COLUMNS / 4, "Left", egui::Align2::CENTER_TOP),
        (COLUMNS / 2, "Forward", egui::Align2::CENTER_TOP),
        (COLUMNS * 3 / 4, "Right", egui::Align2::CENTER_TOP),
        (COLUMNS, "Back", egui::Align2::RIGHT_TOP),
    ] {
        painter.text(
            egui::pos2(area.left() + cell * at(column), below),
            anchor,
            label,
            font.clone(),
            MUTED_DEEP,
        );
    }
}

#[cfg(test)]
#[path = "calibration_tests.rs"]
mod tests;
