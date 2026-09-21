//! One explicit settings submission; each device write remains individually verified.
use posebridge_core as pb;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub rate: u32,
    pub format: u16,
    pub six_axis: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            rate: 20,
            format: 0xa4,
            six_axis: false,
        }
    }
}
impl Settings {
    pub fn observed(observation: &pb::DeviceObservation) -> Option<Self> {
        if !observation.valid {
            return None;
        }
        let rate = match observation.rate_register? {
            3 => 1,
            4 => 2,
            5 => 5,
            6 => 10,
            7 => 20,
            8 => 50,
            9 => 100,
            11 => 200,
            _ => return None,
        };
        let settings = Self {
            rate,
            format: observation.output_register?,
            six_axis: observation.algorithm? == pb::AlgorithmMode::SixAxis,
        };
        settings.profile().ok()?;
        Some(settings)
    }
    pub fn profile(self) -> Result<pb::OutputProfile, String> {
        match self.format {
            0x61 => Ok(pb::OutputProfile::Motion),
            0x81 => Ok(pb::OutputProfile::TimestampEuler),
            0x84 => Ok(pb::OutputProfile::TimestampQuaternion),
            0xa4 => Ok(pb::OutputProfile::TimestampGyroQuaternion),
            0xe4 => Ok(pb::OutputProfile::ExperimentalFullInertial20Hz),
            _ => Err("Unsupported output format".into()),
        }
    }
    pub fn validate(self) -> Result<(), String> {
        self.profile()?;
        if ![1, 2, 5, 10, 20, 50, 100, 200].contains(&self.rate) {
            return Err("Unsupported sample rate".into());
        }
        if self.format == 0xe4 && self.rate != 20 {
            return Err("Full inertial output requires 20 Hz".into());
        }
        Ok(())
    }
    pub fn differences(self, other: Self) -> usize {
        usize::from(self.rate != other.rate)
            + usize::from(self.format != other.format)
            + usize::from(self.six_axis != other.six_axis)
    }
    fn writes(self, current: Self) -> Result<VecDeque<(&'static str, pb::DeviceCommand)>, String> {
        self.validate()?;
        let mut commands = VecDeque::new();
        let rate = ("Sample rate", pb::DeviceCommand::Rate { hz: self.rate });
        // Enter 20 Hz before E4; leave E4 before changing away from 20 Hz.
        if self.format == 0xe4 && self.rate != current.rate {
            commands.push_back(rate.clone());
        }
        if self.format != current.format {
            commands.push_back((
                "Output format",
                pb::DeviceCommand::Output {
                    format: self.profile()?,
                },
            ));
        }
        if self.format != 0xe4 && self.rate != current.rate {
            commands.push_back(rate);
        }
        if self.six_axis != current.six_axis {
            commands.push_back((
                "Fusion mode",
                pb::DeviceCommand::Algorithm {
                    mode: if self.six_axis {
                        pb::AlgorithmMode::SixAxis
                    } else {
                        pb::AlgorithmMode::NineAxis
                    },
                },
            ));
        }
        Ok(commands)
    }
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub target: Settings,
    pub verified: Vec<&'static str>,
    pub step: &'static str,
    pub complete: bool,
    pub error: Option<String>,
}
pub enum Action {
    Write(pb::DeviceCommand),
    Inspect,
    Done,
}
enum Stage {
    Before,
    Write,
    After,
}
pub struct Apply {
    pub progress: Progress,
    expected: Settings,
    stage: Stage,
    pending: VecDeque<(&'static str, pb::DeviceCommand)>,
}
impl Apply {
    pub fn new(expected: Settings, target: Settings) -> Result<Self, String> {
        target.validate()?;
        Ok(Self {
            expected,
            progress: Progress {
                target,
                verified: vec![],
                step: "Reading current settings",
                complete: false,
                error: None,
            },
            stage: Stage::Before,
            pending: VecDeque::new(),
        })
    }
    pub fn advance(&mut self, snapshot: &pb::Snapshot) -> Result<Action, String> {
        if let Some(error) = &self.progress.error {
            return Err(error.clone());
        }
        match self.stage {
            Stage::Before => {
                if snapshot.descriptor.device.calsw == Some(7) {
                    return Err("End magnetic calibration before applying settings".into());
                }
                let current = Settings::observed(&snapshot.descriptor.device)
                    .ok_or("Cannot read the current device settings; no changes applied")?;
                if current != self.expected {
                    return Err("Device settings changed since the last read. Review the highlighted changes and apply again.".into());
                }
                self.pending = self.progress.target.writes(current)?;
                if self.pending.is_empty() {
                    return Ok(self.finish());
                }
            }
            Stage::Write => {
                if !snapshot.operation.as_ref().is_some_and(|op| {
                    op.outcome == pb::OperationOutcome::Succeeded && op.register_verified
                }) {
                    return Err(format!(
                        "{} was not verified; remaining changes were not applied",
                        self.progress.step
                    ));
                }
                self.progress.verified.push(self.progress.step);
            }
            Stage::After => {
                let observed = Settings::observed(&snapshot.descriptor.device)
                    .ok_or("Final settings readback is unavailable")?;
                if observed != self.progress.target {
                    let mut mismatches = vec![];
                    if observed.rate != self.progress.target.rate {
                        mismatches.push("sample rate");
                    }
                    if observed.format != self.progress.target.format {
                        mismatches.push("output format");
                    }
                    if observed.six_axis != self.progress.target.six_axis {
                        mismatches.push("fusion mode");
                    }
                    return Err(format!(
                        "Device readback differs for {}. Changes are not fully applied.",
                        mismatches.join(", ")
                    ));
                }
                return Ok(self.finish());
            }
        }
        if let Some((name, command)) = self.pending.pop_front() {
            self.stage = Stage::Write;
            self.progress.step = name;
            Ok(Action::Write(command))
        } else {
            self.stage = Stage::After;
            self.progress.step = "Verifying all settings";
            Ok(Action::Inspect)
        }
    }
    fn finish(&mut self) -> Action {
        self.progress.complete = true;
        self.progress.step = "All settings verified";
        Action::Done
    }
    pub fn fail(&mut self, error: String) {
        self.pending.clear();
        self.progress.error = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(settings: Settings) -> pb::Snapshot {
        let mut s = pb::Controller::new().unwrap().snapshot();
        s.descriptor.device = pb::DeviceObservation {
            valid: true,
            rate_register: Some(if settings.rate == 20 { 7 } else { 9 }),
            output_register: Some(settings.format),
            algorithm: Some(if settings.six_axis {
                pb::AlgorithmMode::SixAxis
            } else {
                pb::AlgorithmMode::NineAxis
            }),
            ..pb::DeviceObservation::default()
        };
        let mut op = pb::OperationStatus::new("test".into());
        op.outcome = pb::OperationOutcome::Succeeded;
        op.register_verified = true;
        s.operation = Some(op);
        s
    }
    #[test]
    fn full_frame_rate_constraints_determine_write_order() {
        let full = Settings {
            format: 0xe4,
            ..Settings::default()
        };
        let short = Settings {
            rate: 100,
            ..Settings::default()
        };
        let enter = full.writes(short).unwrap();
        assert!(matches!(enter[0].1, pb::DeviceCommand::Rate { hz: 20 }));
        assert!(matches!(enter[1].1, pb::DeviceCommand::Output { .. }));
        let leave = short.writes(full).unwrap();
        assert!(matches!(leave[0].1, pb::DeviceCommand::Output { .. }));
        assert!(matches!(leave[1].1, pb::DeviceCommand::Rate { hz: 100 }));
        assert!(Settings { rate: 100, ..full }.writes(short).is_err());
        assert!(full.writes(full).unwrap().is_empty());
    }
    #[test]
    fn partial_write_and_reconnect_mismatch_are_never_complete() {
        let before = Settings::default();
        let wanted = Settings {
            rate: 100,
            six_axis: true,
            ..before
        };
        let mut apply = Apply::new(before, wanted).unwrap();
        assert!(matches!(
            apply.advance(&snapshot(before)).unwrap(),
            Action::Write(pb::DeviceCommand::Rate { .. })
        ));
        assert!(matches!(
            apply.advance(&snapshot(wanted)).unwrap(),
            Action::Write(pb::DeviceCommand::Algorithm { .. })
        ));
        assert_eq!(apply.progress.verified, ["Sample rate"]);
        assert!(matches!(
            apply.advance(&snapshot(wanted)).unwrap(),
            Action::Inspect
        ));
        assert!(!apply.progress.complete);
        assert!(apply.advance(&snapshot(before)).is_err());
        assert!(!apply.progress.complete);
        assert!(matches!(
            apply.advance(&snapshot(wanted)).unwrap(),
            Action::Done
        ));
        assert!(apply.progress.complete);
        let mut failed = Apply::new(before, wanted).unwrap();
        failed.advance(&snapshot(before)).unwrap();
        let mut unverified = snapshot(wanted);
        unverified.operation.as_mut().unwrap().register_verified = false;
        assert!(failed.advance(&unverified).is_err());
        failed.fail("cancelled".into());
        assert!(failed.pending.is_empty() && !failed.progress.complete);
        assert!(failed.advance(&snapshot(wanted)).is_err());
        let mut stale = Apply::new(before, wanted).unwrap();
        assert!(stale.advance(&snapshot(wanted)).is_err());
        assert!(stale.pending.is_empty());
    }
}
