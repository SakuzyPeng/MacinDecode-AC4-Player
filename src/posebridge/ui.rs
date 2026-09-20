//! UI issues explicit commands; opening this panel never accesses hardware.
use super::service::{Command, Phase, Service, View, configuration};
use super::{Device, Input, Preferences, Transport};
use crate::head_tracking::HeadSnapshot;
use eframe::egui;
use posebridge_core as pb;
use std::time::Duration;

#[derive(Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Tracking,
    Device,
    Diagnostics,
}
pub struct Panel {
    tab: Tab,
    rate: u32,
    format: u16,
    six_axis: bool,
    confirmation: Option<(Device, pb::DeviceCommand)>,
    error: Option<String>,
    observation: Option<(String, Option<u64>)>,
    closing: bool,
    allow_close: bool,
}
impl Default for Panel {
    fn default() -> Self {
        Self {
            tab: Tab::Tracking,
            rate: 100,
            format: 0x84,
            six_axis: false,
            confirmation: None,
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
    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        head: &HeadSnapshot,
        service: &Service,
        enabled: bool,
    ) -> bool {
        let view = service.view();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::Tracking, "Tracking");
            ui.selectable_value(&mut self.tab, Tab::Device, "Device");
            ui.selectable_value(&mut self.tab, Tab::Diagnostics, "Diagnostics");
        });
        let mut recenter = false;
        egui::ScrollArea::vertical()
            .id_salt(match self.tab { Tab::Tracking => "pose-tracking", Tab::Device => "pose-device", Tab::Diagnostics => "pose-diagnostics" })
            .min_scrolled_height(320.0)
            .max_height(430.0)
            .auto_shrink([false, false])
            .show(ui,|ui| {
            if !enabled { ui.label("Choose software binaural or Windows object output to track a sensor."); }
            if let Some(error)=self.error.as_ref().or(view.error.as_ref()) { ui.colored_label(crate::theme::WARNING,error); }
            match self.tab {
                Tab::Tracking => {
                    let angles=head.pose.euler();
                    ui.label(format!("Yaw {:.1}°   Pitch {:.1}°   Roll {:.1}°",angles[0],angles[1],angles[2]));
                    ui.label(head.status.label());
                    self.connection(ui,prefs,service,&view,enabled);
                    recenter=ui.add_enabled(matches!(head.status,crate::head_tracking::HeadStatus::BridgeActive),
                        egui::Button::new("Recenter listening direction")).clicked();
                    ui.add(egui::Slider::new(&mut prefs.smoothing_ms,0.0..=50.0).text("Smoothing · ms"));
                    ui.add(egui::Slider::new(&mut prefs.max_age_ms,50..=500).text("Freeze after · ms"));
                    ui.small("Age starts at host reception. BLE may deliver several samples together.");
                }
                Tab::Device => self.device(ui,prefs,service,&view,enabled),
                Tab::Diagnostics => diagnostics(ui,&view,service),
            }
            if let Some((device,command))=self.confirmation.clone() {
                ui.separator();
                ui.strong(command_label(&command));
                ui.label(format!("Device: {}",device.id));
                ui.label(command_effect(&command));
                ui.horizontal(|ui| {
                    if ui.add_enabled(!view.phase.busy(),egui::Button::new("Confirm operation")).clicked() {
                        self.send(service,Command::Write(device,command)); self.confirmation=None;
                    }
                    if ui.button("Cancel").clicked() { self.confirmation=None; }
                });
            }
        });
        if view.phase.busy() || view.magnetic.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        recenter
    }
    fn connection(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        service: &Service,
        view: &View,
        enabled: bool,
    ) {
        ui.label(if prefs.device.id.is_empty() {
            "No device selected"
        } else if prefs.device.name.is_empty() {
            prefs.device.id.as_str()
        } else {
            prefs.device.name.as_str()
        });
        ui.label(format!("{:?}", view.phase));
        if view.phase == Phase::Tracking {
            if ui.button("Disconnect").clicked() {
                self.send(service, Command::Stop);
            }
        } else if view.phase.busy() {
            if ui
                .add_enabled(
                    view.phase != Phase::Stopping,
                    egui::Button::new("Cancel current operation"),
                )
                .clicked()
            {
                self.send(service, Command::Stop);
            }
        } else {
            let valid = configuration(&prefs.device, true);
            if ui
                .add_enabled(
                    enabled && view.magnetic.is_none() && valid.is_ok(),
                    egui::Button::new("Connect"),
                )
                .clicked()
            {
                prefs.remember();
                self.send(service, Command::Connect(prefs.device.clone()));
            }
            if let Err(error) = valid {
                ui.small(if prefs.device.id.is_empty() {
                    "Choose a device and its mounting axes on the Device tab."
                } else {
                    error.strip_prefix("invalid argument: ").unwrap_or(&error)
                });
            }
        }
    }
    #[allow(
        clippy::too_many_lines,
        reason = "One device panel groups explicit device actions"
    )]
    fn device(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Preferences,
        service: &Service,
        view: &View,
        enabled: bool,
    ) {
        let editable = !view.phase.busy() && view.magnetic.is_none();
        ui.add_enabled_ui(editable,|ui| {
            let mut transport=prefs.device.transport;
            ui.horizontal(|ui| {
                ui.selectable_value(&mut transport,Transport::Ble,"Bluetooth LE");
                ui.selectable_value(&mut transport,Transport::Usb,"USB serial");
                if transport!=prefs.device.transport { prefs.select(transport,String::new(),String::new()); self.confirmation=None; }
                if ui.button("Scan").clicked() { self.send(service,Command::Scan(transport)); }
            });
            egui::ComboBox::from_id_salt("posebridge-device").selected_text(
                if prefs.device.id.is_empty(){"Choose device"}else{&prefs.device.id}).show_ui(ui,|ui| {
                for d in &view.devices {
                    let kind=if d.transport==pb::TransportKind::Ble{Transport::Ble}else{Transport::Usb};
                    if kind==transport && ui.selectable_label(prefs.device.id==d.id,format!("{} · {}",d.name,d.id)).clicked() {
                        prefs.select(kind,d.id.clone(),d.name.clone()); self.confirmation=None;
                    }
                }
                for d in prefs.remembered.clone().into_iter().filter(|d|d.transport==transport) {
                    if ui.selectable_label(prefs.device.id==d.id,format!("Remembered: {}",d.id)).clicked() {
                        prefs.select(d.transport,d.id,d.name); self.confirmation=None;
                    }
                }
            });
            if transport==Transport::Usb {
                ui.horizontal(|ui| {ui.label("Baud");ui.add(egui::DragValue::new(&mut prefs.device.baud).range(1..=921_600));});
            }
            #[cfg(target_os="windows")]
            if transport==Transport::Ble { ui.checkbox(&mut prefs.device.throughput,"Request higher BLE throughput (Windows 11+)"); }
            ui.label("Sensor axes pointing toward your head's:");
            for (index,label) in ["Right","Forward","Up"].into_iter().enumerate() {
                egui::ComboBox::from_id_salt(("mount",index)).selected_text(format!("{label}: {}",axis_name(prefs.device.mounting[index])))
                    .show_ui(ui,|ui| {for axis in [-3,-2,-1,1,2,3] { ui.selectable_value(&mut prefs.device.mounting[index],axis,axis_name(axis)); }});
            }
            ui.small("Select a right-handed mounting. Check all three directions while wearing the sensor.");
            egui::ComboBox::from_id_salt("posebridge-input").selected_text(if prefs.device.input==Input::Automatic{"Read current stream format"}else{"Poll quaternion registers"})
                .show_ui(ui,|ui| {
                    ui.selectable_value(&mut prefs.device.input,Input::Automatic,"Read current stream format");
                    ui.selectable_value(&mut prefs.device.input,Input::RegisterQuaternion,"Poll quaternion registers (up to 50 requests/s)");
                });
        });
        self.connection(ui, prefs, service, view, enabled);
        if let Some(device) = &view.magnetic {
            ui.colored_label(
                crate::theme::WARNING,
                format!("Magnetic calibration may be active on {}", device.id),
            );
            if ui
                .add_enabled(
                    !view.phase.busy(),
                    egui::Button::new("End magnetic calibration"),
                )
                .clicked()
            {
                self.send(
                    service,
                    Command::Write(device.clone(), pb::DeviceCommand::MagStop),
                );
            }
        }
        ui.separator();
        ui.label("Device controls · disconnect tracking before changing configuration");
        let device_valid = configuration(&prefs.device, false).is_ok();
        ui.add_enabled_ui(editable && device_valid,|ui| {
            if ui.button("Read device configuration").clicked() {self.send(service,Command::Inspect(prefs.device.clone()));}
            if let Some(snapshot)=&view.snapshot && view.device.as_ref().is_some_and(|d| d.id==prefs.device.id && d.transport==prefs.device.transport) {
                let d=&snapshot.descriptor.device;
                if d.valid && self.observation.as_ref()!=Some(&(prefs.device.id.clone(),d.observed_unix_ms)) {
                    self.observation=Some((prefs.device.id.clone(),d.observed_unix_ms));
                    if let Some(rate)=d.rate_register {self.rate=match rate {3=>1,4=>2,5=>5,6=>10,7=>20,8=>50,11=>200,_=>100};}
                    if let Some(format)=d.output_register {self.format=format;}
                    self.six_axis=d.algorithm==Some(pb::AlgorithmMode::SixAxis);
                }
            }
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("sensor-rate").selected_text(format!("{} Hz",self.rate)).show_ui(ui,|ui| {
                    for rate in [1,2,5,10,20,50,100,200] {ui.selectable_value(&mut self.rate,rate,format!("{rate} Hz"));}
                });
                if ui.button("Apply rate").clicked() {self.send(service,Command::Write(prefs.device.clone(),pb::DeviceCommand::Rate{hz:self.rate}));}
            });
            egui::ComboBox::from_id_salt("sensor-format").selected_text(format_label(self.format)).show_ui(ui,|ui| {
                for format in [0x61,0x81,0x84,0xa4] {ui.selectable_value(&mut self.format,format,format_label(format));}
            });
            if ui.button("Apply output format").clicked() {
                let format=match self.format {0x81=>pb::OutputProfile::TimestampEuler,0x84=>pb::OutputProfile::TimestampQuaternion,
                    0xa4=>pb::OutputProfile::TimestampGyroQuaternion,_=>pb::OutputProfile::Motion};
                self.send(service,Command::Write(prefs.device.clone(),pb::DeviceCommand::Output{format}));
            }
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.six_axis,false,"Nine-axis");ui.selectable_value(&mut self.six_axis,true,"Six-axis");
                if ui.button("Apply algorithm").clicked() {self.confirmation=Some((prefs.device.clone(),pb::DeviceCommand::Algorithm{mode:
                    if self.six_axis {pb::AlgorithmMode::SixAxis}else{pb::AlgorithmMode::NineAxis}}));}
            });
            for command in [pb::DeviceCommand::ZeroYaw,pb::DeviceCommand::AngleReference,pb::DeviceCommand::AccelCalibrate,
                pb::DeviceCommand::MagStart,pb::DeviceCommand::Save,pb::DeviceCommand::ResetDefaults] {
                if ui.button(command_label(&command)).clicked() {self.confirmation=Some((prefs.device.clone(),command));}
            }
            ui.small("Listening Recenter only changes the player. Device operations can change the sensor's reference.");
        });
        operation_result(ui, view);
    }
    pub fn guard_close(&mut self, context: &egui::Context, service: &Service) {
        let view = service.view();
        let guarded =
            view.magnetic.is_some() || matches!(view.phase, Phase::Operating | Phase::Stopping);
        if context.input(|i| i.viewport().close_requested()) && guarded && !self.allow_close {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
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
        egui::Window::new("Finish device operation before quitting").collapsible(false).show(context,|ui| {
            if let Some(device)=&view.magnetic {
                ui.label("Magnetic calibration may still be active. End it before quitting; this does not save calibration.");
                if ui.add_enabled(!view.phase.busy(),egui::Button::new("End calibration and quit")).clicked() {
                    self.send(service,Command::Write(device.clone(),pb::DeviceCommand::MagStop));
                }
            } else {
                ui.label("Waiting for the device operation. Cancelling does not undo a command already sent.");
                if ui.add_enabled(view.phase!=Phase::Stopping,egui::Button::new("Cancel operation and quit")).clicked() {self.send(service,Command::Stop);}
            }
            if let Some(error)=self.error.as_ref().or(view.error.as_ref()) {ui.label(error);}
            if ui.button("Return to player").clicked() {self.closing=false;}
        });
        context.request_repaint_after(Duration::from_millis(100));
    }
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
fn format_label(format: u16) -> &'static str {
    match format {
        0x61 => "Motion / Euler",
        0x81 => "Timestamp + Euler",
        0x84 => "Timestamp + quaternion",
        0xa4 => "Timestamp + gyro + quaternion",
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
            "Starts magnetic calibration. Follow the sensor's physical calibration procedure, then explicitly end calibration. No SAVE is sent."
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
    fn device_page_expands_a_short_window_without_accessing_hardware() {
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
                    let window = egui::Window::new("Short audio window")
                        .resizable(false)
                        .default_height(160.)
                        .default_width(390.)
                        .show(root.ctx(), |ui| {
                            if frame == 0 {
                                ui.label("Previous short page");
                            } else {
                                panel.draw(
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
        assert!(height > 330., "device controls remained cramped: {height}");
        assert!(service.view().snapshot.is_none());
        assert_eq!(prefs.device.mounting, [0; 3]);
    }
}
