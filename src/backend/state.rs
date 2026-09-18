//! Resolving an element's OAMD state at a point inside a Scene block.
//!
//! This is pure arithmetic over `crate::decoder`'s cross-platform types, and it
//! is shared by the Windows render callback and the scene preview. Validation,
//! timeline trimming and OAMD resolution must agree between those consumers;
//! their arithmetic and its tests run on every platform.

use crate::decoder::{
    DecodedSceneBlock, FIELD_POSITION, SceneLfePcm, SceneObjectPcm, SceneSignature,
    SpatialObjectState,
};
#[cfg(test)]
use crate::decoder::{FIELD_GAIN, FIELD_HEAD_TRACKING, SpatialPosition};
use crate::scene_view::ObjectEnergy;

/// Check the same Scene contract before either consumer accepts a block.
/// Error prefixes are also the app's automatic-reconfiguration contract.
pub(super) fn validate_block(
    block: &DecodedSceneBlock,
    sample_rate: u32,
    dynamic_object_count: u32,
    stream_has_lfe: bool,
    expected_signature: &SceneSignature,
) -> Result<(), String> {
    if block.sample_rate() != sample_rate {
        return Err(format!(
            "Scene sample rate changed from {sample_rate} to {} Hz",
            block.sample_rate()
        ));
    }
    let expected = usize::try_from(block.duration_frames())
        .map_err(|_| "Scene block duration exceeds usize".to_owned())?;
    let actual_dynamic_objects = u32::try_from(block.objects().len())
        .map_err(|_| "Scene object count exceeds the Windows API range".to_owned())?;
    if actual_dynamic_objects != dynamic_object_count {
        return Err(format!(
            "Scene dynamic-object count changed from {dynamic_object_count} to {actual_dynamic_objects} after Spatial Audio activation"
        ));
    }
    if block.lfe().is_some() != stream_has_lfe {
        return Err("Scene LFE layout changed after Spatial Audio activation".to_owned());
    }
    if expected_signature.configuration_generation() != block.configuration_generation() {
        return Err(format!(
            "Scene configuration generation changed from {} to {}; the Spatial Audio stream must be reconfigured",
            expected_signature.configuration_generation(),
            block.configuration_generation()
        ));
    }
    if expected_signature.presentation_index() != block.presentation_index()
        || expected_signature.presentation_id() != block.presentation_id()
    {
        return Err("Selected Scene presentation changed during Spatial Audio playback".to_owned());
    }
    let mut actual_object_ids = block
        .objects()
        .iter()
        .map(SceneObjectPcm::element_id)
        .collect::<Vec<_>>();
    actual_object_ids.sort_unstable();
    if actual_object_ids != expected_signature.object_element_ids() {
        return Err("Scene dynamic-object element IDs changed during playback".to_owned());
    }
    if block.lfe().map(SceneLfePcm::element_id) != expected_signature.lfe_element_id() {
        return Err("Scene LFE element ID changed during playback".to_owned());
    }
    for object in block.objects() {
        if object.samples().len() != expected {
            return Err(format!(
                "Scene object {} PCM length does not match its block",
                object.element_id()
            ));
        }
    }
    if let Some(component) = block.lfe()
        && component.samples().len() != expected
    {
        return Err("Scene LFE PCM length does not match its block".to_owned());
    }
    Ok(())
}

/// Offset of the first frame on the current presentation timeline.
/// `None` skips an entirely expired block, including MP4 pre-zero preroll.
pub(super) fn block_offset_at(
    block: &DecodedSceneBlock,
    timeline_frame: i64,
) -> Result<Option<u32>, String> {
    let block_end = block
        .start_frame()
        .checked_add(i64::from(block.duration_frames()))
        .ok_or_else(|| "Scene block end position overflow".to_owned())?;
    if block_end <= timeline_frame {
        return Ok(None);
    }
    let offset = if block.start_frame() < timeline_frame {
        u32::try_from(timeline_frame - block.start_frame())
            .map_err(|_| "Scene overlap exceeds a block".to_owned())?
    } else {
        0
    };
    Ok(Some(offset))
}

#[cfg(any(feature = "decode", test))]
pub(super) use crate::decoder::metadata::element_state_at;
#[cfg(macinrender_output)]
pub(super) use crate::decoder::metadata::remaining_ramps;
#[cfg(any(macinrender_output, test))]
pub(super) use crate::decoder::metadata::state_at_updates;
#[cfg(test)]
use crate::decoder::metadata::{MetadataRamp, interpolate_state};

/// Flatten a resolved state into listener coordinates: Core/ADM `[x, y, z]`
/// becomes `[x, z, -y]`, clamped to the unit cube.
///
/// This is what Windows Spatial Audio is handed for a dynamic object, and also
/// what the scene view draws in — one conversion, so a picture of a scene and
/// the scene itself cannot disagree about where anything is.
pub(super) fn listener_render_state(state: Option<SpatialObjectState>) -> (bool, [f32; 3], f32) {
    let Some(state) = state else {
        return (false, [0.0; 3], 0.0);
    };
    let Some(position) = state.position() else {
        return (false, [0.0; 3], 0.0);
    };
    let windows_position = [
        position.x().clamp(-1.0, 1.0),
        position.z().clamp(-1.0, 1.0),
        (-position.y()).clamp(-1.0, 1.0),
    ];
    let gain = state.linear_gain().unwrap_or(1.0);
    (
        state.metadata_active() && state.semantic_complete(),
        windows_position,
        gain,
    )
}

/// The same for the LFE bed, which carries activation and gain but no position.
///
/// All consumers use this for the positionless bed's activation and metering.
#[cfg_attr(
    not(feature = "decode"),
    allow(
        dead_code,
        reason = "LFE rendering and metering require a decoded Scene"
    )
)]
pub(super) fn lfe_render_state(state: Option<SpatialObjectState>) -> (bool, f32) {
    let Some(state) = state else {
        return (false, 0.0);
    };
    (
        state.metadata_active() && state.semantic_complete(),
        state.linear_gain().unwrap_or(1.0),
    )
}

/// Unweighted LFE channel level before master volume. BS.1770 excludes LFE;
/// applying its high-pass here would obscure the bass this meter measures.
#[cfg_attr(
    not(feature = "decode"),
    allow(dead_code, reason = "LFE metering requires a Scene consumer")
)]
pub(super) fn measure_lfe(samples: &[f32], gain: f32) -> ObjectEnergy {
    let gain = if gain.is_finite() { gain.max(0.0) } else { 0.0 };
    let mut sum = 0.0_f64;
    let mut peak = 0.0_f32;
    for &sample in samples {
        let value = f64::from(sample) * f64::from(gain);
        sum = value.mul_add(value, sum);
        peak = peak.max(sample.abs() * gain);
    }
    ObjectEnergy {
        sum_squares: sum,
        frames: u32::try_from(samples.len()).unwrap_or(u32::MAX),
        peak,
    }
}

/// ITU-R BS.1770-4 K-weighting: the head-effect shelf and the RLB high-pass,
/// cascaded into one fourth-order section, with one filter's memory.
///
/// This is the measurement half of the object loudness channel, and it lives
/// beside the OAMD resolution for the same reason that does: all three Scene
/// consumers have to agree, and a second derivation would drift. What it
/// returns is energy, never a meter reading — see [`ObjectEnergy`] for why that
/// distinction is load bearing.
///
/// The constants are the standard's, and the bilinear derivation below is the
/// one every conformant implementation uses, so the coefficients come out equal
/// at any sample rate rather than only at forty-eight kilohertz. `state` is
/// direct form II: one slot for the incoming sample and four of history.
///
/// **A per-object loudness is not a standardised quantity.** BS.1770 defines
/// loudness over a channel-based programme, with channel weights and programme
/// gating; object audio is measured by rendering to a reference layout first.
/// What this computes — momentary loudness of one mono object, weight one — is
/// well defined as arithmetic and is what any other object meter would compute,
/// but no standard blesses it as *the* loudness of an object. The UI says so;
/// this type is the reason it can.
#[derive(Debug, Clone, Copy)]
pub(super) struct KWeighting {
    numerator: [f64; 5],
    denominator: [f64; 5],
    state: [f64; 5],
}

impl KWeighting {
    /// Derive the cascade for `sample_rate`.
    #[allow(
        clippy::inconsistent_digit_grouping,
        clippy::unreadable_literal,
        reason = "the coefficients are transcribed digit for digit from ITU-R \
                  BS.1770-4; regrouping them would make checking them against \
                  the standard harder, which is the only check that matters here"
    )]
    pub(super) fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(1));

        // Stage one: the high-frequency shelf standing in for the head.
        let shelf_frequency = 1681.974450955533_f64;
        let shelf_gain_db = 3.999843853973347_f64;
        let shelf_q = 0.7071752369554196_f64;
        let shelf_k = (std::f64::consts::PI * shelf_frequency / rate).tan();
        let shelf_high = 10.0_f64.powf(shelf_gain_db / 20.0);
        let shelf_band = shelf_high.powf(0.4996667741545416);
        let shelf_norm = shelf_k.mul_add(shelf_k, 1.0 + shelf_k / shelf_q);
        let shelf_b = [
            shelf_k.mul_add(shelf_k, shelf_band.mul_add(shelf_k / shelf_q, shelf_high))
                / shelf_norm,
            2.0 * shelf_k.mul_add(shelf_k, -shelf_high) / shelf_norm,
            shelf_k.mul_add(shelf_k, shelf_high - shelf_band * shelf_k / shelf_q) / shelf_norm,
        ];
        let shelf_a = [
            1.0,
            2.0 * shelf_k.mul_add(shelf_k, -1.0) / shelf_norm,
            shelf_k.mul_add(shelf_k, 1.0 - shelf_k / shelf_q) / shelf_norm,
        ];

        // Stage two: the RLB high-pass, whose numerator is a fixed `1, -2, 1`.
        let rlb_frequency = 38.13547087602444_f64;
        let rlb_q = 0.5003270373238773_f64;
        let rlb_k = (std::f64::consts::PI * rlb_frequency / rate).tan();
        let rlb_norm = rlb_k.mul_add(rlb_k, 1.0 + rlb_k / rlb_q);
        let rlb_b = [1.0, -2.0, 1.0];
        let rlb_a = [
            1.0,
            2.0 * rlb_k.mul_add(rlb_k, -1.0) / rlb_norm,
            rlb_k.mul_add(rlb_k, 1.0 - rlb_k / rlb_q) / rlb_norm,
        ];

        Self {
            numerator: convolve(shelf_b, rlb_b),
            denominator: convolve(shelf_a, rlb_a),
            state: [0.0; 5],
        }
    }

    /// Measure one publication window of an object's mono plane.
    ///
    /// `gain` is the OAMD gain the renderer was handed, and it enters as its
    /// square because the sum is of squared samples. Master volume deliberately
    /// does not: the meter reports the content's level, not where the listener
    /// happens to have left the volume control.
    pub(super) fn measure(&mut self, samples: &[f32], gain: f32) -> ObjectEnergy {
        // Samples reaching the FIFO are already finite and correctly sized
        // (`decoder::worker::validate_samples`), so only the gain needs a guard.
        let gain = if gain.is_finite() { gain.max(0.0) } else { 0.0 };
        let mut sum = 0.0_f64;
        let mut peak = 0.0_f32;
        for &sample in samples {
            peak = peak.max(sample.abs());
            let weighted = self.step(f64::from(sample));
            sum = weighted.mul_add(weighted, sum);
        }
        let gain_squared = f64::from(gain) * f64::from(gain);
        ObjectEnergy {
            sum_squares: sum * gain_squared,
            frames: u32::try_from(samples.len()).unwrap_or(u32::MAX),
            peak: peak * gain,
        }
    }

    /// Measure `frames` of a timeline gap.
    ///
    /// A gap renders silence, so it counts as silence rather than as nothing —
    /// and the zeros go through the filter rather than around it, because the
    /// filter has memory and the path that does not skip them (the Windows
    /// quantum, where a gap is already zeros in the buffer) would otherwise
    /// produce a different number from the same audio.
    pub(super) fn measure_silence(&mut self, frames: u32) -> ObjectEnergy {
        let mut sum = 0.0_f64;
        for _ in 0..frames {
            let weighted = self.step(0.0);
            sum = weighted.mul_add(weighted, sum);
        }
        ObjectEnergy {
            sum_squares: sum,
            frames,
            peak: 0.0,
        }
    }

    /// One direct-form-II step.
    fn step(&mut self, sample: f64) -> f64 {
        let denominator = self.denominator;
        let numerator = self.numerator;
        self.state[0] = sample
            - denominator[1] * self.state[1]
            - denominator[2] * self.state[2]
            - denominator[3] * self.state[3]
            - denominator[4] * self.state[4];
        let out = numerator[0] * self.state[0]
            + numerator[1] * self.state[1]
            + numerator[2] * self.state[2]
            + numerator[3] * self.state[3]
            + numerator[4] * self.state[4];
        self.state[4] = self.state[3];
        self.state[3] = self.state[2];
        self.state[2] = self.state[1];
        self.state[1] = self.state[0];
        out
    }
}

/// Cascade two biquads into one fourth-order section.
fn convolve(first: [f64; 3], second: [f64; 3]) -> [f64; 5] {
    [
        first[0] * second[0],
        first[0].mul_add(second[1], first[1] * second[0]),
        first[0].mul_add(second[2], first[1].mul_add(second[1], first[2] * second[0])),
        first[1].mul_add(second[2], first[2] * second[1]),
        first[2] * second[2],
    ]
}

/// Whether an instant metadata update for `element_id` lands in
/// `[from_offset, to_offset)` of `block`.
///
/// `ramp_frames == 0` is OAMD stating outright that nothing is interpolated:
/// the element is at one position and then at another, with no moment in
/// between at which it was anywhere else. That is a fact about the bitstream,
/// not a guess from how far the object moved — whether the jump is *worth
/// drawing a marker for* is a separate, perceptual question, and it is decided
/// in `scene3d`.
pub(super) fn has_instant_update(
    block: &DecodedSceneBlock,
    element_id: u64,
    from_offset: u32,
    to_offset: u32,
) -> bool {
    for update in block.metadata_updates() {
        // Updates are in ascending offset order, the same assumption
        // `element_state_at` makes.
        if update.offset_frames() >= to_offset {
            break;
        }
        if update.element_id() == element_id
            && update.changed_fields() & FIELD_POSITION != 0
            && update.ramp_frames() == 0
            && update.offset_frames() >= from_offset
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use crate::decoder::{SceneMetadataUpdate, SceneObjectPcm};

    use super::*;

    fn complete_state(gain: f32) -> SpatialObjectState {
        SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(0.25, -0.5, 0.75)),
            Some(gain),
            true,
        )
    }

    const RATE: u32 = 48_000;

    /// Momentary loudness of a mean square, as ITU-R BS.1770-4 defines it.
    fn loudness(mean_square: f64) -> f64 {
        10.0 * mean_square.log10() - 0.691
    }

    fn sine(frequency: f64, amplitude: f32, frames: u32) -> Vec<f32> {
        (0..frames)
            .map(|frame| {
                let phase = std::f64::consts::TAU * frequency * f64::from(frame) / f64::from(RATE);
                amplitude * as_f32(phase.sin())
            })
            .collect()
    }

    /// Narrow a generated sample. Test signals only, where the value is bounded
    /// to the unit interval and the narrowing is the point.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "test signal generation, bounded to [-1, 1]"
    )]
    fn as_f32(value: f64) -> f32 {
        value as f32
    }

    /// Deterministic noise: a 32-bit xorshift mapped into `[-amplitude, amplitude]`.
    fn noise(amplitude: f32, frames: u32) -> Vec<f32> {
        let mut state = 0x1234_5678_u32;
        (0..frames)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                amplitude * as_f32((f64::from(state) / f64::from(u32::MAX)).mul_add(2.0, -1.0))
            })
            .collect()
    }

    /// Our own momentary reading over a whole signal, the way the mirror's ring
    /// computes it: one energy accumulation, then mean square to loudness.
    fn measured_loudness(samples: &[f32], gain: f32) -> f64 {
        let mut filter = KWeighting::new(RATE);
        let energy = filter.measure(samples, gain);
        loudness(energy.sum_squares / f64::from(energy.frames))
    }

    fn reference_loudness(samples: &[f32], gain: f32) -> f64 {
        let mut meter =
            ebur128::EbuR128::new(1, RATE, ebur128::Mode::M).expect("reference meter is valid");
        let scaled: Vec<f32> = samples.iter().map(|sample| sample * gain).collect();
        meter
            .add_frames_f32(&scaled)
            .expect("reference accepts f32");
        meter.loudness_momentary().expect("momentary is in mode M")
    }

    #[test]
    fn k_weighted_loudness_matches_the_reference_implementation() {
        // The reference resolves momentary loudness over its own trailing
        // 400 ms window, so the signals below are exactly that long and the two
        // therefore integrate the same audio. What is left is arithmetic order,
        // which is why the budget is a hundredth of a decibel rather than the
        // tenth the cross-check contract allows.
        let frames = 400 * RATE / 1000;
        for (name, samples) in [
            ("1 kHz sine at -20 dBFS", sine(1000.0, 0.1, frames)),
            ("1 kHz sine at full scale", sine(1000.0, 1.0, frames)),
            ("60 Hz sine below the RLB corner", sine(60.0, 0.5, frames)),
            ("10 kHz sine above the shelf", sine(10_000.0, 0.5, frames)),
            ("wideband noise", noise(0.3, frames)),
        ] {
            for gain in [1.0_f32, 0.5, 0.031_622_78] {
                let ours = measured_loudness(&samples, gain);
                let reference = reference_loudness(&samples, gain);
                assert!(
                    (ours - reference).abs() < 0.01,
                    "{name} at gain {gain}: measured {ours} LUFS, reference {reference} LUFS"
                );
            }
        }
    }

    #[test]
    fn silence_measures_as_frames_without_energy_and_leaves_no_peak() {
        let mut filter = KWeighting::new(RATE);
        let energy = filter.measure_silence(480);
        assert_eq!(energy.frames, 480);
        assert_eq!(energy.sum_squares.to_bits(), 0.0_f64.to_bits());
        assert_eq!(energy.peak.to_bits(), 0.0_f32.to_bits());
    }

    #[test]
    fn a_gap_measured_as_silence_equals_the_same_span_of_zero_samples() {
        // The Windows quantum hands the filter a buffer whose gap region is
        // already zeros, while the preview skips the samples and calls
        // `measure_silence`. The two have to come out identical or the paths
        // disagree about the same audio.
        let tail = sine(1000.0, 0.5, 480);
        let mut zeros_path = KWeighting::new(RATE);
        let mut silence_path = KWeighting::new(RATE);

        let mut buffered = vec![0.0_f32; 480];
        buffered.extend_from_slice(&tail);
        let buffered = zeros_path.measure(&buffered, 1.0);

        let mut skipped = silence_path.measure_silence(480);
        skipped.absorb(silence_path.measure(&tail, 1.0));

        assert_eq!(buffered.frames, skipped.frames);
        assert!(
            (buffered.sum_squares - skipped.sum_squares).abs() < 1e-12,
            "buffered {} vs skipped {}",
            buffered.sum_squares,
            skipped.sum_squares
        );
    }

    #[test]
    fn gain_enters_the_energy_as_its_square_and_degenerate_gains_land_on_silence() {
        let samples = sine(1000.0, 0.5, 4800);
        let unity = KWeighting::new(RATE).measure(&samples, 1.0);
        let halved = KWeighting::new(RATE).measure(&samples, 0.5);
        assert!(
            (halved.sum_squares / unity.sum_squares - 0.25).abs() < 1e-9,
            "half gain gave {} of the unity energy",
            halved.sum_squares / unity.sum_squares
        );
        assert!((halved.peak - unity.peak * 0.5).abs() < f32::EPSILON);

        for degenerate in [0.0_f32, -1.0, f32::NAN, f32::INFINITY] {
            let energy = KWeighting::new(RATE).measure(&samples, degenerate);
            assert_eq!(
                energy.sum_squares.to_bits(),
                0.0_f64.to_bits(),
                "gain {degenerate} should measure as silence"
            );
            assert_eq!(energy.frames, 4800);
        }
    }

    #[test]
    fn an_empty_span_measures_as_nothing_at_all() {
        let energy = KWeighting::new(RATE).measure(&[], 1.0);
        assert_eq!(energy.frames, 0);
        assert_eq!(energy.sum_squares.to_bits(), 0.0_f64.to_bits());
    }

    #[test]
    fn tracking_switches_at_onset_and_does_not_replace_continuous_targets() {
        use crate::decoder::ContentHeadTracking;
        let initial = SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(-1.0, 0.0, 0.0)),
            Some(0.0),
            true,
        )
        .with_tracking(ContentHeadTracking::SceneRelative);
        let target = SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(1.0, 0.0, 0.0)),
            Some(1.0),
            true,
        )
        .with_tracking(ContentHeadTracking::HeadRelative);
        let updates = [
            SceneMetadataUpdate::new(7, 0, 100, FIELD_POSITION, target),
            SceneMetadataUpdate::new(7, 0, 200, FIELD_GAIN, target),
            SceneMetadataUpdate::new(7, 25, 0, FIELD_HEAD_TRACKING, target),
            // An ignored binaural-mode-only event must not restart either ramp.
            SceneMetadataUpdate::new(7, 40, 0, FIELD_HEAD_TRACKING, target),
        ];
        assert_eq!(state_at_updates(&updates, 7, None, 0), Some(target));
        let before = state_at_updates(&updates, 7, Some(initial), 24).unwrap();
        assert_eq!(before.tracking(), ContentHeadTracking::SceneRelative);
        let at_switch = state_at_updates(&updates, 7, Some(initial), 25).unwrap();
        assert_eq!(at_switch.tracking(), ContentHeadTracking::HeadRelative);
        assert_eq!(
            at_switch.position().unwrap().x().to_bits(),
            (-0.5_f32).to_bits()
        );
        assert_eq!(at_switch.linear_gain(), Some(0.125));
        let later = state_at_updates(&updates, 7, Some(initial), 50).unwrap();
        assert_eq!(later.position().unwrap().x().to_bits(), 0.0_f32.to_bits());
        assert_eq!(later.linear_gain(), Some(0.25));
        let end = state_at_updates(&updates, 7, Some(initial), 200).unwrap();
        assert_eq!(end, target);
        let block = DecodedSceneBlock::new(
            48_000,
            0,
            240,
            1,
            0,
            None,
            true,
            vec![SceneObjectPcm::new(7, Some(initial), vec![0.01; 240])],
            None,
            updates.to_vec(),
        );
        assert!(!has_instant_update(&block, 7, 25, 41));
        #[cfg(macinrender_output)]
        {
            let ramps = remaining_ramps(&block, 7, 50);
            assert_eq!(ramps[0].unwrap().ramp_frames(), 50);
            assert_eq!(ramps[1].unwrap().ramp_frames(), 150);
            assert_eq!(ramps[0].unwrap().state().position(), target.position());
            assert_eq!(
                ramps[1].unwrap().state().linear_gain(),
                target.linear_gain()
            );
        }
    }

    #[test]
    fn unsupported_tracking_does_not_bypass_spatial_checks_or_invent_warmup_state() {
        use crate::decoder::{ContentHeadTracking, TrackingIssue};
        let tracking = ContentHeadTracking::Unsupported(TrackingIssue::MissingGlobalControl);
        let state = complete_state(0.7).with_tracking(tracking);
        assert!(listener_render_state(Some(state)).0);
        let invalid = SpatialObjectState::new(true, state.position(), state.linear_gain(), false)
            .with_tracking(tracking);
        assert!(!listener_render_state(Some(invalid)).0);
        assert!(!listener_render_state(None).0);
        assert_eq!(state_at_updates(&[], 7, None, 0), None);
    }
    #[test]
    fn maps_core_adm_axes_to_windows_listener_coordinates() {
        let state = SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(0.5, 1.0, 0.25)),
            Some(0.75),
            true,
        );
        let (active, position, gain) = listener_render_state(Some(state));
        assert!(active);
        for (actual, expected) in position.into_iter().zip([0.5, 0.25, -1.0]) {
            assert!((actual - expected).abs() < f32::EPSILON);
        }
        assert!((gain - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn interpolates_position_and_gain_over_metadata_ramps() {
        let from = SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(-1.0, 0.0, 0.0)),
            Some(0.25),
            true,
        );
        let to = SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(1.0, 0.0, 1.0)),
            Some(0.75),
            true,
        );
        let middle = interpolate_state(from, to, 0.5);
        assert_eq!(middle.position(), Some(SpatialPosition::new(0.0, 0.0, 0.5)));
        assert_eq!(middle.linear_gain(), Some(0.5));
    }

    #[test]
    fn later_update_establishes_state_when_initial_state_is_missing() {
        let target = complete_state(0.6);
        let block = DecodedSceneBlock::new(
            48_000,
            0,
            16,
            1,
            0,
            None,
            true,
            vec![SceneObjectPcm::new(42, None, vec![0.0; 16])],
            None,
            vec![SceneMetadataUpdate::new(42, 4, 8, u32::MAX, target)],
        );

        assert_eq!(element_state_at(&block, 42, None, 3), None);
        assert_eq!(element_state_at(&block, 42, None, 4), Some(target));
        assert_eq!(element_state_at(&block, 42, None, 12), Some(target));
    }

    #[test]
    fn ramp_endpoint_uses_the_exact_complete_target_state() {
        let incomplete = SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(-1.0, 0.0, 0.0)),
            Some(0.25),
            false,
        );
        let complete = complete_state(0.9);

        assert_eq!(interpolate_state(incomplete, complete, 1.0), complete);
        assert_eq!(
            MetadataRamp {
                start_frame: 10,
                duration_frames: 5,
                from: incomplete,
                to: complete,
            }
            .state_at(15),
            complete
        );
    }

    #[test]
    fn lfe_render_state_follows_oamd_activation_and_gain() {
        assert_eq!(lfe_render_state(None), (false, 0.0));
        assert_eq!(lfe_render_state(Some(complete_state(0.4))), (true, 0.4));

        let inactive = SpatialObjectState::new(false, None, Some(0.8), true);
        assert_eq!(lfe_render_state(Some(inactive)), (false, 0.8));
    }

    #[test]
    fn instant_update_detection_is_half_open_element_specific_and_ignores_ramps() {
        let state = complete_state(1.0);
        let block = DecodedSceneBlock::new(
            48_000,
            0,
            16,
            1,
            0,
            None,
            true,
            Vec::new(),
            None,
            vec![
                SceneMetadataUpdate::new(7, 2, 0, u32::MAX, state),
                SceneMetadataUpdate::new(9, 4, 0, u32::MAX, state),
                SceneMetadataUpdate::new(7, 6, 3, u32::MAX, state),
                SceneMetadataUpdate::new(7, 8, 0, u32::MAX, state),
            ],
        );

        assert!(has_instant_update(&block, 7, 2, 3));
        assert!(!has_instant_update(&block, 7, 0, 2));
        assert!(!has_instant_update(&block, 7, 3, 8));
        assert!(has_instant_update(&block, 7, 3, 9));
        assert!(!has_instant_update(&block, 8, 0, 16));
    }
}
