//! Controller-owned policy for the macOS-only, silent content-label helper.
use super::OutputPhase;
use macindecode_macinrender::atmos::{Assist, State};

#[derive(Default)]
struct PresentationGate {
    last_position: Option<u64>,
    advanced: bool,
}
impl PresentationGate {
    fn observe(&mut self, position: u64) -> bool {
        self.advanced |= self
            .last_position
            .is_some_and(|previous| position > previous);
        self.last_position = Some(position);
        self.advanced
    }
}

#[derive(Default)]
pub(super) struct AtmosController {
    helper: Option<Assist>,
    gate: PresentationGate,
    device: Option<u32>,
    failure: Option<String>,
}
impl AtmosController {
    #[cfg(test)]
    pub fn failed_for_test(message: &str) -> Self {
        Self {
            failure: Some(message.into()),
            ..Self::default()
        }
    }

    pub fn reset(&mut self) {
        self.helper = None;
        self.gate = PresentationGate::default();
        self.device = None;
        self.failure = None;
    }

    pub fn pause(&mut self) {
        if let Some(helper) = &mut self.helper {
            helper.play(false);
        }
    }

    pub fn update(
        &mut self,
        eligible: bool,
        playing: bool,
        phase: OutputPhase,
        position: u64,
    ) -> String {
        if !eligible {
            self.reset();
            return "Inactive for current settings".into();
        }
        if matches!(
            phase,
            OutputPhase::Idle | OutputPhase::Unavailable | OutputPhase::Ended | OutputPhase::Failed
        ) {
            self.reset();
            return "Inactive".into();
        }
        let advanced = self.gate.observe(position);
        if let Some(helper) = &self.helper {
            match helper.snapshot() {
                Ok(status) => {
                    if self.device.is_some_and(|old| old != status.default_device) {
                        self.reset();
                        self.gate.observe(position);
                        return "Waiting for PCM after output-device change".into();
                    }
                    self.device = Some(status.default_device);
                    if status.state == State::Failed {
                        self.failure = Some(status.error);
                    }
                }
                Err(error) => self.failure = Some(error),
            }
        }
        if let Some(error) = &self.failure {
            return format!("Unavailable: {error}");
        }
        if !playing {
            self.pause();
            return "Paused".into();
        }
        if !advanced {
            return "Waiting for PCM presentation".into();
        }
        if self.helper.is_none() {
            match Assist::new() {
                Ok(helper) => self.helper = Some(helper),
                Err(error) => {
                    self.failure = Some(error.clone());
                    return format!("Unavailable: {error}");
                }
            }
        }
        let helper = self.helper.as_mut().expect("helper created above");
        helper.play(true);
        match helper.snapshot() {
            Ok(status) => format!(
                "{:?} · {} ch · {} frames · {} loops · {} items / {} taps{}",
                status.state,
                status.channels,
                status.frames,
                status.loops,
                status.live_items,
                status.live_taps,
                if status.error.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", status.error)
                }
            ),
            Err(error) => {
                self.failure = Some(error.clone());
                format!("Unavailable: {error}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_for_actual_progress_in_each_epoch_and_survives_a_pause() {
        let mut gate = PresentationGate::default();
        assert!(!gate.observe(96_000));
        assert!(!gate.observe(96_000));
        assert!(gate.observe(97_024));
        assert!(gate.observe(97_024));
        gate = PresentationGate::default();
        assert!(!gate.observe(480_000));
        assert!(gate.observe(481_024));
    }

    #[test]
    fn disabled_and_terminal_states_release_without_starting_a_helper() {
        let mut control = AtmosController {
            failure: Some("injected failure".into()),
            ..AtmosController::default()
        };
        assert_eq!(
            control.update(true, true, OutputPhase::Playing, 10),
            "Unavailable: injected failure"
        );
        assert_eq!(
            control.update(true, true, OutputPhase::Playing, 100),
            "Unavailable: injected failure"
        );
        control.update(false, true, OutputPhase::Playing, 100);
        assert!(control.failure.is_none());
        for phase in [OutputPhase::Idle, OutputPhase::Ended, OutputPhase::Failed] {
            assert_eq!(control.update(true, true, phase, 100), "Inactive");
            assert!(control.helper.is_none());
        }
    }
}
