//! UI issues explicit commands; opening this panel never accesses hardware.
use super::configuration::Settings;
use super::service::{Command, Phase, Service, View, configuration};
use super::{Device, Preferences, Transport, mounting};
use crate::head_tracking::{HeadSnapshot, Quaternion};
use eframe::egui;
use posebridge_core as pb;
use std::time::{Duration, Instant};

/// How long one motion of the mounting check is watched for. Long enough to
/// look up and come back without hurrying, short enough not to be a pose.
const CAPTURE: Duration = Duration::from_secs(4);

fn wide(ui: &egui::Ui) -> bool {
    ui.available_width() >= 800.
}
fn section<R>(ui: &mut egui::Ui, title: &str, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(crate::theme::SURFACE)
        .stroke(egui::Stroke::new(1., crate::theme::BORDER))
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong(title);
            ui.add_space(6.);
            contents(ui)
        })
        .inner
}

/// Calibration belongs to one device configuration and acquisition/reference
/// epoch. A connection can change while this window is closed.
#[derive(Clone, PartialEq, Eq)]
struct CheckSource {
    request: u64,
    device: Device,
    stream: (u64, u64, u64),
}
impl CheckSource {
    fn current(
        head: &HeadSnapshot,
        prefs: &Preferences,
        view: &View,
        enabled: bool,
    ) -> Option<Self> {
        if !enabled
            || view.phase != Phase::Tracking
            || head.status != crate::head_tracking::HeadStatus::BridgeActive
        {
            return None;
        }
        let device = view.device.as_ref()?;
        // Applying a suggestion changes preferences before the running
        // connection adopts them. No new check may mix those two mappings.
        if device != &prefs.device {
            return None;
        }
        let source = &view.snapshot.as_ref()?.descriptor;
        Some(Self {
            request: view.request,
            device: device.clone(),
            stream: (
                source.instance_id,
                source.session_id,
                source.reference_epoch,
            ),
        })
    }
}

/// A mounting check in progress.
#[derive(Default)]
struct Check {
    source: Option<CheckSource>,
    calibration: mounting::Calibration,
    /// The motion being captured, the pose it began from, and when it ends.
    capturing: Option<(mounting::Motion, Quaternion, Instant)>,
    /// The largest turn seen so far in this capture. A motion returns to where
    /// it started, so the end pose says nothing and the peak says everything.
    peak: Option<mounting::Observation>,
    /// What each motion came out as, so a wrong mounting can be shown as what
    /// happened rather than only as a verdict.
    results: [Option<mounting::Observation>; 3],
    /// Why the last motion was not counted, until the next one starts.
    refused: Option<&'static str>,
}
impl Check {
    fn start(&mut self, motion: mounting::Motion, pose: Quaternion) {
        if self.source.is_none() || self.capturing.is_some() {
            return;
        }
        self.calibration.clear_motion(motion);
        self.results[motion.axis()] = None;
        self.capturing = Some((motion, pose, Instant::now() + CAPTURE));
        self.peak = None;
        self.refused = None;
    }
}

#[derive(Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Tracking,
    Device,
    Maintenance,
    Magnetic,
    Diagnostics,
}
#[allow(
    clippy::struct_excessive_bools,
    reason = "window visibility, sensor mode and quit guards are independent state"
)]
pub struct Panel {
    /// Whether the device window is showing. The audio settings keep one row;
    /// everything else lives behind this.
    open: bool,
    tab: Tab,
    draft: Settings,
    baseline: Option<Settings>,
    settings_device: Option<(Transport, String)>,
    prepare_stream: bool,
    prepare_request: Option<u64>,
    confirmation: Option<(Device, pb::DeviceCommand)>,
    check: Check,
    /// The calibration instruction on screen, held against flicker.
    instruction: calibration::Instruction,
    error: Option<String>,
    observation: Option<(String, Option<u64>)>,
    closing: bool,
    allow_close: bool,
}
impl Default for Panel {
    fn default() -> Self {
        Self {
            open: false,
            tab: Tab::Tracking,
            draft: Settings::default(),
            baseline: None,
            settings_device: None,
            prepare_stream: false,
            prepare_request: None,
            confirmation: None,
            check: Check::default(),
            instruction: calibration::Instruction::default(),
            error: None,
            observation: None,
            closing: false,
            allow_close: false,
        }
    }
}
impl Panel {
    fn send(&mut self, service: &Service, command: Command) {
        self.error = service.send(command).err();
    }
    /// Whether the device window is showing, so the coordinator can skip the
    /// settings clone a closed window would never read.
    pub const fn is_open(&self) -> bool {
        self.open
    }
    /// The one row the audio settings keep: what the sensor is doing, the way
    /// into the rest, and the button worth reaching for without opening it.
    ///
    /// Reports whether the listening direction was asked to be recentred.
    pub fn draw_summary(
        &mut self,
        ui: &mut egui::Ui,
        head: &HeadSnapshot,
        service: &Service,
        enabled: bool,
    ) -> bool {
        let view = service.view();
        if !enabled {
            ui.label("Choose software binaural or Windows object output to track a sensor.");
        }
        ui.label(head.status.label());
        // The head status already says what is being heard, so the phase earns
        // a line only where it says something that one cannot: the rate it is
        // arriving at, or work in progress.
        match view.phase {
            Phase::Tracking => {
                if let Some(snapshot) = &view.snapshot {
                    ui.colored_label(
                        phase_tone(view.phase),
                        format!("{:.0} Hz", snapshot.status.actual_rate_hz),
                    );
                }
            }
            Phase::Idle => {}
            phase => {
                ui.colored_label(phase_tone(phase), phase.label());
            }
        }
        let mut recenter = false;
        ui.horizontal(|ui| {
            if ui.button("Device panel…").clicked() {
                self.open = true;
                // The panel can already exist behind the player or minimized.
                // Restore it only on an explicit click, never on every repaint.
                let viewport = egui::ViewportId::from_hash_of("posebridge-device");
                for command in [
                    egui::ViewportCommand::Minimized(false),
                    egui::ViewportCommand::Visible(true),
                    egui::ViewportCommand::Focus,
                ] {
                    ui.ctx().send_viewport_cmd_to(viewport, command);
                }
            }
            recenter = ui
                .add_enabled(
                    matches!(head.status, crate::head_tracking::HeadStatus::BridgeActive),
                    egui::Button::new("Recenter listening direction"),
                )
                .clicked();
        });
        if let Some(error) = self.error.as_ref().or(view.error.as_ref()) {
            ui.colored_label(crate::theme::WARNING, error);
        }
        if view.phase.busy() {
            ui.ctx().request_repaint_after(repaint_interval(view.phase));
        }
        recenter
    }
    /// The device panel, in a window of its own.
    ///
    /// Three tabs inside the audio settings' fixed 390 px had nowhere to go. A
    /// viewport also lets the panel and the listener figure be watched at the
    /// same time, which is what checking a mounting actually takes.
    pub fn draw_window(
        &mut self,
        context: &egui::Context,
        prefs: &mut Preferences,
        head: &HeadSnapshot,
        service: &Service,
        enabled: bool,
    ) -> bool {
        if !self.open {
            return false;
        }
        let mut recenter = false;
        let remains_open = context.show_viewport_immediate(
            egui::ViewportId::from_hash_of("posebridge-device"),
            egui::ViewportBuilder::default()
                .with_title("MacinDecode AC-4 PoseBridge")
                .with_icon(crate::app_icon::load())
                .with_inner_size([1000.0, 700.0])
                .with_min_inner_size([580.0, 440.0]),
            |root, _class| {
                let close_requested = root.ctx().input(|input| input.viewport().close_requested());
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(crate::theme::BACKGROUND)
                            .inner_margin(egui::Margin::same(16)),
                    )
                    .show(root, |ui| {
                        recenter = self.contents(ui, prefs, head, service, enabled);
                    });
                !close_requested
            },
        );
        self.open = remains_open;
        // The window is an immediate viewport, so it is only redrawn when the
        // root is. Ask the root directly rather than rely on a child request
        // reaching it.
        let view = service.view();
        if view.phase.busy() || view.magnetic.is_some() {
            context.request_repaint_after(repaint_interval(view.phase));
        }
        recenter
    }
    fn contents(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        head: &HeadSnapshot,
        service: &Service,
        enabled: bool,
    ) -> bool {
        let view = service.view();
        self.sync_settings(prefs, &view);
        let recenter = self.connection_bar(ui, prefs, head, service, &view, enabled);
        ui.separator();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::Tracking, "Tracking");
            ui.selectable_value(&mut self.tab, Tab::Device, "Device");
            ui.selectable_value(&mut self.tab, Tab::Maintenance, "Maintenance");
            ui.selectable_value(&mut self.tab, Tab::Magnetic, "Magnetic");
            ui.selectable_value(&mut self.tab, Tab::Diagnostics, "Diagnostics");
        });
        self.advance_check(head, prefs, &view, enabled);
        ui.add_space(8.);
        let footer = if self.tab == Tab::Device {
            if view.apply.as_ref().is_some_and(|a| !a.complete) {
                144.
            } else {
                104.
            }
        } else {
            0.
        };
        let height = (ui.available_height() - footer).max(80.);
        egui::ScrollArea::vertical()
            .id_salt(match self.tab {
                Tab::Tracking => "pose-tracking",
                Tab::Device => "pose-device",
                Tab::Maintenance => "pose-maintenance",
                Tab::Magnetic => "pose-magnetic",
                Tab::Diagnostics => "pose-diagnostics",
            })
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| match self.tab {
                Tab::Tracking => {
                    if wide(ui) {
                        ui.columns(2, |columns| {
                            section(&mut columns[0], "Listening direction", |ui| {
                                Self::tracking(ui, prefs, head, service);
                            });
                            section(&mut columns[1], "Mounting check", |ui| {
                                self.mounting_check(ui, prefs, head);
                            });
                        });
                    } else {
                        section(ui, "Listening direction", |ui| {
                            Self::tracking(ui, prefs, head, service);
                        });
                        ui.add_space(12.);
                        section(ui, "Mounting check", |ui| {
                            self.mounting_check(ui, prefs, head);
                        });
                    }
                }
                Tab::Device => self.device(ui, prefs, service, &view),
                Tab::Maintenance => self.maintenance(ui, prefs, &view),
                Tab::Magnetic => self.calibration(ui, prefs, service, &view),
                Tab::Diagnostics => {
                    if wide(ui) {
                        ui.columns(2, |columns| {
                            section(&mut columns[0], "Sensor processing", |ui| {
                                motion_diagnostics(ui, service);
                            });
                            section(&mut columns[1], "Connection details", |ui| {
                                diagnostics(ui, &view, service);
                            });
                        });
                    } else {
                        section(ui, "Sensor processing", |ui| {
                            motion_diagnostics(ui, service);
                        });
                        ui.add_space(12.);
                        section(ui, "Connection details", |ui| {
                            diagnostics(ui, &view, service);
                        });
                    }
                }
            });
        if self.tab == Tab::Device {
            self.settings_actions(ui, prefs, service, &view);
        }
        self.confirm_operation(ui.ctx(), service, &view);
        if view.phase.busy() || view.magnetic.is_some() {
            ui.ctx().request_repaint_after(repaint_interval(view.phase));
        }
        recenter
    }
    fn tracking(
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        head: &HeadSnapshot,
        service: &Service,
    ) {
        let [yaw, pitch, roll] = head.pose.euler();
        ui.horizontal_wrapped(|ui| {
            for text in [
                format!("Yaw {yaw:.1}°"),
                format!("Pitch {pitch:.1}°"),
                format!("Roll {roll:.1}°"),
            ] {
                ui.label(text);
            }
        });
        ui.label(head.status.label());
        ui.add_space(8.);
        ui.checkbox(&mut prefs.enhanced_tracking, "Sensor-side enhancement");
        ui.label(
            service
                .enhancement
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .state
                .label(),
        );
        ui.small("Uses matched timestamps and gyro when reliable.");
        ui.add_space(8.);
        ui.add(egui::Slider::new(&mut prefs.smoothing_ms, 0.0..=50.0).text("Smoothing · ms"));
        ui.add(egui::Slider::new(&mut prefs.max_age_ms, 50..=500).text("Freeze after · ms"));
        ui.small(if prefs.enhanced_tracking { "Expiry uses host age and trusted mapped age. Fixed link and audio delay remain unknown." }
            else { "Age starts at host reception. BLE may deliver samples in batches." });
    }
    /// Check the mounting by moving, rather than by reading three combo boxes
    /// back and hoping.
    ///
    /// Each motion turns about one head axis, so whichever canonical axis the
    /// pose turns about names the entry holding the sensor axis that motion
    /// really used. Three motions name all three entries; see
    /// [`super::mounting`] for why that is the whole derivation.
    fn mounting_check(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences, head: &HeadSnapshot) {
        ui.small(
                    "Which sensor axis points where cannot be read off the three combo boxes. Move, and each motion says which one it used.",
                );
        if self.check.source.is_none() {
            ui.label("Connect or reconnect the selected sensor with these settings to check its mounting.");
            return;
        }
        for motion in mounting::Motion::ALL {
            self.check_row(ui, motion, head);
        }
        if let Some(refused) = self.check.refused {
            ui.colored_label(crate::theme::WARNING, refused);
        }
        self.check_result(ui, prefs);
    }

    /// One motion: the button that starts it, and what it came out as.
    fn check_row(&mut self, ui: &mut egui::Ui, motion: mounting::Motion, head: &HeadSnapshot) {
        let capturing = self
            .check
            .capturing
            .filter(|(current, ..)| *current == motion);
        ui.horizontal(|ui| {
            ui.label(motion.label());
            if let Some((_, _, until)) = capturing {
                if ui.button("Stop").clicked() {
                    self.check.capturing = None;
                    self.check.peak = None;
                }
                let left = until
                    .saturating_duration_since(Instant::now())
                    .as_secs_f32();
                ui.label(match self.check.peak {
                    Some(peak) => format!(
                        "{left:.1} s · {:.0}° about {} · {:.0}% clean",
                        peak.degrees,
                        mounting::Motion::angle_of(peak.axis),
                        peak.purity * 100.0,
                    ),
                    None => format!("{left:.1} s"),
                });
            } else if ui
                .add_enabled(self.check.capturing.is_none(), egui::Button::new("Start"))
                .clicked()
                && let Some(measured) = head.measurement
            {
                self.check.start(motion, measured);
            }
        });
        if capturing.is_some() {
            ui.small(motion.instruction());
            return;
        }
        let Some(result) = self.check.results[motion.axis()] else {
            ui.small(motion.instruction());
            return;
        };
        if !result.usable() {
            ui.small("This motion was not counted. Repeat it.");
            return;
        }
        let angle = mounting::Motion::angle_of(result.axis);
        let correct = result.axis == motion.axis() && result.positive;
        ui.colored_label(
            if correct {
                crate::theme::SUCCESS
            } else {
                crate::theme::WARNING
            },
            if correct {
                format!("Turned {angle}, as it should")
            } else if result.axis == motion.axis() {
                format!("Turned {angle} the other way — was the motion reversed?")
            } else {
                format!("Turned {angle}, not {}", motion.reads_as())
            },
        );
    }

    /// What the motions add up to, and the one place a mounting is offered.
    fn check_result(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences) {
        ui.separator();
        match self.check.calibration.resolve() {
            Ok(resolved) if resolved == prefs.device.mounting => {
                ui.colored_label(
                    crate::theme::SUCCESS,
                    "All three motions agree with this mounting.",
                );
            }
            Ok(resolved) => {
                ui.colored_label(
                    crate::theme::WARNING,
                    format!(
                        "These motions describe {}.",
                        mounting_words(resolved).join(" · ")
                    ),
                );
                if ui.button("Use this mounting").clicked() {
                    prefs.device.mounting = resolved;
                    self.check = Check::default();
                }
                ui.small(
                    "This changes the setting only. Disconnect and connect again for the sensor to be read with it.",
                );
            }
            Err(reason) => {
                ui.small(reason);
            }
        }
    }

    /// Advance a capture, and close it when its window is up.
    ///
    /// Validate the source even when no capture is running: completed results
    /// must not survive a device change, reconnect or pending mounting edit.
    fn advance_check(
        &mut self,
        head: &HeadSnapshot,
        prefs: &Preferences,
        view: &View,
        enabled: bool,
    ) {
        let source = head
            .measurement
            .and_then(|_| CheckSource::current(head, prefs, view, enabled));
        if self.check.source != source {
            let interrupted = self.check.capturing.is_some();
            self.check = Check {
                source,
                refused: interrupted
                    .then_some("Tracking or device settings changed. Repeat all three motions."),
                ..Check::default()
            };
        }
        let Some(source) = &self.check.source else {
            return;
        };
        let mounting = source.device.mounting;
        let Some((motion, start, until)) = self.check.capturing else {
            return;
        };
        let observation = mounting::observe(start, head.measurement.unwrap_or(start));
        if self
            .check
            .peak
            .is_none_or(|peak| observation.degrees > peak.degrees)
        {
            self.check.peak = Some(observation);
        }
        if Instant::now() < until {
            return;
        }
        self.check.capturing = None;
        let Some(peak) = self.check.peak.take() else {
            return;
        };
        self.check.results[motion.axis()] = Some(peak);
        self.check.refused = (!self.check.calibration.record(mounting, motion, peak)).then_some(
            if peak.degrees < mounting::MINIMUM_DEGREES {
                "That was too small to read. Move further and repeat it."
            } else {
                "That turned about more than one axis. Move about one at a time and repeat it."
            },
        );
    }

    /// Runs from [`eframe::App::logic`], including when eframe skips the entire UI pass.
    pub fn guard_close(&mut self, context: &egui::Context, view: &View) {
        let guarded = view.guards_quit();
        if context.input(|i| i.viewport().close_requested()) && guarded && !self.allow_close {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.closing {
                context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                context.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            self.closing = true;
        }
        if !self.closing {
            return;
        }
        if !guarded {
            self.allow_close = true;
            self.closing = false;
            context.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        context.request_repaint_after(Duration::from_millis(100));
    }
    pub fn draw_close_prompt(&mut self, context: &egui::Context, service: &Service) {
        if !self.closing {
            return;
        }
        let view = service.view();
        egui::Window::new("Finish device operation before quitting").collapsible(false).show(context,|ui| {
            if let Some(device)=&view.magnetic {
                ui.label("Magnetic calibration may still be active. End it before quitting; this does not save calibration.");
                let end=Command::Write(device.clone(),pb::DeviceCommand::MagStop);
                if ui.add_enabled(view.admits(&end),egui::Button::new("End calibration and quit")).clicked() {
                    self.send(service,end);
                }
            } else {
                ui.label("Waiting for the device operation. Cancelling does not undo a command already sent.");
                if ui.add_enabled(view.phase!=Phase::Stopping,egui::Button::new("Cancel operation and quit")).clicked() {self.send(service,Command::Stop);}
            }
            if let Some(error)=self.error.as_ref().or(view.error.as_ref()) {ui.label(error);}
            if ui.button("Return to player").clicked() {self.closing=false;}
        });
    }
}
/// A tracking sensor is read as fast as the panel can draw it; a scan or a
/// write only has to look alive.
fn repaint_interval(phase: Phase) -> Duration {
    if phase == Phase::Tracking {
        Duration::from_millis(33)
    } else {
        Duration::from_millis(100)
    }
}
fn phase_tone(phase: Phase) -> egui::Color32 {
    match phase {
        Phase::Failed => crate::theme::WARNING,
        Phase::Tracking => crate::theme::SUCCESS,
        Phase::Idle => crate::theme::MUTED,
        _ => crate::theme::TEXT,
    }
}
/// A resolved mounting in the words its three combo boxes use.
fn mounting_words(mounting: [i8; 3]) -> [String; 3] {
    std::array::from_fn(|index| {
        format!(
            "{} {}",
            mounting::AXIS_ORDER[index],
            axis_name(mounting[index])
        )
    })
}
fn axis_name(axis: i8) -> &'static str {
    match axis {
        -3 => "−Z",
        -2 => "−Y",
        -1 => "−X",
        1 => "+X",
        2 => "+Y",
        3 => "+Z",
        _ => "Choose axis",
    }
}
fn enhanced_profile(rate_register: Option<u16>) -> pb::OutputProfile {
    if pb::FULL_INERTIAL_20HZ_VALIDATED && rate_register == Some(7) {
        pb::OutputProfile::ExperimentalFullInertial20Hz
    } else {
        pb::OutputProfile::TimestampGyroQuaternion
    }
}
fn format_label(format: u16) -> &'static str {
    match format {
        0x61 => "Motion / Euler",
        0x81 => "Timestamp + Euler",
        0x84 => "Timestamp + quaternion",
        0xa4 => "Timestamp + gyro + quaternion",
        0xe4 => "Timestamp + acceleration + gyro + quaternion · 20 Hz",
        _ => "Unknown current format",
    }
}
fn command_label(command: &pb::DeviceCommand) -> &'static str {
    match command {
        pb::DeviceCommand::Algorithm { .. } => "Change fusion algorithm",
        pb::DeviceCommand::ZeroYaw => "Zero device yaw",
        pb::DeviceCommand::AngleReference => "Set device angle reference and save",
        pb::DeviceCommand::AccelCalibrate => "Calibrate accelerometer",
        pb::DeviceCommand::MagStart => "Start magnetic calibration",
        pb::DeviceCommand::MagStop => "End magnetic calibration",
        pb::DeviceCommand::Save => "Save current device settings",
        pb::DeviceCommand::ResetDefaults => "Restore device defaults and save",
        _ => "Apply device settings",
    }
}
fn command_effect(command: &pb::DeviceCommand) -> &'static str {
    match command {
        pb::DeviceCommand::AngleReference => {
            "Sets the sensor's angle reference AND sends SAVE. Reference accuracy and power-cycle persistence require separate verification."
        }
        pb::DeviceCommand::ResetDefaults => {
            "Restores and saves device defaults. This replaces the sensor configuration; readback verifies only known fields."
        }
        pb::DeviceCommand::Save => {
            "Sends SAVE to the selected sensor. A sent command is not proof of power-cycle persistence."
        }
        pb::DeviceCommand::ZeroYaw => {
            "Requires six-axis mode; the player will not switch it automatically. Changes sensor reference without SAVE."
        }
        pb::DeviceCommand::AccelCalibrate => {
            "Keep the sensor level and motionless. Observed start/completion does not verify accuracy. No SAVE is sent."
        }
        pb::DeviceCommand::MagStart => {
            "Starts magnetic calibration in the open session. It runs until you end it: turn the headset through every direction meanwhile. No SAVE is sent."
        }
        pb::DeviceCommand::Algorithm { .. } => {
            "Changes six/nine-axis fusion and may change the reference direction. Verifies readback; no SAVE is sent."
        }
        _ => "Executes this operation on the selected sensor.",
    }
}
fn operation_result(ui: &mut egui::Ui, view: &View) {
    if let Some(s) = &view.snapshot
        && let Some(op) = &s.operation
    {
        ui.separator();
        operation_lines(ui, op);
    }
}
/// One operation in the words every device operation is reported in: sent,
/// readback, completion and persistence, none of which is accuracy.
fn operation_lines(ui: &mut egui::Ui, op: &pb::OperationStatus) {
    ui.label(format!("{} · {:?}", op.action, op.outcome));
    ui.small(format!(
        "Source: {} · Operation {}",
        op.source_id.as_deref().unwrap_or("unknown"),
        op.id
    ));
    ui.label(format!(
        "Sent: {} · Readback: {} · Completion observed: {}",
        op.command_sent, op.register_verified, op.completion_observed
    ));
    ui.label(format!("Persistence: {}", op.persistence));
    if let Some(message) = &op.message {
        ui.label(message);
    }
}
fn motion_diagnostics(ui: &mut egui::Ui, service: &Service) {
    let d = *service
        .enhancement
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ui.label(d.state.label());
    ui.label(format!(
        "Sensor attitude: {}",
        if d.orientation_source.is_empty() {
            "Unavailable"
        } else {
            d.orientation_source
        }
    ));
    if let Some(w) = d.gyro {
        ui.label(format!(
            "Head X/Y/Z gyro: {:.2}, {:.2}, {:.2} °/s",
            w[0].to_degrees(),
            w[1].to_degrees(),
            w[2].to_degrees()
        ));
    }
    if let Some([ax, ay, az]) = d.acceleration {
        ui.label(format!(
            "Head X/Y/Z acceleration: {ax:.3}, {ay:.3}, {az:.3} g"
        ));
    }
    ui.label(format!(
        "Clock ready: {} · drift {} ppm",
        d.clock_ready,
        d.drift_ppm
            .map_or_else(|| "—".into(), |v| format!("{v:.0}"))
    ));
    ui.label(format!(
        "Estimated excess holding: {} ms",
        d.excess_age_ms
            .map_or_else(|| "—".into(), |v| format!("{v:.1}"))
    ));
    ui.label(format!(
        "Prediction: {:.1} ms / {:.2}° · gyro residual {}°",
        d.prediction_ms,
        d.prediction_degrees,
        d.residual_degrees
            .map_or_else(|| "—".into(), |v| format!("{v:.2}"))
    ));
    ui.label(format!(
        "Motion samples: {} · combined for presentation: {} · history overruns: {}",
        d.samples, d.coalesced, d.overruns
    ));
    ui.small(
        "Clock mapping cannot measure the unknown minimum link/fusion delay or audio output delay.",
    );
}
fn diagnostics(ui: &mut egui::Ui, view: &View, service: &Service) {
    if let Some(warning) = &view.timing_warning {
        ui.colored_label(crate::theme::WARNING, warning);
    }
    let sample = service
        .sample
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(p) = sample {
        ui.label(format!(
            "Host reception age: {:.1} ms",
            (p.age + p.queried_at.elapsed()).as_secs_f64() * 1000.
        ));
    }
    if let Some(s) = &view.snapshot {
        ui.label(format!("Acquisition: {:?}", s.status.state));
        ui.label(format!(
            "Samples: {:.1} Hz · {} this session",
            s.status.actual_rate_hz, s.status.session_samples
        ));
        ui.label(format!(
            "Host deliveries: {:.1} Hz · {} total · up to {} frames per delivery",
            view.delivery_hz, s.status.delivery.reads, s.status.delivery.max_frames_per_read
        ));
        ui.label(format!(
            "Reconnects: {} · Invalid poses: {}",
            s.status.reconnect_count, s.status.invalid_poses
        ));
        if let Some(time) = s.pose.as_ref().and_then(|p| p.sample_time) {
            ui.label(format!(
                "Sample clock: {:?} · {} ms · epoch {}",
                time.kind, time.time_ms, time.clock_epoch
            ));
        } else {
            ui.label("Device sample time: unavailable");
        }
        ui.small(
            "Device time is unsynchronized. Host age excludes sensor, USB/BLE and audio delay.",
        );
        ui.label(format!(
            "Configuration observation valid: {} · firmware {}",
            s.descriptor.device.valid,
            s.descriptor
                .device
                .firmware_version
                .as_deref()
                .unwrap_or("unknown")
        ));
        operation_result(ui, view);
    } else {
        ui.label("No device has been accessed.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preparation_keeps_unvalidated_full_frames_out_of_presets() {
        for rate in [None, Some(3), Some(7), Some(9), Some(11)] {
            let selected = enhanced_profile(rate);
            if !pb::FULL_INERTIAL_20HZ_VALIDATED || rate != Some(7) {
                assert!(matches!(
                    selected,
                    pb::OutputProfile::TimestampGyroQuaternion
                ));
            }
        }
    }
    #[test]
    fn hidden_close_is_guarded_until_device_work_and_magnetic_calibration_end() {
        for (minimized, occluded) in [(true, false), (false, true)] {
            let context = egui::Context::default();
            let mut panel = Panel::default();
            let mut input = egui::RawInput::default();
            let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
            viewport.minimized = Some(minimized);
            viewport.occluded = Some(occluded);
            viewport.events.push(egui::ViewportEvent::Close);
            let mut view = View {
                phase: Phase::Operating,
                ..View::default()
            };
            // No UI pass runs: only viewport state is available to the close guard.
            let output = context.run_logic(&input, |ctx| panel.guard_close(ctx, &view));
            let commands = &output.viewport_commands[&egui::ViewportId::ROOT];
            assert!(commands.contains(&egui::ViewportCommand::CancelClose));
            assert!(commands.contains(&egui::ViewportCommand::Minimized(false)));
            assert!(commands.contains(&egui::ViewportCommand::Visible(true)));
            assert!(commands.contains(&egui::ViewportCommand::Focus));
            assert!(panel.closing);

            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .events
                .clear();
            // Completing/cancelling a write is insufficient if magnetic calibration persists.
            view.phase = Phase::Idle;
            view.magnetic = Some(Device::default());
            let output = context.run_logic(&input, |ctx| panel.guard_close(ctx, &view));
            assert!(output.viewport_commands.is_empty());
            assert!(panel.closing);

            view.phase = Phase::Stopping;
            view.magnetic = None;
            let output = context.run_logic(&input, |ctx| panel.guard_close(ctx, &view));
            assert!(output.viewport_commands.is_empty());
            assert!(panel.closing);

            view.phase = Phase::Idle;
            let output = context.run_logic(&input, |ctx| panel.guard_close(ctx, &view));
            assert!(
                output.viewport_commands[&egui::ViewportId::ROOT]
                    .contains(&egui::ViewportCommand::Close)
            );
            assert!(!panel.closing);
        }
    }
    #[test]
    fn session_writes_guard_a_hidden_quit_until_their_outcome_is_published() {
        let context = egui::Context::default();
        let mut panel = Panel::default();
        let mut input = egui::RawInput::default();
        let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
        viewport.minimized = Some(true);
        viewport.events.push(egui::ViewportEvent::Close);
        let mut view = View {
            phase: Phase::Magnetic,
            session_write_pending: true,
            ..View::default()
        };
        // The write is queued, before PoseBridge has an operation to publish.
        let output = context.run_logic(&input, |ctx| panel.guard_close(ctx, &view));
        let commands = &output.viewport_commands[&egui::ViewportId::ROOT];
        assert!(commands.contains(&egui::ViewportCommand::CancelClose));
        assert!(commands.contains(&egui::ViewportCommand::Minimized(false)));
        assert!(panel.closing);
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .clear();
        // A subsequent logic tick must not mistake the session for idle work.
        let output = context.run_logic(&input, |ctx| panel.guard_close(ctx, &view));
        assert!(output.viewport_commands.is_empty());
        assert!(panel.closing);
        // SAVE finished. The session can remain open and read-only while quit
        // proceeds; it must not require a separate Close session click.
        view.session_write_pending = false;
        let output = context.run_logic(&input, |ctx| panel.guard_close(ctx, &view));
        assert!(
            output.viewport_commands[&egui::ViewportId::ROOT]
                .contains(&egui::ViewportCommand::Close)
        );
        assert!(!panel.closing);
    }
    #[test]
    fn idle_or_read_only_session_close_does_not_restore_or_block_a_hidden_window() {
        for phase in [Phase::Idle, Phase::Magnetic] {
            let context = egui::Context::default();
            let mut panel = Panel::default();
            let mut input = egui::RawInput::default();
            let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
            viewport.minimized = Some(true);
            viewport.events.push(egui::ViewportEvent::Close);
            let view = View {
                phase,
                ..View::default()
            };
            let output = context.run_logic(&input, |ctx| panel.guard_close(ctx, &view));
            assert!(output.viewport_commands.is_empty());
            assert!(!panel.closing);
        }
    }
    /// The window this panel now owns sizes itself, so what is worth pinning is
    /// the module's own claim: drawing any tab, repeatedly, issues no command.
    #[test]
    fn drawing_the_device_page_never_accesses_hardware() {
        let context = egui::Context::default();
        let service = Service::new();
        let mut panel = Panel {
            tab: Tab::Device,
            ..Panel::default()
        };
        let mut prefs = Preferences::default();
        let mut height = 0.0;
        for frame in 0..4 {
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000., 760.),
                    )),
                    time: Some(f64::from(frame) * 0.02),
                    ..Default::default()
                },
                |root| {
                    let window = egui::Window::new("Device panel")
                        .default_height(560.)
                        .default_width(520.)
                        .show(root.ctx(), |ui| {
                            if frame == 0 {
                                ui.label("Previous page");
                            } else {
                                panel.contents(
                                    ui,
                                    &mut prefs,
                                    &HeadSnapshot::default(),
                                    &service,
                                    true,
                                );
                            }
                        })
                        .unwrap();
                    height = window.response.rect.height();
                },
            );
            output.textures_delta.clear();
        }
        assert!(height > 0., "the device page drew nothing");
        assert!(service.view().snapshot.is_none());
        assert_eq!(prefs.device.mounting, [0; 3]);
    }
    /// The audio settings keep one row, and a row that reaches a device would
    /// put hardware behind simply opening the settings window.
    #[test]
    fn the_settings_summary_opens_nothing_on_its_own() {
        let context = egui::Context::default();
        let service = Service::new();
        let mut panel = Panel::default();
        assert!(!panel.open);
        let mut recentred = None;
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000., 760.),
                )),
                ..Default::default()
            },
            |root| {
                let window = egui::Window::new("Audio settings").show(root.ctx(), |ui| {
                    panel.draw_summary(ui, &HeadSnapshot::default(), &service, true)
                });
                recentred = window.and_then(|window| window.inner);
            },
        );
        output.textures_delta.clear();
        assert_eq!(
            recentred,
            Some(false),
            "the row drew, and asked for nothing"
        );
        assert!(
            !panel.open,
            "the summary row opened the device window by itself"
        );
        assert!(service.view().snapshot.is_none());
    }
}

#[cfg(test)]
#[path = "ui/mounting_tests.rs"]
mod mounting_tests;

mod calibration;
mod device;
#[cfg(test)]
mod layout_tests;
