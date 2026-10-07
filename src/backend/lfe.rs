//! LFE folding is an output policy; source planes and meters stay independent.
use super::state::{element_state_at, lfe_render_state};
use crate::decoder::DecodedSceneBlock;

const ACTIVE_EPSILON: f32 = 1.0e-12;

fn has_signal(samples: &[f32]) -> bool {
    samples.iter().any(|sample| sample.abs() > ACTIVE_EPSILON)
}

/// Decide once over a render block, not at individual waveform zero crossings.
fn pair_gain(first: &[f32], second: &[f32]) -> f32 {
    if has_signal(first) && has_signal(second) {
        std::f32::consts::FRAC_1_SQRT_2
    } else {
        1.0
    }
}

#[cfg_attr(not(macinrender_output), allow(dead_code))]
pub(super) fn normalization(block: &DecodedSceneBlock, offset: u32) -> f32 {
    if block.lfes().len() < 2 {
        return 1.0;
    }
    let active = block
        .lfes()
        .iter()
        .filter(|lfe| {
            lfe.samples()
                .iter()
                .enumerate()
                .skip(offset as usize)
                .any(|(frame, sample)| {
                    let (active, gain) = lfe_render_state(element_state_at(
                        block,
                        lfe.element_id(),
                        lfe.initial_state(),
                        u32::try_from(frame).unwrap_or(u32::MAX),
                    ));
                    block.state_complete() && active && (sample * gain).abs() > ACTIVE_EPSILON
                })
        })
        .count();
    if active > 1 {
        std::f32::consts::FRAC_1_SQRT_2
    } else {
        1.0
    }
}

/// Resolve the two independent source controls before folding into one bed LFE.
/// This is pure so retrying a backpressured Scene frame cannot advance a ramp.
#[cfg(any(macinrender_output, test))]
pub(super) fn fold(block: &DecodedSceneBlock, offset: u32) -> Vec<f32> {
    let gain = normalization(block, offset);
    (offset..block.duration_frames())
        .map(|frame| {
            block
                .lfes()
                .iter()
                .map(|lfe| {
                    let (active, level) = lfe_render_state(element_state_at(
                        block,
                        lfe.element_id(),
                        lfe.initial_state(),
                        frame,
                    ));
                    if active {
                        lfe.samples()[frame as usize] * level
                    } else {
                        0.0
                    }
                })
                .sum::<f32>()
                * gain
        })
        .collect()
}

#[cfg_attr(not(any(windows_spatial_output, test)), allow(dead_code))]
pub(super) struct Render {
    pub active: bool,
    pub gain: f32,
    pub samples: Vec<f32>,
}

/// The one Windows LFE destination, retaining two input planes for metering.
#[cfg_attr(not(any(windows_spatial_output, test)), allow(dead_code))]
pub(super) struct Quantum {
    pub render: Render,
    initialized: bool,
    inputs: Option<[Vec<f32>; 2]>,
    states: [(bool, f32); 2],
}

#[cfg_attr(not(any(windows_spatial_output, test)), allow(dead_code))]
impl Quantum {
    #[cfg(test)]
    pub fn new(frames: usize) -> Self {
        Self::with_channels(frames, 1)
    }

    pub fn with_channels(frames: usize, channels: usize) -> Self {
        Self {
            render: Render {
                active: false,
                gain: 0.0,
                samples: vec![0.0; frames],
            },
            initialized: false,
            inputs: (channels > 1).then(|| [vec![0.0; frames], vec![0.0; frames]]),
            states: [(false, 0.0); 2],
        }
    }

    pub fn copy(
        &mut self,
        block: &DecodedSceneBlock,
        offset: u32,
        destination: usize,
        take: usize,
    ) -> Result<(), String> {
        let from = offset as usize;
        let end = from.checked_add(take).ok_or("LFE input range overflow")?;
        let to = destination
            .checked_add(take)
            .ok_or("LFE output range overflow")?;
        if to > self.render.samples.len() || block.lfes().len() > 2 {
            return Err("Invalid LFE render range".into());
        }
        for (index, lfe) in block.lfes().iter().enumerate() {
            let samples = lfe.samples().get(from..end).ok_or("Truncated LFE input")?;
            if !self.initialized {
                self.states[index] = lfe_render_state(element_state_at(
                    block,
                    lfe.element_id(),
                    lfe.initial_state(),
                    offset,
                ));
            }
            let (active, gain) = self.states[index];
            if let Some(inputs) = &mut self.inputs {
                for (out, sample) in inputs[index][destination..to].iter_mut().zip(samples) {
                    *out = if active { sample * gain } else { 0.0 };
                }
            } else {
                self.render.samples[destination..to].copy_from_slice(samples);
                self.render.active = active;
                self.render.gain = gain;
            }
        }
        self.initialized = true;
        Ok(())
    }

    pub fn mix(&mut self) {
        if let Some([first, second]) = &self.inputs {
            let gain = pair_gain(first, second);
            for ((out, a), b) in self.render.samples.iter_mut().zip(first).zip(second) {
                *out = (a + b) * gain;
            }
            self.render.active = self.states.iter().any(|(active, _)| *active);
            self.render.gain = 1.0;
        }
    }

    /// PCM here already includes gain for a folded input; single-input PCM
    /// retains its original gain to preserve the existing callback semantics.
    pub fn input(&self, index: usize) -> Option<(&[f32], bool, f32, f32)> {
        let (active, gain) = *self.states.get(index)?;
        if let Some(inputs) = &self.inputs {
            Some((&inputs[index], active, gain, 1.0))
        } else if index == 0 {
            Some((
                &self.render.samples,
                active,
                gain,
                if active { gain } else { 0.0 },
            ))
        } else {
            None
        }
    }

    pub fn finish(mut self) -> Render {
        self.mix();
        self.render
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::{SceneLfePcm, SpatialObjectState};

    fn block(first: &[f32], second: &[f32], second_gain: f32) -> DecodedSceneBlock {
        let state = |gain| Some(SpatialObjectState::new(true, None, Some(gain), true));
        DecodedSceneBlock::new(
            48_000,
            0,
            u32::try_from(first.len()).unwrap(),
            0,
            0,
            None,
            true,
            vec![],
            None,
            vec![],
        )
        .with_lfes(vec![
            SceneLfePcm::new(4, state(1.0), first.to_vec()),
            SceneLfePcm::new(10, state(second_gain), second.to_vec()),
        ])
    }

    #[test]
    fn single_lfe_keeps_its_samples_and_metadata_gain_separate() {
        let state = Some(SpatialObjectState::new(true, None, Some(0.8), true));
        let block = DecodedSceneBlock::new(
            48_000,
            0,
            2,
            0,
            0,
            None,
            true,
            vec![],
            Some(SceneLfePcm::new(4, state, vec![4.0, 5.0])),
            vec![],
        );
        let mut quantum = Quantum::new(2);
        quantum.copy(&block, 0, 0, 2).unwrap();
        let (samples, active, gain, meter_gain) = quantum.input(0).unwrap();
        assert_eq!(samples, [4.0, 5.0]);
        assert!(active && (gain - 0.8).abs() < f32::EPSILON);
        assert!((meter_gain - 0.8).abs() < f32::EPSILON);
        assert!(quantum.input(1).is_none());
        let rendered = quantum.finish();
        assert!((rendered.gain - 0.8).abs() < f32::EPSILON);
        assert_eq!(rendered.samples, [4.0, 5.0]);
    }

    #[test]
    fn one_active_lfe_keeps_unity_and_two_active_lfes_use_equal_power() {
        for (first, second, second_gain, expected) in [
            (0.5, 0.0, 1.0, 0.5),
            (0.0, 0.5, 1.0, 0.5),
            (0.5, 0.5, 1.0, std::f32::consts::FRAC_1_SQRT_2),
            (0.5, 0.5, 0.0, 0.5),
            (0.0, 0.0, 1.0, 0.0),
        ] {
            let block = block(&[first; 8], &[second; 8], second_gain);
            let mut quantum = Quantum::with_channels(8, 2);
            quantum.copy(&block, 0, 0, 8).unwrap();
            let rendered = quantum.finish();
            assert!(
                rendered
                    .samples
                    .iter()
                    .all(|sample| (sample - expected).abs() < 1e-6)
            );
            let native_fold = (first + second * second_gain) * normalization(&block, 0);
            assert!((native_fold - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn zero_crossings_and_quantum_fragmentation_do_not_toggle_fold_gain() {
        let block = block(&[0.5, 0.0, -0.5, 0.0], &[0.0, 0.5, 0.0, -0.5], 1.0);
        let mut quantum = Quantum::with_channels(4, 2);
        quantum.copy(&block, 0, 0, 2).unwrap();
        quantum.copy(&block, 2, 2, 2).unwrap();
        let rendered = quantum.finish();
        for (actual, input) in rendered.samples.iter().zip([0.5, 0.5, -0.5, -0.5]) {
            assert!((actual - input * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        }
    }

    #[test]
    fn folded_lfe_bakes_gain_ramps_once_and_is_repeatable_after_trimming() {
        use crate::decoder::{FIELD_GAIN, SceneMetadataUpdate};
        let first = SpatialObjectState::new(true, None, Some(1.0), true);
        let second = SpatialObjectState::new(true, None, Some(0.0), true);
        let block = DecodedSceneBlock::new(
            48_000,
            0,
            8,
            1,
            0,
            None,
            true,
            vec![],
            None,
            vec![SceneMetadataUpdate::new(10, 2, 2, FIELD_GAIN, first)],
        )
        .with_lfes(vec![
            SceneLfePcm::new(4, Some(first), vec![0.5; 8]),
            SceneLfePcm::new(10, Some(second), vec![0.5; 8]),
        ]);
        let expected = [0.5, 0.5, 0.5, 0.75, 1.0, 1.0, 1.0, 1.0]
            .map(|value| value * std::f32::consts::FRAC_1_SQRT_2);
        let actual = fold(&block, 0);
        for (a, b) in actual.iter().zip(expected) {
            assert!((a - b).abs() < 1e-6);
        }
        assert_eq!(fold(&block, 3), fold(&block, 3));
        assert_eq!(fold(&block, 3), actual[3..]);
    }
}
