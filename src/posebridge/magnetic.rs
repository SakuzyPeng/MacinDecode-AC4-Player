//! The player's side of the `PoseBridge` magnetic session: which sweep each
//! reading belongs to, in which unit and axes, and what the panel is shown.
//!
//! `PoseBridge` reads the device's own `0x3A`–`0x3C` output and only scales it by
//! the ratio `0x72` names. Nothing is compensated on the host, here included.
//! The readings are split by the session's phase into the three sweeps
//! `docs/POSEBRIDGE-MAGNETIC.md` compares, and turned into headset axes
//! through the device's mounting, because the grid and every instruction on it
//! are drawn in them.
use super::Device;
use super::coverage::{self, Coverage};
use posebridge_core as pb;

/// The three sweeps a calibration is judged by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sweep {
    /// Monitoring before a calibration.
    Before,
    /// Calibrating: the sweep the guidance steers.
    During,
    /// Monitoring after a verified end.
    After,
}

impl Sweep {
    pub const ALL: [Self; 3] = [Self::Before, Self::During, Self::After];

    /// The sweep a reading taken in `phase` belongs to. Opening, the moments a
    /// command is being verified, saving and closing belong to none: the device
    /// is between states there, and a reading would describe neither side.
    pub const fn of(phase: pb::MagneticPhase) -> Option<Self> {
        match phase {
            pb::MagneticPhase::Monitoring => Some(Self::Before),
            pb::MagneticPhase::Calibrating => Some(Self::During),
            pb::MagneticPhase::Calibrated => Some(Self::After),
            _ => None,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Before => 0,
            Self::During => 1,
            Self::After => 2,
        }
    }
}

/// The unit one session's readings are taken in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Units {
    /// `field_ut`: the register value scaled by the ratio its sensor type names.
    Microtesla,
    /// `register_xyz`, when the sensor type is unknown or unread. Directions
    /// and spread do not care; an offset has no µT to be shown in.
    Counts,
}

impl Units {
    fn of(batch: &pb::MagneticBatch) -> Self {
        if batch.scale_ut_per_count.is_some() {
            Self::Microtesla
        } else {
            Self::Counts
        }
    }

    fn reading(self, sample: &pb::MagneticSample) -> Option<coverage::Field> {
        match self {
            Self::Microtesla => sample.field_ut,
            Self::Counts => Some(sample.register_xyz.map(f64::from)),
        }
    }
}

/// The commands a magnetic session takes. The worker sends them into the open
/// session rather than opening a connection of its own, which the session
/// holds exclusively anyway.
pub const fn in_session(command: &pb::DeviceCommand) -> bool {
    matches!(
        command,
        pb::DeviceCommand::MagStart | pb::DeviceCommand::MagStop | pb::DeviceCommand::Save
    )
}

/// Whether a session can be guided on `device`: the grid and every instruction
/// on it are in headset axes, and a mounting that is no rotation turns them all
/// around.
pub fn check(device: &Device) -> Result<(), String> {
    if coverage::proper(device.mounting) {
        Ok(())
    } else {
        Err("Set the sensor mounting first: calibration guidance is drawn in headset axes.".into())
    }
}

/// One session's readings, split into sweeps.
#[derive(Debug)]
struct Sweeps {
    mounting: [i8; 3],
    units: Option<Units>,
    held: [Coverage; 3],
    /// The sweep the latest binned reading went to, which is what tells a
    /// second calibration from more of the first.
    last: Option<Sweep>,
    latest: Option<coverage::Field>,
}

impl Sweeps {
    fn new(mounting: [i8; 3]) -> Self {
        Self {
            mounting,
            units: None,
            held: std::array::from_fn(|_| Coverage::default()),
            last: None,
            latest: None,
        }
    }

    /// Take in one batch, and report whether any sweep changed.
    fn absorb(&mut self, batch: &pb::MagneticBatch) -> bool {
        let units = Units::of(batch);
        // A new session, or a unit that changed under the old one, shares
        // nothing with what was read before.
        let mut changed = batch.reset || self.units.is_some_and(|held| held != units);
        if changed {
            self.clear();
        }
        self.units = Some(units);
        for sample in &batch.samples {
            if sample.phase == pb::MagneticPhase::ExternalCalibration {
                // Another program is changing the calibration: nothing read so
                // far describes the device that comes out of it.
                self.clear();
                changed = true;
                continue;
            }
            let Some(sweep) = Sweep::of(sample.phase) else {
                continue;
            };
            if sweep == Sweep::During && self.last == Some(Sweep::After) {
                // A second calibration starts from where the first one ended,
                // so what it is judged against is the first one's after.
                self.held[Sweep::Before.index()] =
                    std::mem::take(&mut self.held[Sweep::After.index()]);
                self.held[Sweep::During.index()].clear();
                changed = true;
            }
            self.last = Some(sweep);
            let Some(field) = units
                .reading(sample)
                .and_then(|field| coverage::headset_axes(self.mounting, field))
            else {
                continue;
            };
            changed |= self.held[sweep.index()].push(field);
            self.latest = Some(field);
        }
        changed
    }

    fn clear(&mut self) {
        self.held.iter_mut().for_each(Coverage::clear);
        self.last = None;
        self.latest = None;
    }
}

/// One open session on the device worker: how far its history has been read,
/// its sweeps, and the snapshots last drawn from them.
#[derive(Debug)]
pub struct Run {
    pub device: Device,
    cursor: Option<pb::MagneticCursor>,
    sweeps: Sweeps,
    drawn: [coverage::Snapshot; 3],
    overrun: u64,
    end_verified: bool,
}

impl Run {
    pub fn new(device: Device) -> Result<Self, String> {
        check(&device)?;
        Ok(Self {
            sweeps: Sweeps::new(device.mounting),
            device,
            cursor: None,
            drawn: std::array::from_fn(|_| Coverage::default().snapshot()),
            overrun: 0,
            end_verified: false,
        })
    }

    pub const fn cursor(&self) -> Option<pb::MagneticCursor> {
        self.cursor
    }

    /// Take in the latest batch, and return the session as the panel is to
    /// see it. Sweeps are redrawn only when a reading arrived: `PoseBridge` reads
    /// at 5 Hz, so most polls bring none.
    pub fn absorb(&mut self, batch: pb::MagneticBatch) -> Session {
        self.cursor = batch.cursor;
        self.overrun = self.overrun.saturating_add(batch.history_overrun);
        if self.sweeps.absorb(&batch) {
            self.drawn = Sweep::ALL.map(|sweep| self.sweeps.held[sweep.index()].snapshot());
        }
        if batch.reset {
            self.end_verified = false;
        }
        // The session reports only its latest operation, and the save that
        // usually follows an end would hide whether the end was verified.
        if let Some(operation) = &batch.operation {
            match operation.action.as_str() {
                "mag_start" if operation.write_attempted => self.end_verified = false,
                "mag_stop" => {
                    self.end_verified = operation.register_verified
                        && operation.outcome == pb::OperationOutcome::Succeeded;
                }
                _ => {}
            }
        }
        Session {
            phase: batch.phase,
            active: batch.active,
            units: self.sweeps.units,
            sensor_type: batch.sensor_type,
            type_error: batch.type_error,
            calsw: batch.calsw,
            device_may_be_calibrating: batch.device_may_be_calibrating,
            operation: batch.operation,
            cleanup: batch.cleanup,
            last_error: batch.last_error,
            statistics: batch.statistics,
            sweeps: self.drawn.clone(),
            latest: self.sweeps.latest,
            overrun: self.overrun,
            end_verified: self.end_verified,
        }
    }
}

/// What the panel is shown of a magnetic session.
#[derive(Clone, Debug)]
pub struct Session {
    pub phase: pb::MagneticPhase,
    /// Readings are arriving and fresh.
    pub active: bool,
    /// Unknown until the first batch says.
    pub units: Option<Units>,
    pub sensor_type: Option<u16>,
    pub type_error: Option<String>,
    pub calsw: Option<u16>,
    /// The session's own claim: it wrote a start it has not seen end,
    /// or the device reads CALSW = 7.
    pub device_may_be_calibrating: bool,
    pub operation: Option<pb::OperationStatus>,
    /// What closing the session did about a calibration it had started.
    pub cleanup: Option<pb::OperationStatus>,
    pub last_error: Option<String>,
    /// The session's count, duration and rate for the current window.
    pub statistics: pb::MagneticStatistics,
    /// Snapshots of the sweeps, in [`Sweep::ALL`] order.
    pub sweeps: [coverage::Snapshot; 3],
    /// The latest binned reading, in headset axes.
    pub latest: Option<coverage::Field>,
    /// Readings the session dropped before the player read them.
    pub overrun: u64,
    /// Whether the latest calibration ended through a stop this session sent
    /// and read back, whatever operation came after it.
    pub end_verified: bool,
}

impl Session {
    pub fn sweep(&self, sweep: Sweep) -> &coverage::Snapshot {
        &self.sweeps[sweep.index()]
    }
}

#[cfg(test)]
pub(super) mod fixtures {
    use super::pb;

    /// A batch holding `samples`, in whichever unit `scaled` says, with nothing
    /// else to report.
    pub fn batch(samples: Vec<pb::MagneticSample>, scaled: bool) -> pb::MagneticBatch {
        pb::MagneticBatch {
            schema: 1,
            source_id: "test".into(),
            instance_id: 1,
            session_id: 1,
            cursor: samples.last().map(|sample| sample.cursor),
            reset: false,
            history_overrun: 0,
            active: true,
            phase: samples
                .last()
                .map_or(pb::MagneticPhase::Monitoring, |sample| sample.phase),
            sensor_type: scaled.then_some(6),
            scale_ut_per_count: scaled.then_some(1.0 / 120.0),
            type_error: None,
            calsw: Some(0),
            calsw_age_ns: Some(0),
            device_may_be_calibrating: false,
            latest: samples.last().cloned(),
            samples,
            statistics: pb::MagneticStatistics {
                window_id: 1,
                sample_count: 0,
                elapsed_ns: 0,
                frozen: false,
                actual_rate_hz: 0.0,
                minimum_counts: None,
                maximum_counts: None,
                span_counts: None,
            },
            operation: None,
            cleanup: None,
            last_error: None,
        }
    }

    /// One reading, carried both as µT and as counts.
    pub fn sample(
        sequence: u64,
        phase: pb::MagneticPhase,
        field_ut: [f64; 3],
        register_xyz: [i16; 3],
    ) -> pb::MagneticSample {
        pb::MagneticSample {
            cursor: pb::MagneticCursor {
                instance_id: 1,
                session_id: 1,
                sequence,
            },
            window_id: 1,
            phase,
            register_xyz,
            field_ut: Some(field_ut),
            received_ns: 0,
            age_ns: 0,
            fresh: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{batch, sample};
    use super::*;
    use pb::MagneticPhase::{
        Calibrated, Calibrating, ExternalCalibration, Monitoring, Saving, Starting, Stopping,
    };

    const IDENTITY: [i8; 3] = [1, 2, 3];

    fn device(mounting: [i8; 3]) -> Device {
        Device {
            id: "sensor".into(),
            mounting,
            ..Device::default()
        }
    }

    /// `count` readings in `phase`, numbered on from `first`.
    fn readings(first: u64, count: u64, phase: pb::MagneticPhase) -> Vec<pb::MagneticSample> {
        (first..first + count)
            .map(|sequence| sample(sequence, phase, [30.0, 20.0, -40.0], [3600, 2400, -4800]))
            .collect()
    }

    fn held(session: &Session) -> [usize; 3] {
        Sweep::ALL.map(|sweep| session.sweep(sweep).readings)
    }

    #[test]
    fn each_reading_goes_to_the_sweep_of_its_phase() {
        let mut run = Run::new(device(IDENTITY)).unwrap();
        let mut samples = readings(1, 3, Monitoring);
        samples.extend(readings(4, 2, Starting));
        samples.extend(readings(6, 4, Calibrating));
        samples.extend(readings(10, 1, Stopping));
        samples.extend(readings(11, 5, Calibrated));
        samples.extend(readings(16, 2, Saving));
        let session = run.absorb(batch(samples, true));
        // Verifying a command and saving belong to no sweep.
        assert_eq!(held(&session), [3, 4, 5]);
        assert_eq!(run.cursor().map(|cursor| cursor.sequence), Some(17));
    }

    #[test]
    fn a_second_calibration_is_judged_against_the_first_ones_after() {
        let mut run = Run::new(device(IDENTITY)).unwrap();
        let mut samples = readings(1, 2, Monitoring);
        samples.extend(readings(3, 3, Calibrating));
        samples.extend(readings(6, 4, Calibrated));
        run.absorb(batch(samples, true));
        let session = run.absorb(batch(readings(10, 1, Calibrating), true));
        assert_eq!(held(&session), [4, 1, 0]);
    }

    #[test]
    fn an_external_calibration_discards_every_sweep() {
        let mut run = Run::new(device(IDENTITY)).unwrap();
        let mut samples = readings(1, 3, Monitoring);
        samples.extend(readings(4, 2, ExternalCalibration));
        samples.extend(readings(6, 2, Monitoring));
        let session = run.absorb(batch(samples, true));
        assert_eq!(held(&session), [2, 0, 0]);
    }

    #[test]
    fn a_new_session_or_a_new_unit_starts_over() {
        let mut run = Run::new(device(IDENTITY)).unwrap();
        run.absorb(batch(readings(1, 3, Monitoring), true));
        let mut restarted = batch(readings(1, 1, Monitoring), true);
        restarted.reset = true;
        assert_eq!(held(&run.absorb(restarted)), [1, 0, 0]);
        let counted = run.absorb(batch(readings(2, 2, Monitoring), false));
        assert_eq!(held(&counted), [2, 0, 0]);
        assert_eq!(counted.units, Some(Units::Counts));
    }

    #[test]
    fn counts_stand_in_only_when_no_scale_is_known() {
        let mut scaled = Run::new(device(IDENTITY)).unwrap();
        let session = scaled.absorb(batch(readings(1, 1, Monitoring), true));
        assert_eq!(session.units, Some(Units::Microtesla));
        assert_eq!(session.latest, Some([30.0, 20.0, -40.0]));
        let mut counted = Run::new(device(IDENTITY)).unwrap();
        let session = counted.absorb(batch(readings(1, 1, Monitoring), false));
        assert_eq!(session.units, Some(Units::Counts));
        assert_eq!(session.latest, Some([3600.0, 2400.0, -4800.0]));
    }

    /// The session hands over sensor axes; the grid is drawn in headset axes.
    #[test]
    fn readings_are_turned_into_headset_axes() {
        // The mounting the manual records: right is sensor −Y, forward sensor +X.
        let mut run = Run::new(device([-2, 1, 3])).unwrap();
        let along_sensor_x = sample(1, Monitoring, [40.0, 0.0, 0.0], [0; 3]);
        let session = run.absorb(batch(vec![along_sensor_x], true));
        assert_eq!(session.latest, Some([0.0, 40.0, 0.0]));
    }

    #[test]
    fn a_mounting_that_is_no_rotation_opens_nothing() {
        for mounting in [[0, 0, 0], [2, 1, 3], [1, 1, 3]] {
            assert!(Run::new(device(mounting)).is_err(), "{mounting:?}");
            assert!(check(&device(mounting)).is_err());
        }
        assert!(check(&device(IDENTITY)).is_ok());
    }

    #[test]
    fn sweeps_are_redrawn_only_when_a_reading_arrives() {
        let mut sweeps = Sweeps::new(IDENTITY);
        assert!(sweeps.absorb(&batch(readings(1, 2, Monitoring), true)));
        assert!(!sweeps.absorb(&batch(Vec::new(), true)));
        assert!(!sweeps.absorb(&batch(readings(3, 1, Starting), true)));
    }

    #[test]
    fn only_calibration_commands_enter_a_session() {
        for command in [
            pb::DeviceCommand::MagStart,
            pb::DeviceCommand::MagStop,
            pb::DeviceCommand::Save,
        ] {
            assert!(in_session(&command));
        }
        for command in [
            pb::DeviceCommand::ZeroYaw,
            pb::DeviceCommand::AccelCalibrate,
            pb::DeviceCommand::ResetDefaults,
        ] {
            assert!(!in_session(&command));
        }
    }

    /// A save after the end replaces the session's latest operation; the end
    /// must still read as verified, and a new start must clear it.
    #[test]
    fn a_verified_end_outlives_the_save_after_it() {
        let operation = |action: &str, verified: bool| {
            let mut status = pb::OperationStatus::new(action.into());
            status.write_attempted = true;
            status.command_sent = true;
            status.register_verified = verified;
            status.outcome = if verified || action == "save" {
                pb::OperationOutcome::Succeeded
            } else {
                pb::OperationOutcome::Failed
            };
            status
        };
        let mut run = Run::new(device(IDENTITY)).unwrap();
        let report = |run: &mut Run, status: Option<pb::OperationStatus>| {
            let mut report = batch(Vec::new(), true);
            report.operation = status;
            run.absorb(report).end_verified
        };
        assert!(!report(&mut run, None));
        assert!(!report(&mut run, Some(operation("mag_start", true))));
        assert!(report(&mut run, Some(operation("mag_stop", true))));
        assert!(report(&mut run, Some(operation("save", false))));
        assert!(report(&mut run, None));
        // A start that was refused before writing leaves the last end standing.
        let mut refused = operation("mag_start", false);
        refused.write_attempted = false;
        assert!(report(&mut run, Some(refused)));
        assert!(!report(&mut run, Some(operation("mag_start", true))));
        assert!(!report(&mut run, Some(operation("mag_stop", false))));
    }

    /// The panel reads the session's own status from here, so nothing may be
    /// dropped or swapped on the way through.
    #[test]
    fn the_session_status_passes_through_unchanged() {
        let mut run = Run::new(device(IDENTITY)).unwrap();
        let mut report = batch(readings(1, 1, Calibrating), false);
        report.active = false;
        report.type_error = Some("unknown magnetic sensor type 9".into());
        report.calsw = Some(7);
        report.device_may_be_calibrating = true;
        report.operation = Some(pb::OperationStatus::new("mag_start".into()));
        report.cleanup = Some(pb::OperationStatus::new("mag_stop_cleanup".into()));
        report.last_error = Some("magnetic read timed out".into());
        report.statistics.sample_count = 5;
        report.history_overrun = 3;
        let session = run.absorb(report);
        assert_eq!(session.phase, Calibrating);
        assert!(!session.active);
        assert_eq!(session.sensor_type, None);
        assert_eq!(
            session.type_error.as_deref(),
            Some("unknown magnetic sensor type 9")
        );
        assert_eq!(session.calsw, Some(7));
        assert!(session.device_may_be_calibrating);
        assert_eq!(session.operation.unwrap().action, "mag_start");
        assert_eq!(session.cleanup.unwrap().action, "mag_stop_cleanup");
        assert_eq!(
            session.last_error.as_deref(),
            Some("magnetic read timed out")
        );
        assert_eq!(session.statistics.sample_count, 5);
        assert_eq!(session.overrun, 3);
        // Dropped readings accumulate over the session.
        let mut later = batch(readings(2, 1, Calibrating), false);
        later.history_overrun = 2;
        assert_eq!(run.absorb(later).overrun, 5);
    }
}
