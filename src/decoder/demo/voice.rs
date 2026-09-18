//! Modal synthesis for the demo track: struck and plucked bodies, in closed form.
//!
//! A resonant body rings as a sum of decaying sinusoids, so one note is
//!
//! ```text
//! s(t) = A(t) * sum_k a_k * exp(-t / tau_k) * sin(2*pi * f * r_k * t)
//! ```
//!
//! with `t` measured from the note's own onset. Two things follow, and both are
//! why this family was chosen over a delay line:
//!
//! - **It is closed form.** Any sample can be evaluated directly from `t`,
//!   which is what keeps [`super::DemoProgram::block_at`] a pure function of
//!   its start frame. A Karplus-Strong string sounds at least as good but is a
//!   recurrence, so a seek would have to re-render every ringing note from its
//!   onset to reproduce the same samples.
//! - **The transients are sharp.** Struck and plucked timbres localise far
//!   better than sustained ones, which is what a spatial demo needs: a pad is
//!   diffuse everywhere, and a mallet is somewhere.
//!
//! Within one block each partial advances by a rotation and a multiply rather
//! than a fresh `sin` and `exp`. That is not a departure from the closed form:
//! the recurrence is seeded exactly from the block's own start time, so the
//! same block start always produces the same samples, which is the property
//! that matters. What it buys is roughly an order of magnitude, which keeps a
//! debug build comfortably ahead of real time.

use std::f64::consts::TAU;

/// One resonant mode: where it sits, how loud it starts, how fast it fades.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Partial {
    /// Multiple of the fundamental. Struck bars are markedly inharmonic.
    ratio: f32,
    amplitude: f32,
    /// Seconds to fall by `1/e`.
    tau: f32,
}

/// Where a mode has fallen far enough to stop evaluating it.
///
/// `ln(10^(36/20))` is 36 dB down, which is [`crate::scene3d::params::OBJECT_SILENT_GAIN`]
/// -- the one floor the nameplate, the footprint and the meter bank all share.
/// Past it the scene already reads the object as silent, so carrying the tail
/// further would cost arithmetic to produce something nothing displays.
const SILENCE_TAUS: f32 = 4.145;

/// How long a note is given to fade when the next one on its object arrives.
pub(crate) const DAMP_SECONDS: f32 = 0.08;

#[derive(Debug, Clone)]
pub(crate) struct Timbre {
    partials: Vec<Partial>,
    /// Raised-cosine onset. Without it every partial starts at full amplitude
    /// on one sample and the step is audible as a click.
    attack_seconds: f32,
    /// Sum of the partial amplitudes, i.e. the value the sum reaches when they
    /// are all in phase at `t = 0`. Dividing by it puts a note's peak at one,
    /// so a level is a level rather than a guess.
    peak: f32,
}

impl Timbre {
    /// A bar, a bell or a plate: a handful of measured, inharmonic modes.
    pub(crate) fn modal(attack_seconds: f32, modes: &[(f32, f32, f32)]) -> Self {
        let partials = modes
            .iter()
            .map(|&(ratio, amplitude, tau)| Partial {
                ratio,
                amplitude,
                tau,
            })
            .collect();
        Self::new(partials, attack_seconds)
    }

    /// A plucked string: harmonic modes whose upper partials fade first.
    ///
    /// `1/k` amplitudes with decays going as `k^-0.8` is the first-order
    /// picture of a string losing its highs, and it is the closed-form stand-in
    /// for the delay line this module deliberately does not use.
    pub(crate) fn plucked(attack_seconds: f32, fundamental_tau: f32, harmonics: u8) -> Self {
        let partials = (1..=harmonics)
            .map(|harmonic| {
                let index = f32::from(harmonic);
                Partial {
                    ratio: index,
                    amplitude: 1.0 / index,
                    tau: fundamental_tau / index.powf(0.8),
                }
            })
            .collect();
        Self::new(partials, attack_seconds)
    }

    fn new(partials: Vec<Partial>, attack_seconds: f32) -> Self {
        let peak = partials.iter().map(|partial| partial.amplitude).sum();
        Self {
            partials,
            attack_seconds,
            peak,
        }
    }

    /// Seconds until every mode has passed the scene's silence floor.
    pub(crate) fn tail_seconds(&self) -> f32 {
        self.partials
            .iter()
            .map(|partial| partial.tau)
            .fold(0.0, f32::max)
            * SILENCE_TAUS
    }

    /// The longest mode, which is what a pool slot must outlive before it may
    /// be handed to another note.
    pub(crate) fn longest_tau(&self) -> f32 {
        self.partials
            .iter()
            .map(|partial| partial.tau)
            .fold(0.0, f32::max)
    }
}

/// Add one note to `out`, which holds one block of a single object's PCM.
///
/// `onset_offset` is the note's onset relative to the block's first frame, so a
/// note that began in an earlier block passes a negative value and is picked up
/// mid-decay. `stop_offset` is where the note must be silent regardless of how
/// much of its decay is left: an object that carries a melodic line is one
/// instrument playing it, and a real one does not leave twenty notes ringing
/// while it plays the twenty-first. A pool slot holding a single note passes
/// its natural tail instead and rings out in full.
///
/// Nothing here reads or writes state that outlives the call.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "every value converted here is bounded by one Scene block: offsets \
              are clamped into 0..frames before indexing, and the times are \
              seconds inside a three-minute programme, so none of them reaches \
              the range where a cast could lose anything that matters"
)]
pub(crate) fn render(
    out: &mut [f32],
    timbre: &Timbre,
    frequency: f32,
    level: f32,
    onset_offset: i64,
    stop_offset: i64,
    sample_rate: u32,
) {
    let frames = out.len();
    if frames == 0 || sample_rate == 0 || level == 0.0 {
        return;
    }
    let rate = f64::from(sample_rate);
    // Where in this block the note is audible: from its onset (or the block's
    // start, if it began earlier) until its longest mode passes the floor.
    let first = usize::try_from(onset_offset.max(0))
        .unwrap_or(usize::MAX)
        .min(frames);
    let tail = f64::from(timbre.tail_seconds()) * rate;
    let last = onset_offset
        .saturating_add(tail as i64)
        .min(stop_offset)
        .clamp(0, frames as i64) as usize;
    if first >= last {
        return;
    }

    // Window this note before mixing it with any tails already in `out`.
    let mut note = vec![0.0f32; last - first];
    let scale = level / timbre.peak;
    for partial in &timbre.partials {
        // Seed the recurrence from the exact time this block reaches, so the
        // block is reproducible from its start frame alone.
        let elapsed = (first as i64 - onset_offset) as f64 / rate;
        let step = TAU * f64::from(frequency) * f64::from(partial.ratio) / rate;
        let phase = step * (first as i64 - onset_offset) as f64;
        let (mut sine, mut cosine) = phase.sin_cos();
        let (step_sin, step_cos) = step.sin_cos();
        let decay = (-1.0 / (f64::from(partial.tau) * rate)).exp();
        let mut envelope = f64::from(partial.amplitude) * (-elapsed / f64::from(partial.tau)).exp();
        for sample in &mut note {
            *sample += (envelope * sine) as f32 * scale;
            let rotated = (
                sine * step_cos + cosine * step_sin,
                cosine * step_cos - sine * step_sin,
            );
            sine = rotated.0;
            cosine = rotated.1;
            envelope *= decay;
        }
    }

    // The attack window, applied once over the sum rather than per partial.
    let attack = (f64::from(timbre.attack_seconds) * rate) as i64;
    if attack > 0 {
        let attack_end = onset_offset
            .saturating_add(attack)
            .clamp(first as i64, last as i64) as usize;
        for (index, sample) in note[..attack_end - first].iter_mut().enumerate() {
            let progress = (first as i64 + index as i64 - onset_offset) as f64 / attack as f64;
            *sample *= raised_cosine(progress);
        }
    }

    // A note cut short is damped rather than dropped: truncating a ringing
    // partial mid-cycle is a step, and a step is a click.
    let damp = (f64::from(DAMP_SECONDS) * rate) as i64;
    if damp > 0 && stop_offset < onset_offset.saturating_add(tail as i64) {
        let damp_start = (stop_offset - damp).clamp(first as i64, last as i64) as usize;
        for (index, sample) in note[damp_start - first..].iter_mut().enumerate() {
            let remaining = (stop_offset - damp_start as i64 - index as i64) as f64 / damp as f64;
            *sample *= raised_cosine(remaining.clamp(0.0, 1.0));
        }
    }
    for (sample, contribution) in out[first..last].iter_mut().zip(note) {
        *sample += contribution;
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a window value in 0..=1 narrowed to the sample format it scales"
)]
fn raised_cosine(progress: f64) -> f32 {
    (0.5 - 0.5 * (std::f64::consts::PI * progress).cos()) as f32
}

/// Equal temperament, A4 = 440 Hz at MIDI 69.
pub(crate) fn frequency_of(pitch: u8) -> f32 {
    440.0 * ((f32::from(pitch) - 69.0) / 12.0).exp2()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    #[test]
    fn demo_overlapping_notes_keep_independent_envelopes() {
        let timbre = Timbre::plucked(0.003, 1.8, 10);
        let mut old = vec![0.0; 2048];
        let mut new = vec![0.0; 2048];
        render(
            &mut old,
            &timbre,
            frequency_of(50),
            0.125,
            -38912,
            4928,
            RATE,
        );
        render(
            &mut new,
            &timbre,
            frequency_of(45),
            0.125,
            1088,
            i64::MAX,
            RATE,
        );
        let mut mixed = old.clone();
        render(
            &mut mixed,
            &timbre,
            frequency_of(45),
            0.125,
            1088,
            i64::MAX,
            RATE,
        );
        let difference = mixed
            .iter()
            .zip(old.iter().zip(&new))
            .map(|(actual, (old, new))| (actual - old - new).abs())
            .fold(0.0f32, f32::max);
        assert!(
            difference < 1e-6,
            "the new note changed the old tail by {difference}; at its onset {} became {}",
            old[1088],
            mixed[1088]
        );
    }

    fn marimba() -> Timbre {
        Timbre::modal(
            0.003,
            &[(1.0, 1.0, 0.75), (3.9, 0.25, 0.22), (9.2, 0.08, 0.10)],
        )
    }

    fn peak(samples: &[f32]) -> f32 {
        samples
            .iter()
            .fold(0.0, |peak, sample| peak.max(sample.abs()))
    }

    #[test]
    fn a_note_peaks_at_the_level_it_was_asked_for() {
        let mut out = vec![0.0; RATE as usize];
        render(
            &mut out,
            &marimba(),
            frequency_of(69),
            0.5,
            0,
            i64::MAX,
            RATE,
        );
        let peak = peak(&out);
        assert!(
            (0.3..=0.5).contains(&peak),
            "peak {peak} should approach but not exceed the requested level"
        );
    }

    #[test]
    fn the_onset_is_windowed_rather_than_stepped() {
        let mut out = vec![0.0; 256];
        render(
            &mut out,
            &marimba(),
            frequency_of(69),
            1.0,
            0,
            i64::MAX,
            RATE,
        );
        // A 3 ms raised cosine has barely opened one sample in.
        assert!(out[0].abs() < 1e-6, "the first sample must not step");
        assert!(out[1].abs() < 0.05, "the window must open gradually");
    }

    #[test]
    fn a_block_is_the_same_wherever_the_note_started() {
        // The property the whole seek design rests on: a note picked up
        // mid-decay must produce the samples it would have produced had the
        // block been reached by playing through.
        let timbre = marimba();
        let mut whole = vec![0.0; 8192];
        render(
            &mut whole,
            &timbre,
            frequency_of(60),
            0.8,
            0,
            i64::MAX,
            RATE,
        );
        let mut later = vec![0.0; 2048];
        render(
            &mut later,
            &timbre,
            frequency_of(60),
            0.8,
            -4096,
            i64::MAX,
            RATE,
        );
        for (index, (&expected, &actual)) in whole[4096..6144].iter().zip(&later).enumerate() {
            assert!(
                (expected - actual).abs() < 1e-4,
                "sample {index} differs: {expected} vs {actual}"
            );
        }
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        reason = "a decay of under a second at a known rate, converted to frames"
    )]
    fn a_note_is_dropped_once_it_passes_the_silence_floor() {
        let timbre = marimba();
        let tail = (timbre.tail_seconds() * RATE as f32) as usize;
        let mut out = vec![0.0; tail + 4096];
        render(&mut out, &timbre, frequency_of(69), 1.0, 0, i64::MAX, RATE);
        assert!(
            peak(&out[tail..]) == 0.0,
            "the tail must stop being evaluated"
        );
        assert!(
            peak(&out[tail - 512..tail]) < 0.02,
            "and it must already be inaudible when it does"
        );
    }

    #[test]
    fn a_plucked_string_loses_its_highs_first() {
        let timbre = Timbre::plucked(0.002, 1.4, 12);
        let first = timbre.partials[0];
        let last = timbre.partials[11];
        assert!(last.tau < first.tau, "upper partials must fade sooner");
        assert!(last.amplitude < first.amplitude);
        assert!((timbre.longest_tau() - 1.4).abs() < 1e-6);
    }

    #[test]
    fn equal_temperament_lands_on_the_reference_pitch() {
        assert!((frequency_of(69) - 440.0).abs() < 1e-3);
        assert!((frequency_of(81) - 880.0).abs() < 1e-2);
        assert!((frequency_of(57) - 220.0).abs() < 1e-3);
    }
}
