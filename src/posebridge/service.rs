//! Own all blocking lifecycle calls on one device worker, never on egui/audio.
use super::configuration::{Action as ApplyAction, Apply, Progress as ApplyProgress, Settings};
use super::enhancement::{Diagnostics, Inbox, Motion};
use super::magnetic;
use super::{Device, Input, Sample, Transport};
use crate::head_tracking::Quaternion;
use posebridge_core as pb;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
// Ordinary and enhanced consumers observe the same publication. Always lock
// sample before motion, including cancellation and the control thread.
fn clear_acquisition(sample: &Mutex<Option<Sample>>, motion: &Mutex<Inbox>) {
    let mut sample = lock(sample);
    let mut motion = lock(motion);
    *sample = None;
    *motion = Inbox {
        reset: true,
        ..Inbox::default()
    };
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Scanning,
    Inspecting,
    Tracking,
    Operating,
    /// A magnetic session is open. It holds the device exclusively, so there
    /// is no tracking and no pose for as long as it lasts.
    Magnetic,
    Stopping,
    Failed,
}
impl Phase {
    pub fn busy(self) -> bool {
        !matches!(self, Self::Idle | Self::Failed)
    }
    /// What the phase means to whoever is wearing the sensor. The panel prints
    /// this rather than the variant name: `Inspecting` and `Stopping` name the
    /// worker's state, not what the player is waiting for.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "Not connected",
            Self::Scanning => "Looking for devices…",
            Self::Inspecting => "Reading device configuration…",
            Self::Tracking => "Connected · tracking",
            Self::Operating => "Applying a device operation…",
            Self::Magnetic => "Reading the magnetic field · not tracking",
            Self::Stopping => "Finishing…",
            Self::Failed => "Device operation failed",
        }
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
    /// The open magnetic session, or the last one until something else starts:
    /// what its cleanup did stays readable after it closed.
    pub magnetic_session: Option<magnetic::Session>,
    /// A session write has been queued and has not finished or been refused.
    /// Set by send, before the worker can publish its operation status.
    pub session_write_pending: bool,
    pub apply: Option<ApplyProgress>,
}
impl View {
    /// Why the service would refuse `command` now, if it would. Controls ask
    /// this rather than reading the phase, which cannot tell a calibration
    /// command for the open magnetic session from one for a device that is
    /// busy, and a disabled control shows the reason where it stands.
    pub fn refusal(&self, command: &Command) -> Option<String> {
        admit(self, command).err()
    }
    pub fn admits(&self, command: &Command) -> bool {
        self.refusal(command).is_none()
    }
    pub fn guards_quit(&self) -> bool {
        self.magnetic.is_some()
            || self.session_write_pending
            || matches!(self.phase, Phase::Operating | Phase::Stopping)
    }
}
pub enum Command {
    Scan(Transport),
    #[cfg(test)]
    Simulate {
        rate: u32,
    },
    Connect(Device),
    Inspect(Device),
    /// Open a read-only magnetic session. Starting, ending and saving a
    /// calibration are then ordinary `Write`s to the same device, which the
    /// worker sends into the session.
    Magnetic(Device),
    Write(Device, pb::DeviceCommand),
    Apply(Device, Settings, Settings),
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
    pub motion: Arc<Mutex<Inbox>>,
    pub enhancement: Arc<Mutex<Diagnostics>>,
    listener: Arc<Mutex<Option<Thread>>>,
    join: Option<JoinHandle<()>>,
}
impl Service {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel(8);
        let view = Arc::new(Mutex::new(View::default()));
        let sample = Arc::new(Mutex::new(None));
        let listener = Arc::new(Mutex::new(None::<Thread>));
        let motion = Arc::new(Mutex::new(Inbox::default()));
        let motion_worker = Arc::clone(&motion);
        let enhancement = Arc::new(Mutex::new(Diagnostics::default()));
        let (v, s, l) = (
            Arc::clone(&view),
            Arc::clone(&sample),
            Arc::clone(&listener),
        );
        let join = thread::Builder::new()
            .name("posebridge-device".into())
            .spawn(move || worker(&receiver, &v, &s, &motion_worker, &l));
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
            motion,
            enhancement,
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
        if let Command::Apply(_, _, settings) = &command {
            settings.validate()?;
        }
        if let Command::Magnetic(device) = &command {
            magnetic::check(device)?;
        }
        #[cfg(target_os = "macos")]
        if needs_bluetooth(&command) && !packaged_host() {
            return Err("Bluetooth access requires the packaged MacinDecode .app with its Bluetooth usage description. USB remains available.".into());
        }
        let mut v = lock(&self.view);
        let phase = admit(&v, &command)?;
        let id = v.request.wrapping_add(1);
        let device = match &command {
            Command::Connect(d)
            | Command::Inspect(d)
            | Command::Magnetic(d)
            | Command::Write(d, _)
            | Command::Apply(d, _, _) => Some(d.clone()),
            _ => v.device.clone(),
        };
        let stopping = matches!(command, Command::Stop | Command::Shutdown);
        let session_write = into_session(&v, &command);
        self.sender
            .try_send(Request { id, command })
            .map_err(|e| format!("Device worker: {e}"))?;
        v.request = id;
        v.phase = phase;
        v.error = None;
        v.device = device;
        v.session_write_pending = session_write;
        if !stopping {
            v.apply = None;
        }
        drop(v);
        clear_acquisition(&self.sample, &self.motion);
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
/// Whether `command` may start now, and the phase it leaves the worker in.
fn admit(v: &View, command: &Command) -> Result<Phase, String> {
    if v.session_write_pending && !matches!(command, Command::Stop | Command::Shutdown) {
        return Err("Waiting for the last operation to finish.".into());
    }
    let into_session = into_session(v, command);
    if v.phase.busy() && !into_session && !matches!(command, Command::Stop | Command::Shutdown) {
        return Err("Disconnect or finish the current operation first".into());
    }
    if let Some(magnetic) = &v.magnetic {
        // A latched device can still be ended, looked at, or watched while it
        // is ended; nothing else may reach it, and no other device may start.
        let allowed = match command {
            Command::Write(device, pb::DeviceCommand::MagStop)
            | Command::Inspect(device)
            | Command::Magnetic(device) => device == magnetic,
            Command::Stop | Command::Shutdown => true,
            _ => false,
        };
        if !allowed {
            return Err("End magnetic calibration on its original device first".into());
        }
    }
    Ok(match command {
        Command::Scan(_) => Phase::Scanning,
        #[cfg(test)]
        Command::Simulate { .. } => Phase::Tracking,
        Command::Connect(_) => Phase::Tracking,
        Command::Inspect(_) => Phase::Inspecting,
        Command::Magnetic(_) => Phase::Magnetic,
        Command::Write(_, _) if into_session => Phase::Magnetic,
        Command::Write(_, _) | Command::Apply(_, _, _) => Phase::Operating,
        Command::Stop | Command::Shutdown => Phase::Stopping,
    })
}
/// A calibration command for the device whose magnetic session is open. It
/// goes into that session: a connection of its own would be refused, because
/// the session holds the device.
fn into_session(v: &View, command: &Command) -> bool {
    v.phase == Phase::Magnetic
        && matches!(command, Command::Write(device, operation)
            if v.device.as_ref() == Some(device) && magnetic::in_session(operation))
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
    if observation.output_register == Some(0xe4) && observation.rate_register != Some(7) {
        return Err(
            "Full inertial output requires 20 Hz. Change the format while disconnected.".into(),
        );
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
/// Publish a session, latching its device when the session says it may be
/// calibrating: a start it wrote and has not seen end, or CALSW = 7 found on
/// the device. Only a verified stop unlatches, so a session that stops saying
/// so leaves the latch as it was.
fn publish_session(v: &mut View, session: magnetic::Session, device: &Device, request: u64) {
    if session.device_may_be_calibrating {
        v.magnetic = Some(device.clone());
    }
    // Cleanup can publish after a newer request was queued. Only the request
    // that dispatched the write may release its quit guard.
    if v.request == request {
        v.session_write_pending &= session
            .operation
            .as_ref()
            .is_some_and(|operation| operation.outcome == pb::OperationOutcome::Running);
    }
    v.magnetic_session = Some(session);
}
fn start_acquisition(controller: &mut pb::Controller, config: pb::Config) -> pb::Result<()> {
    // Complete is published before PoseBridge's task handle finishes. Retire the
    // inspection on this device worker before set_config can reject it as Busy.
    controller.stop()?;
    controller.set_config(config)?;
    controller.start()
}
#[allow(
    clippy::too_many_lines,
    reason = "One worker serializes the complete device lifecycle"
)]
fn worker(
    receiver: &mpsc::Receiver<Request>,
    view: &Mutex<View>,
    sample: &Mutex<Option<Sample>>,
    motion: &Mutex<Inbox>,
    listener: &Mutex<Option<Thread>>,
) {
    let mut controller = None::<pb::Controller>;
    let mut current = 0;
    let mut cursor = None;
    let mut latest = None::<Sample>;
    let mut phase = Phase::Idle;
    let mut device = None::<Device>;
    let mut pending_connect = None::<Device>;
    let mut pending_apply = None::<Apply>;
    let mut session = None::<magnetic::Run>;
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
            Ok(Request {
                id,
                command: Command::Write(target, operation),
            }) if phase == Phase::Magnetic
                && session.as_ref().is_some_and(|run| {
                    run.device == target && magnetic::in_session(&operation)
                }) =>
            {
                // The session, its history and its sweeps carry on: only the
                // command is new, and a refusal leaves the session as it was.
                // A failed session must instead be retired below, so recovery
                // starts a fresh operation rather than repeating its failure.
                current = id;
                if let Some(c) = controller.as_mut()
                    && let Err(error) = c.configure_device(operation)
                {
                    let mut v = lock(view);
                    if v.request == current {
                        v.session_write_pending = false;
                        v.error = Some(match error {
                            pb::Error::Busy => "The magnetic session is not ready, or its last operation is still running".into(),
                            other => other.to_string(),
                        });
                    }
                }
            }
            Ok(request) => {
                current = request.id;
                if let Some(mut apply) = pending_apply.take() {
                    apply.fail("Cancelled. Read settings to check any writes already sent.".into());
                    lock(view).apply = Some(apply.progress);
                }
                if matches!(request.command, Command::Shutdown) {
                    break;
                }
                clear_acquisition(sample, motion);
                cursor = None;
                latest = None;
                wake(listener);
                pending_connect = None;
                let result = (|| -> Result<(), String> {
                    if controller.is_none() {
                        controller = Some(pb::Controller::new().map_err(|e| e.to_string())?);
                    }
                    let c = controller.as_mut().expect("created controller");
                    if let Some(mut run) = session.take() {
                        // PoseBridge ends a calibration this session started
                        // while it stops, and what that cleanup did is readable
                        // only until the next configuration resets the session.
                        let stopped = c.stop();
                        let last = run.absorb(c.magnetic_since(run.cursor()));
                        publish_session(&mut lock(view), last, &run.device, current);
                        // A session that already failed has said why; its
                        // stop repeating that is not news. A healthy one whose
                        // cleanup or close failed is.
                        if matches!(request.command, Command::Stop) && phase != Phase::Failed {
                            stopped.map_err(|e| e.to_string())?;
                        }
                    }
                    if !matches!(request.command, Command::Stop) {
                        // Whatever starts now is no longer about that session.
                        lock(view).magnetic_session = None;
                    }
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
                        Command::Magnetic(d) => {
                            let run = magnetic::Run::new(d.clone())?;
                            // Retire a finished inspection's task first, as
                            // acquisition does, before the session claims the
                            // device.
                            c.stop().map_err(|e| e.to_string())?;
                            c.set_config(configuration(&d, false)?)
                                .map_err(|e| e.to_string())?;
                            c.magnetic_start().map_err(|e| e.to_string())?;
                            device = Some(d);
                            session = Some(run);
                            phase = Phase::Magnetic;
                        }
                        Command::Write(d, command) => {
                            c.set_config(configuration(&d, false)?)
                                .map_err(|e| e.to_string())?;
                            c.configure_device(command).map_err(|e| e.to_string())?;
                            device = Some(d);
                            phase = Phase::Operating;
                        }
                        Command::Apply(d, before, settings) => {
                            let apply = Apply::new(before, settings)?;
                            c.stop().map_err(|e| e.to_string())?;
                            c.set_config(configuration(&d, false)?)
                                .map_err(|e| e.to_string())?;
                            c.inspect_start().map_err(|e| e.to_string())?;
                            lock(view).apply = Some(apply.progress.clone());
                            pending_apply = Some(apply);
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
                        v.session_write_pending = false;
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
        let mut snapshot = c.snapshot();
        if let Some(apply) = pending_apply.as_mut() {
            if snapshot.status.state == pb::ConnectionState::Complete {
                let result = (|| -> Result<bool, String> {
                    let action = apply.advance(&snapshot)?;
                    c.stop().map_err(|e| e.to_string())?;
                    match action {
                        ApplyAction::Write(command) => {
                            c.configure_device(command).map_err(|e| e.to_string())?;
                        }
                        ApplyAction::Inspect => c.inspect_start().map_err(|e| e.to_string())?,
                        ApplyAction::Done => return Ok(true),
                    }
                    Ok(false)
                })();
                let done = match result {
                    Ok(done) => {
                        if done {
                            phase = Phase::Idle;
                        }
                        done
                    }
                    Err(error) => {
                        apply.fail(error.clone());
                        lock(view).error = Some(error);
                        phase = Phase::Failed;
                        true
                    }
                };
                lock(view).apply = Some(apply.progress.clone());
                if done {
                    pending_apply = None;
                }
                snapshot = c.snapshot();
            } else if matches!(
                snapshot.status.state,
                pb::ConnectionState::Failed | pb::ConnectionState::Stopped
            ) {
                let error = snapshot.status.last_error.clone().unwrap_or_else(|| {
                    "Device operation stopped; remaining changes were not applied".into()
                });
                apply.fail(error.clone());
                let mut v = lock(view);
                v.apply = Some(apply.progress.clone());
                v.error = Some(error);
                phase = Phase::Failed;
                pending_apply = None;
            }
        } else if let Some(d) = pending_connect.clone() {
            if snapshot.status.state == pb::ConnectionState::Complete {
                let result = (|| -> Result<(), String> {
                    if snapshot.descriptor.device.calsw == Some(7) {
                        return Err(
                            "Magnetic calibration is active. End it before tracking.".into()
                        );
                    }
                    let mut config = configuration(&d, true)?;
                    config.pose_input = observed_input(&d, &snapshot.descriptor.device)?;
                    start_acquisition(c, config).map_err(|e| e.to_string())
                })();
                pending_connect = None;
                if let Err(error) = result {
                    phase = Phase::Failed;
                    let mut v = lock(view);
                    if v.request == current {
                        v.error = Some(error);
                    }
                }
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
        let batch = c.motion_since(cursor);
        cursor = batch.cursor;
        if batch.reset {
            latest = None;
        }
        if let Some(p) = batch.samples.last() {
            latest = Some(Sample {
                instance: p.cursor.instance_id,
                session: p.cursor.session_id,
                reference: p.reference_epoch,
                sequence: p.cursor.sequence,
                angles: p.euler_deg.map(bounded_angle),
                fresh: p.fresh,
                age: Duration::from_nanos(p.age_ns),
                queried_at: batch.queried_at,
            });
        }
        let next = if phase == Phase::Tracking && pending_connect.is_none() {
            latest.clone().map(|mut p| {
                p.fresh = batch.active;
                p
            })
        } else {
            None
        };
        let now = Instant::now();
        next_poll += Duration::from_millis(5);
        if next_poll <= now {
            next_poll = now + Duration::from_millis(5);
        }
        // Read even when publication is skipped below: the sweeps keep what
        // arrived, and the next publication carries it.
        let published = session
            .as_mut()
            .map(|run| run.absorb(c.magnetic_since(run.cursor())));
        // A queued stop supersedes publication immediately, even while stop itself is blocking.
        let mut v = lock(view);
        if v.request != current {
            continue;
        }
        let mut old = lock(sample);
        {
            let mut inbox = lock(motion);
            if batch.reset {
                inbox.samples.clear();
                inbox.reset = true;
            }
            inbox.overrun = inbox.overrun.saturating_add(batch.history_overrun);
            if phase == Phase::Tracking && pending_connect.is_none() {
                for p in batch.samples {
                    let [qx, qy, qz, qw] = p.orientation_xyzw;
                    inbox.push(Motion {
                        instance: p.cursor.instance_id,
                        session: p.cursor.session_id,
                        reference: p.reference_epoch,
                        sequence: p.cursor.sequence,
                        delivery: p.delivery_id,
                        device_ms: p.sample_time.map(|t| t.time_ms),
                        clock_epoch: p.sample_time.map_or(0, |t| t.clock_epoch),
                        clock_kind: p.sample_time.map_or(0, |t| t.kind as u32),
                        received_at: batch
                            .queried_at
                            .checked_sub(Duration::from_nanos(p.age_ns))
                            .unwrap_or(batch.queried_at),
                        physical: Quaternion([qw, qx, qy, qz]),
                        gyro: p.angular_velocity_rad_s,
                        acceleration: p.acceleration_g,
                        profile: p.profile,
                        orientation_source: match p.orientation_source {
                            pb::OrientationSource::NativeQuaternion => "Native quaternion",
                            pb::OrientationSource::ConvertedEuler => "Converted device Euler",
                            pb::OrientationSource::RegisterQuaternion => "Register quaternion",
                            pb::OrientationSource::Simulator => "Simulator",
                        },
                    });
                }
            }
        }
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
        if let (Some(published), Some(run)) = (published, &session) {
            publish_session(&mut v, published, &run.device, current);
        }
        let reads = snapshot.status.delivery.reads;
        let session_id = snapshot.status.session_id;
        match delivery_mark {
            Some((at, before, previous)) if previous == session_id && reads >= before => {
                if now.duration_since(at) >= Duration::from_secs(1) {
                    v.delivery_hz = count_rate(reads - before, now.duration_since(at));
                    delivery_mark = Some((now, reads, session_id));
                }
            }
            _ => {
                delivery_mark = Some((now, reads, session_id));
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
    clear_acquisition(sample, motion);
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
    fn acquisition_handoff_retires_a_still_running_controller_task() {
        let mut controller = pb::Controller::new().unwrap();
        controller.start().unwrap();
        until(|| controller.latest_pose().is_some_and(|p| p.fresh));
        let previous = controller.latest_pose().unwrap().instance_id;
        let config = pb::Config {
            source: pb::Source::Simulate {
                pattern: pb::Pattern::Fixed,
                euler_deg: [45., 10., 20.],
                rate_hz: 100,
                sample_clock: false,
            },
            ..pb::Config::default()
        };
        // Keep the old task alive deterministically to exercise the lifecycle
        // boundary that a completed inspection's snapshot alone cannot guarantee.
        assert!(matches!(
            controller.set_config(config.clone()),
            Err(pb::Error::Busy)
        ));
        start_acquisition(&mut controller, config).unwrap();
        until(|| {
            controller
                .latest_pose()
                .is_some_and(|p| p.fresh && p.instance_id != previous)
        });
        let pose = controller.latest_pose().unwrap();
        for (actual, expected) in pose.euler_deg.into_iter().zip([45., 10., 20.]) {
            assert!((actual - expected).abs() < 1e-6);
        }
        controller.stop().unwrap();
        assert!(!controller.latest_pose().unwrap().fresh);
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
    /// The whole worker path without hardware: a port that is not there fails
    /// the session the way a lost device would, and it must fail readably and
    /// stop cleanly rather than latch anything.
    #[test]
    fn a_session_that_cannot_open_reports_why_and_stops_cleanly() {
        let service = Service::new();
        let device = Device {
            transport: Transport::Usb,
            id: "posebridge-test-no-such-port".into(),
            mounting: [1, 2, 3],
            ..Device::default()
        };
        service.send(Command::Magnetic(device)).unwrap();
        until(|| service.view().phase == Phase::Failed);
        let view = service.view();
        let reason = view.error.clone();
        let session = view
            .magnetic_session
            .expect("a failed session stays readable");
        assert_eq!(session.phase, pb::MagneticPhase::Failed);
        assert!(session.last_error.is_some() && reason.is_some());
        assert!(view.magnetic.is_none(), "nothing was written to latch");
        service.send(Command::Stop).unwrap();
        // Idle, not failed again: the session already said why it failed,
        // and stopping it adds no failure of its own.
        until(|| service.view().phase == Phase::Idle);
        let view = service.view();
        assert_eq!(view.error, reason);
        assert!(
            view.magnetic_session.is_some(),
            "stopping keeps the outcome"
        );
    }

    #[test]
    fn ending_a_failed_session_attempts_a_new_operation_on_the_first_request() {
        let service = Service::new();
        let device = Device {
            transport: Transport::Usb,
            id: "posebridge-recovery-no-such-port".into(),
            mounting: [1, 2, 3],
            ..Device::default()
        };
        service.send(Command::Magnetic(device.clone())).unwrap();
        until(|| service.view().phase == Phase::Failed);
        // Model the retained warning after a connection was lost during
        // calibration. Recovery must try the original device straight away.
        lock(&service.view).magnetic = Some(device.clone());
        service
            .send(Command::Write(device.clone(), pb::DeviceCommand::MagStop))
            .unwrap();
        until(|| service.view().error.is_some());
        let view = service.view();
        assert!(view.magnetic_session.is_none(), "retire the failed session");
        let operation = view
            .snapshot
            .as_ref()
            .unwrap()
            .operation
            .as_ref()
            .expect("a new stop attempt");
        assert_eq!(operation.action, "mag_stop");
        // The port is still absent, so this new attempt cannot verify an end.
        assert_eq!(operation.outcome, pb::OperationOutcome::Failed);
        assert!(!operation.write_attempted);
        assert_eq!(view.magnetic, Some(device));
        assert!(view.guards_quit());
    }
}

#[cfg(test)]
mod magnetic_tests {
    use super::*;
    use pb::DeviceCommand::{MagStart, MagStop, Save, ZeroYaw};

    fn sensor(id: &str) -> Device {
        Device {
            id: id.into(),
            mounting: [1, 2, 3],
            ..Device::default()
        }
    }

    #[test]
    fn calibration_goes_into_the_open_session_and_nothing_else_gets_in() {
        let (device, other) = (sensor("sensor"), sensor("other"));
        let open = View {
            phase: Phase::Magnetic,
            device: Some(device.clone()),
            ..View::default()
        };
        for operation in [MagStart, MagStop, Save] {
            let write = Command::Write(device.clone(), operation);
            assert_eq!(admit(&open, &write), Ok(Phase::Magnetic));
        }
        for refused in [
            Command::Write(device.clone(), ZeroYaw),
            Command::Write(other, MagStart),
            Command::Connect(device.clone()),
            Command::Magnetic(device.clone()),
        ] {
            assert!(admit(&open, &refused).is_err());
        }
        assert_eq!(admit(&open, &Command::Stop), Ok(Phase::Stopping));
        // Without a session the same write is an operation of its own.
        let idle = View::default();
        assert_eq!(
            admit(&idle, &Command::Write(device.clone(), MagStart)),
            Ok(Phase::Operating)
        );
        assert_eq!(
            admit(&idle, &Command::Magnetic(device)),
            Ok(Phase::Magnetic)
        );
    }

    #[test]
    fn queued_session_writes_guard_quit_before_the_worker_can_report_them() {
        for operation in [MagStart, MagStop, Save] {
            let device = Device {
                transport: Transport::Usb,
                ..sensor("sensor")
            };
            let (sender, receiver) = mpsc::sync_channel(8);
            // No worker: keep the command queued deterministically, with no
            // operation snapshot yet and without accessing a real device.
            let service = Service {
                sender,
                view: Arc::new(Mutex::new(View {
                    phase: Phase::Magnetic,
                    device: Some(device.clone()),
                    ..View::default()
                })),
                sample: Arc::default(),
                motion: Arc::default(),
                enhancement: Arc::default(),
                listener: Arc::default(),
                join: None,
            };
            assert!(!service.view().guards_quit(), "monitoring writes nothing");
            service
                .send(Command::Write(device.clone(), operation))
                .unwrap();
            let queued = service.view();
            assert_eq!(queued.phase, Phase::Magnetic);
            assert!(queued.guards_quit());
            assert!(queued.magnetic.is_none() && queued.snapshot.is_none());
            // A second write cannot supersede the guard before dispatch.
            assert!(service.send(Command::Write(device, MagStop)).is_err());
            assert_eq!(service.view().request, queued.request);
            assert!(matches!(
                receiver.try_recv().unwrap().command,
                Command::Write(..)
            ));
            service.send(Command::Stop).unwrap();
            assert!(!service.view().session_write_pending);
            assert!(
                service.view().guards_quit(),
                "closing still waits for cleanup"
            );
        }
    }

    #[test]
    fn a_session_write_guards_until_its_outcome_and_preserves_the_calibration_warning() {
        let device = sensor("sensor");
        for outcome in [
            pb::OperationOutcome::Succeeded,
            pb::OperationOutcome::Unverified,
            pb::OperationOutcome::Failed,
            pb::OperationOutcome::Cancelled,
        ] {
            let mut run = magnetic::Run::new(device.clone()).unwrap();
            let mut view = View {
                request: 1,
                phase: Phase::Magnetic,
                session_write_pending: true,
                ..View::default()
            };
            let mut batch = magnetic::fixtures::batch(Vec::new(), true);
            batch.phase = pb::MagneticPhase::Saving;
            batch.operation = Some(pb::OperationStatus::new("save".into()));
            publish_session(&mut view, run.absorb(batch.clone()), &device, 1);
            assert!(view.guards_quit(), "SAVE is still running");
            batch.operation.as_mut().unwrap().outcome = outcome;
            batch.phase = pb::MagneticPhase::Calibrated;
            publish_session(&mut view, run.absorb(batch.clone()), &device, 0);
            assert!(
                view.guards_quit(),
                "an older request cannot release the write"
            );
            publish_session(&mut view, run.absorb(batch.clone()), &device, 1);
            assert!(!view.guards_quit(), "SAVE finished: {outcome:?}");

            // Finishing a write is not enough when calibration may persist.
            view.session_write_pending = true;
            batch.device_may_be_calibrating = true;
            publish_session(&mut view, run.absorb(batch), &device, 1);
            assert!(!view.session_write_pending);
            assert!(view.guards_quit());
        }
    }

    #[test]
    fn a_latched_device_can_be_watched_and_ended_but_not_restarted() {
        let (device, other) = (sensor("sensor"), sensor("other"));
        let latched = View {
            magnetic: Some(device.clone()),
            ..View::default()
        };
        assert_eq!(
            admit(&latched, &Command::Magnetic(device.clone())),
            Ok(Phase::Magnetic)
        );
        assert!(admit(&latched, &Command::Magnetic(other)).is_err());
        assert!(admit(&latched, &Command::Connect(device.clone())).is_err());
        let watching = View {
            phase: Phase::Magnetic,
            device: Some(device.clone()),
            ..latched
        };
        assert_eq!(
            admit(&watching, &Command::Write(device.clone(), MagStop)),
            Ok(Phase::Magnetic)
        );
        // So the End calibration buttons stay live while the session is open,
        // though the phase alone reads as busy.
        assert!(watching.phase.busy());
        assert!(watching.admits(&Command::Write(device.clone(), MagStop)));
        assert!(admit(&watching, &Command::Write(device.clone(), MagStart)).is_err());
        assert!(admit(&watching, &Command::Write(device, Save)).is_err());
    }

    #[test]
    fn a_session_without_a_mounting_is_refused_before_any_hardware() {
        let service = Service::new();
        let error = service
            .send(Command::Magnetic(Device {
                id: "sensor".into(),
                ..Device::default()
            }))
            .unwrap_err();
        assert!(error.contains("mounting"), "{error}");
        let view = service.view();
        assert_eq!(view.phase, Phase::Idle);
        assert!(view.snapshot.is_none() && view.magnetic_session.is_none());
    }

    #[test]
    fn a_session_that_may_be_calibrating_latches_and_never_unlatches() {
        let device = sensor("sensor");
        let mut run = magnetic::Run::new(device.clone()).unwrap();
        let mut view = View::default();
        let mut calibrating = magnetic::fixtures::batch(Vec::new(), true);
        calibrating.device_may_be_calibrating = true;
        publish_session(&mut view, run.absorb(calibrating), &device, 0);
        assert_eq!(view.magnetic, Some(device.clone()));
        // Only a verified stop unlatches; a session that stops saying so does not.
        let quiet = magnetic::fixtures::batch(Vec::new(), true);
        publish_session(&mut view, run.absorb(quiet), &device, 0);
        assert_eq!(view.magnetic, Some(device));
        assert!(view.magnetic_session.is_some());
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
    #[test]
    #[ignore = "requires MACINDECODE_POSEBRIDGE_DEVICE JSON with its mounting; read-only magnetic session"]
    fn reads_the_magnetic_field_without_writing_to_the_device() {
        let device: Device = serde_json::from_str(
            &std::env::var("MACINDECODE_POSEBRIDGE_DEVICE").expect("explicit Device JSON"),
        )
        .unwrap();
        let service = Service::new();
        service.send(Command::Magnetic(device)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let session = loop {
            let view = service.view();
            assert_ne!(view.phase, Phase::Failed, "{:?}", view.error);
            if let Some(session) = view.magnetic_session
                && session.sweep(magnetic::Sweep::Before).readings >= 25
            {
                break session;
            }
            assert!(
                Instant::now() < deadline,
                "No magnetic readings: {:?}",
                view.error
            );
            thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(session.phase, pb::MagneticPhase::Monitoring);
        assert!(session.operation.is_none(), "monitoring wrote nothing");
        assert!(
            session
                .latest
                .is_some_and(|field| field.iter().all(|value| value.is_finite()))
        );
        println!(
            "READ-ONLY MAGNETIC: units={:?} sensor_type={:?} rate_hz={:.2} calsw={:?} latest={:?}",
            session.units,
            session.sensor_type,
            session.statistics.actual_rate_hz,
            session.calsw,
            session.latest
        );
        service.send(Command::Stop).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while service.view().phase != Phase::Idle {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        assert!(service.view().magnetic.is_none());
    }
}

#[cfg(target_os = "macos")]
fn needs_bluetooth(command: &Command) -> bool {
    match command {
        Command::Scan(Transport::Ble) => true,
        Command::Connect(device)
        | Command::Inspect(device)
        | Command::Magnetic(device)
        | Command::Write(device, _)
        | Command::Apply(device, _, _) => device.transport == Transport::Ble,
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
