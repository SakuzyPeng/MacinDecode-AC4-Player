//! Own all blocking lifecycle calls on one device worker, never on egui/audio.
use super::{Device, Input, Sample, Transport};
use posebridge_core as pb;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Scanning,
    Inspecting,
    Tracking,
    Operating,
    Stopping,
    Failed,
}
impl Phase {
    pub fn busy(self) -> bool {
        !matches!(self, Self::Idle | Self::Failed)
    }
}
#[derive(Clone, Default)]
pub struct View {
    pub request: u64,
    pub phase: Phase,
    pub device: Option<Device>,
    pub devices: Vec<pb::DeviceInfo>,
    pub snapshot: Option<pb::Snapshot>,
    pub error: Option<String>,
    pub timing_warning: Option<String>,
    pub delivery_hz: f64,
    /// Retained until a verified stop, including uncertain/cancelled start writes.
    pub magnetic: Option<Device>,
}
pub enum Command {
    Scan(Transport),
    #[cfg(test)]
    Simulate {
        rate: u32,
    },
    Connect(Device),
    Inspect(Device),
    Write(Device, pb::DeviceCommand),
    Stop,
    Shutdown,
}
struct Request {
    id: u64,
    command: Command,
}
pub struct Service {
    sender: mpsc::SyncSender<Request>,
    view: Arc<Mutex<View>>,
    pub sample: Arc<Mutex<Option<Sample>>>,
    listener: Arc<Mutex<Option<Thread>>>,
    join: Option<JoinHandle<()>>,
}
impl Service {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel(8);
        let view = Arc::new(Mutex::new(View::default()));
        let sample = Arc::new(Mutex::new(None));
        let listener = Arc::new(Mutex::new(None::<Thread>));
        let (v, s, l) = (
            Arc::clone(&view),
            Arc::clone(&sample),
            Arc::clone(&listener),
        );
        let join = thread::Builder::new()
            .name("posebridge-device".into())
            .spawn(move || worker(&receiver, &v, &s, &l));
        let join = match join {
            Ok(join) => Some(join),
            Err(error) => {
                let mut v = lock(&view);
                v.phase = Phase::Failed;
                v.error = Some(format!("Cannot start device worker: {error}"));
                None
            }
        };
        Self {
            sender,
            view,
            sample,
            listener,
            join,
        }
    }
    pub fn set_listener(&self, listener: Thread) {
        *lock(&self.listener) = Some(listener);
    }
    pub fn view(&self) -> View {
        lock(&self.view).clone()
    }
    pub fn send(&self, command: Command) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        if needs_bluetooth(&command) && !packaged_host() {
            return Err("Bluetooth access requires the packaged MacinDecode .app with its Bluetooth usage description. USB remains available.".into());
        }
        let mut v = lock(&self.view);
        if v.phase.busy() && !matches!(command, Command::Stop | Command::Shutdown) {
            return Err("Disconnect or finish the current operation first".into());
        }
        if let Some(magnetic) = &v.magnetic {
            let allowed = match &command {
                Command::Write(device, pb::DeviceCommand::MagStop) | Command::Inspect(device) => {
                    device == magnetic
                }
                Command::Stop | Command::Shutdown => true,
                _ => false,
            };
            if !allowed {
                return Err("End magnetic calibration on its original device first".into());
            }
        }
        let phase = match &command {
            Command::Scan(_) => Phase::Scanning,
            #[cfg(test)]
            Command::Simulate { .. } => Phase::Tracking,
            Command::Connect(_) => Phase::Tracking,
            Command::Inspect(_) => Phase::Inspecting,
            Command::Write(_, _) => Phase::Operating,
            Command::Stop | Command::Shutdown => Phase::Stopping,
        };
        let id = v.request.wrapping_add(1);
        let device = match &command {
            Command::Connect(d) | Command::Inspect(d) | Command::Write(d, _) => Some(d.clone()),
            _ => v.device.clone(),
        };
        self.sender
            .try_send(Request { id, command })
            .map_err(|e| format!("Device worker: {e}"))?;
        v.request = id;
        v.phase = phase;
        v.error = None;
        v.device = device;
        drop(v);
        *lock(&self.sample) = None;
        wake(&self.listener);
        Ok(())
    }
    pub fn stop_tracking(&self) {
        if self.view().phase == Phase::Tracking {
            let _ = self.send(Command::Stop);
        }
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        // Bounded controller stop; the receiver continues draining lifecycle commands.
        let _ = self.sender.send(Request {
            id: u64::MAX,
            command: Command::Shutdown,
        });
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
fn wake(listener: &Mutex<Option<Thread>>) {
    if let Some(listener) = lock(listener).as_ref() {
        listener.unpark();
    }
}
pub fn configuration(device: &Device, acquisition: bool) -> Result<pb::Config, String> {
    let [right, forward, up] = device.mounting;
    let mounting = pb::pose::Mounting { right, forward, up };
    if acquisition {
        mounting.validate().map_err(|e| e.to_string())?;
    }
    let config = pb::Config {
        source: match device.transport {
            Transport::Ble => pb::Source::Ble {
                device_id: device.id.clone(),
                connection_mode: if device.throughput {
                    pb::BleConnectionMode::Throughput
                } else {
                    pb::BleConnectionMode::Default
                },
            },
            Transport::Usb => pb::Source::Usb {
                port: device.id.clone(),
                baud: device.baud,
            },
        },
        source_id: None,
        pose_input: if device.input == Input::RegisterQuaternion {
            pb::PoseInput::Quaternion
        } else {
            pb::PoseInput::Euler
        },
        mounting: acquisition.then_some(mounting),
        osc: None,
    };
    config.validate().map_err(|e| e.to_string())?;
    Ok(config)
}
fn observed_input(
    device: &Device,
    observation: &pb::DeviceObservation,
) -> Result<pb::PoseInput, String> {
    if device.input == Input::RegisterQuaternion {
        return Ok(pb::PoseInput::Quaternion);
    }
    if !observation.valid {
        return Err("Device configuration could not be verified".into());
    }
    let fields = observation.output_fields.as_deref().unwrap_or_default();
    if fields.iter().any(|f| f == "quaternion") {
        Ok(pb::PoseInput::StreamQuaternion)
    } else if fields.iter().any(|f| f == "euler") {
        Ok(pb::PoseInput::Euler)
    } else {
        Err("Unsupported output format. Select a supported format while disconnected.".into())
    }
}
fn observe_magnetic(view: &mut View, snapshot: &pb::Snapshot, device: Option<&Device>) {
    let Some(device) = device else {
        return;
    };
    if snapshot.descriptor.device.valid && snapshot.descriptor.device.calsw == Some(7) {
        view.magnetic = Some(device.clone());
    }
    if let Some(op) = &snapshot.operation {
        if op.action == "mag_start" && op.write_attempted {
            view.magnetic = Some(device.clone());
        }
        if op.action == "mag_stop"
            && op.register_verified
            && op.outcome == pb::OperationOutcome::Succeeded
        {
            view.magnetic = None;
        }
    }
}
#[allow(
    clippy::too_many_lines,
    reason = "One worker serializes the complete device lifecycle"
)]
fn worker(
    receiver: &mpsc::Receiver<Request>,
    view: &Mutex<View>,
    sample: &Mutex<Option<Sample>>,
    listener: &Mutex<Option<Thread>>,
) {
    let mut controller = None::<pb::Controller>;
    let mut current = 0;
    let mut phase = Phase::Idle;
    let mut device = None::<Device>;
    let mut pending_connect = None::<Device>;
    let mut last_view = Instant::now();
    let mut next_poll = Instant::now();
    let mut delivery_mark = None::<(Instant, u64, u64)>;
    #[cfg(target_os = "windows")]
    let mut _timer = None::<macindecode_windows_spatial_audio::timing::Resolution>;
    #[cfg(target_os = "windows")]
    let mut timer_attempted = false;
    loop {
        let delay = if phase == Phase::Tracking {
            next_poll.saturating_duration_since(Instant::now())
        } else {
            Duration::from_millis(20)
        };
        match receiver.recv_timeout(delay) {
            Ok(request) => {
                current = request.id;
                if matches!(request.command, Command::Shutdown) {
                    break;
                }
                *lock(sample) = None;
                wake(listener);
                pending_connect = None;
                let result = (|| -> Result<(), String> {
                    if controller.is_none() {
                        controller = Some(pb::Controller::new().map_err(|e| e.to_string())?);
                    }
                    let c = controller.as_mut().expect("created controller");
                    match request.command {
                        #[cfg(test)]
                        Command::Simulate { rate } => {
                            c.set_config(pb::Config {
                                source: pb::Source::Simulate {
                                    pattern: pb::Pattern::Combined,
                                    euler_deg: [0.; 3],
                                    rate_hz: rate,
                                    sample_clock: true,
                                },
                                ..pb::Config::default()
                            })
                            .map_err(|e| e.to_string())?;
                            c.start().map_err(|e| e.to_string())?;
                            device = None;
                            phase = Phase::Tracking;
                        }
                        Command::Stop => {
                            c.stop().map_err(|e| e.to_string())?;
                            phase = Phase::Idle;
                        }
                        Command::Scan(kind) => {
                            c.scan_start(
                                match kind {
                                    Transport::Ble => pb::TransportKind::Ble,
                                    Transport::Usb => pb::TransportKind::Usb,
                                },
                                5,
                            )
                            .map_err(|e| e.to_string())?;
                            phase = Phase::Scanning;
                        }
                        Command::Connect(d) => {
                            c.set_config(configuration(&d, true)?)
                                .map_err(|e| e.to_string())?;
                            c.inspect_start().map_err(|e| e.to_string())?;
                            device = Some(d.clone());
                            pending_connect = Some(d);
                            phase = Phase::Tracking;
                        }
                        Command::Inspect(d) => {
                            c.set_config(configuration(&d, false)?)
                                .map_err(|e| e.to_string())?;
                            c.inspect_start().map_err(|e| e.to_string())?;
                            device = Some(d);
                            phase = Phase::Inspecting;
                        }
                        Command::Write(d, command) => {
                            c.set_config(configuration(&d, false)?)
                                .map_err(|e| e.to_string())?;
                            c.configure_device(command).map_err(|e| e.to_string())?;
                            device = Some(d);
                            phase = Phase::Operating;
                        }
                        Command::Shutdown => unreachable!(),
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    phase = Phase::Failed;
                    pending_connect = None;
                    let mut v = lock(view);
                    if v.request == current {
                        v.error = Some(error);
                    }
                }
                last_view = Instant::now()
                    .checked_sub(Duration::from_secs(1))
                    .unwrap_or_else(Instant::now);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let Some(c) = controller.as_mut() else {
            continue;
        };
        let mut queried_at = Instant::now();
        let mut snapshot = c.snapshot();
        if let Some(d) = pending_connect.clone() {
            if snapshot.status.state == pb::ConnectionState::Complete {
                let result = (|| -> Result<(), String> {
                    if snapshot.descriptor.device.calsw == Some(7) {
                        return Err(
                            "Magnetic calibration is active. End it before tracking.".into()
                        );
                    }
                    let mut config = configuration(&d, true)?;
                    config.pose_input = observed_input(&d, &snapshot.descriptor.device)?;
                    c.set_config(config).map_err(|e| e.to_string())?;
                    c.start().map_err(|e| e.to_string())
                })();
                pending_connect = None;
                if let Err(error) = result {
                    phase = Phase::Failed;
                    let mut v = lock(view);
                    if v.request == current {
                        v.error = Some(error);
                    }
                }
                queried_at = Instant::now();
                snapshot = c.snapshot();
            }
        } else if matches!(
            phase,
            Phase::Scanning | Phase::Inspecting | Phase::Operating
        ) && matches!(
            snapshot.status.state,
            pb::ConnectionState::Complete | pb::ConnectionState::Stopped
        ) {
            phase = Phase::Idle;
        }
        if snapshot.status.state == pb::ConnectionState::Failed {
            phase = Phase::Failed;
            pending_connect = None;
        }
        #[cfg(target_os = "windows")]
        {
            if phase == Phase::Tracking && !timer_attempted {
                timer_attempted = true;
                match macindecode_windows_spatial_audio::timing::Resolution::new() {
                    Ok(value) => {
                        _timer = Some(value);
                        lock(view).timing_warning = None;
                    }
                    Err(error) => {
                        let mut v = lock(view);
                        if v.timing_warning.is_none() {
                            v.timing_warning = Some(error);
                        }
                    }
                }
            } else if phase != Phase::Tracking {
                _timer = None;
                timer_attempted = false;
            }
        }
        let next = if phase == Phase::Tracking && pending_connect.is_none() {
            snapshot.pose.as_ref().map(|p| Sample {
                instance: p.instance_id,
                session: p.session_id,
                reference: p.reference_epoch,
                sequence: p.sequence,
                angles: p.euler_deg.map(bounded_angle),
                fresh: p.fresh,
                age: Duration::from_nanos(p.age_ns),
                queried_at,
            })
        } else {
            None
        };
        let now = Instant::now();
        next_poll += Duration::from_millis(5);
        if next_poll <= now {
            next_poll = now + Duration::from_millis(5);
        }
        // A queued stop supersedes publication immediately, even while stop itself is blocking.
        let mut v = lock(view);
        if v.request != current {
            continue;
        }
        let mut old = lock(sample);
        let changed = old
            .as_ref()
            .map(|p| (p.instance, p.session, p.reference, p.sequence, p.fresh))
            != next
                .as_ref()
                .map(|p| (p.instance, p.session, p.reference, p.sequence, p.fresh));
        *old = next;
        drop(old);
        if changed {
            wake(listener);
        }
        observe_magnetic(&mut v, &snapshot, device.as_ref());
        let reads = snapshot.status.delivery.reads;
        let session = snapshot.status.session_id;
        match delivery_mark {
            Some((at, before, previous)) if previous == session && reads >= before => {
                if now.duration_since(at) >= Duration::from_secs(1) {
                    v.delivery_hz = count_rate(reads - before, now.duration_since(at));
                    delivery_mark = Some((now, reads, session));
                }
            }
            _ => {
                delivery_mark = Some((now, reads, session));
                v.delivery_hz = 0.0;
            }
        }
        if v.phase != phase || last_view.elapsed() >= Duration::from_millis(100) {
            v.phase = phase;
            if let Some(error) = &snapshot.status.last_error {
                v.error = Some(error.clone());
            }
            v.devices = c.devices();
            v.snapshot = Some(snapshot);
            last_view = Instant::now();
        }
    }
    *lock(sample) = None;
    wake(listener);
    if let Some(c) = controller.as_mut() {
        let _ = c.stop();
    }
}
#[allow(
    clippy::cast_possible_truncation,
    reason = "PoseBridge validates bounded degree angles"
)]
fn bounded_angle(value: f64) -> f32 {
    value as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inspection_needs_no_mount_and_acquisition_never_enables_osc() {
        let mut d = Device {
            id: "test".into(),
            ..Device::default()
        };
        assert!(configuration(&d, false).is_ok());
        assert!(configuration(&d, true).is_err());
        d.mounting = [-2, 1, 3];
        let config = configuration(&d, true).unwrap();
        assert!(config.osc.is_none());
        assert!(config.validate_acquisition().is_ok());
    }
    #[test]
    fn input_requires_verified_fields_and_never_invents_a_timestamp() {
        let d = Device::default();
        let mut observation = pb::DeviceObservation::default();
        assert!(observed_input(&d, &observation).is_err());
        observation.valid = true;
        observation.output_fields = Some(vec!["quaternion".into()]);
        assert_eq!(
            observed_input(&d, &observation).unwrap(),
            pb::PoseInput::StreamQuaternion
        );
        observation.output_fields = Some(vec!["euler".into()]);
        assert_eq!(
            observed_input(&d, &observation).unwrap(),
            pb::PoseInput::Euler
        );
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "Display-only short-window delivery rate"
)]
fn count_rate(count: u64, elapsed: Duration) -> f64 {
    count as f64 / elapsed.as_secs_f64()
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    fn until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !condition() {
            assert!(Instant::now() < deadline, "device worker timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn simulator_worker_stops_clears_pose_and_requires_explicit_restart() {
        let service = Service::new();
        assert!(service.view().snapshot.is_none());
        service.send(Command::Simulate { rate: 100 }).unwrap();
        until(|| {
            lock(&service.sample)
                .as_ref()
                .is_some_and(|p| p.fresh && p.sequence >= 3)
        });
        let first = lock(&service.sample).as_ref().unwrap().instance;
        assert!(service.send(Command::Inspect(Device::default())).is_err());
        service.send(Command::Stop).unwrap();
        assert!(lock(&service.sample).is_none());
        until(|| service.view().phase == Phase::Idle);
        thread::sleep(Duration::from_millis(30));
        assert!(lock(&service.sample).is_none());
        service.send(Command::Simulate { rate: 100 }).unwrap();
        until(|| {
            lock(&service.sample)
                .as_ref()
                .is_some_and(|p| p.fresh && p.instance != first)
        });
    }
    #[test]
    fn uncertain_magnetic_start_remains_guarded_until_verified_stop() {
        let controller = pb::Controller::new().unwrap();
        let mut snapshot = controller.snapshot();
        let mut view = View::default();
        let device = Device {
            id: "sensor".into(),
            ..Device::default()
        };
        let mut op = pb::OperationStatus::new("mag_start".into());
        op.write_attempted = true;
        op.outcome = pb::OperationOutcome::Cancelled;
        snapshot.operation = Some(op);
        observe_magnetic(&mut view, &snapshot, Some(&device));
        assert_eq!(view.magnetic, Some(device.clone()));
        let mut op = pb::OperationStatus::new("mag_stop".into());
        op.outcome = pb::OperationOutcome::Failed;
        snapshot.operation = Some(op);
        observe_magnetic(&mut view, &snapshot, Some(&device));
        assert!(view.magnetic.is_some());
        let op = snapshot.operation.as_mut().unwrap();
        op.register_verified = true;
        op.outcome = pb::OperationOutcome::Succeeded;
        observe_magnetic(&mut view, &snapshot, Some(&device));
        assert!(view.magnetic.is_none());
    }
}

#[cfg(test)]
mod hardware_tests {
    use super::*;
    #[test]
    #[ignore = "requires MACINDECODE_POSEBRIDGE_DEVICE JSON; read-only device inspection and acquisition"]
    fn reads_a_selected_device_without_changing_its_configuration() {
        let device: Device = serde_json::from_str(
            &std::env::var("MACINDECODE_POSEBRIDGE_DEVICE").expect("explicit Device JSON"),
        )
        .unwrap();
        let service = Service::new();
        service.send(Command::Connect(device)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let view = service.view();
            assert_ne!(view.phase, Phase::Failed, "{:?}", view.error);
            if lock(&service.sample)
                .as_ref()
                .is_some_and(|p| p.fresh && p.sequence >= 10)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "No fresh device samples: {:?}",
                view.error
            );
            thread::sleep(Duration::from_millis(10));
        }
        let sample = lock(&service.sample).clone().unwrap();
        assert!(sample.angles.iter().all(|v| v.is_finite()));
        assert!(sample.valid_at(Instant::now(), Duration::from_millis(500)));
        let view = service.view();
        let snapshot = view.snapshot.unwrap();
        assert!(snapshot.descriptor.device.valid);
        assert!(!snapshot.operation.as_ref().unwrap().write_attempted);
        println!(
            "READ-ONLY SENSOR: samples={} rate_hz={:.2} max_frames_per_delivery={} timestamp={:?}",
            snapshot.status.session_samples,
            snapshot.status.actual_rate_hz,
            snapshot.status.delivery.max_frames_per_read,
            snapshot.pose.and_then(|p| p.sample_time)
        );
        service.send(Command::Stop).unwrap();
        assert!(lock(&service.sample).is_none());
        let deadline = Instant::now() + Duration::from_secs(8);
        while service.view().phase != Phase::Idle {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(target_os = "macos")]
fn needs_bluetooth(command: &Command) -> bool {
    match command {
        Command::Scan(Transport::Ble) => true,
        Command::Connect(device) | Command::Inspect(device) | Command::Write(device, _) => {
            device.transport == Transport::Ble
        }
        _ => false,
    }
}
#[cfg(target_os = "macos")]
fn packaged_host() -> bool {
    // Development/test executables do not carry the bundle privacy declaration.
    // The OS still owns permission validation for an actual application bundle.
    std::env::current_exe().is_ok_and(|path| {
        path.parent()
            .filter(|p| p.file_name().is_some_and(|n| n == "MacOS"))
            .and_then(std::path::Path::parent)
            .filter(|p| p.file_name().is_some_and(|n| n == "Contents"))
            .and_then(std::path::Path::parent)
            .is_some_and(|p| p.extension().is_some_and(|e| e == "app"))
    })
}
#[cfg(all(test, target_os = "macos"))]
mod host_tests {
    use super::*;
    #[test]
    fn standalone_host_rejects_bluetooth_before_accessing_hardware() {
        assert!(!packaged_host());
        let service = Service::new();
        assert!(
            service
                .send(Command::Scan(Transport::Ble))
                .unwrap_err()
                .contains("packaged")
        );
        assert!(service.view().snapshot.is_none());
        assert_eq!(service.view().phase, Phase::Idle);
    }
}
