//! Reading an `AutoEq` `ParametricEQ` profile for display.
//!
//! The renderer owns the authoritative parse, the coefficient design and the
//! audio. This is a second, independent reading of the same documented format,
//! and it exists for one reason: a profile is otherwise a filename, and a
//! filename says nothing about what it does to the sound.
//!
//! Being independent is the point rather than a cost, because the two readings
//! are checked against each other twice. [`Profile::disagreement`] compares this
//! parse band for band against the renderer's own, which needs no output and so
//! runs the moment a file is read; `app::hptf_disagreement` then compares the
//! designed peak against a cascade that is actually running. Same posture as
//! `backend::state`'s cross-check against `ebur128`: agreement with a separate
//! implementation is what pins the arithmetic.
//!
//! Nothing here touches audio, the filesystem beyond one read, or any OS API.

use std::f64::consts::PI;

/// The renderer's cascade is a fixed-length POD so it can be published to the
/// audio callback by value, and it refuses a profile asking for more rather
/// than shortening one.
const MAX_BANDS: usize = 32;
/// `Q` for a filter line that omits it, matching the renderer's `k_default_q`.
const DEFAULT_Q: f64 = 0.707;
/// The audible band the renderer searches for its own `max_response_db`.
pub const MIN_HZ: f64 = 20.0;
pub const MAX_HZ: f64 = 20_000.0;
/// 1/96 octave over the audible decade and a half. Dense enough that the peak
/// found here agrees with the renderer's bisection to well inside the tolerance
/// `app` compares them at, and cheap enough to hold for drawing.
const POINTS: usize = 960;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BandType {
    Peaking,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    BandPass,
    Notch,
}

impl BandType {
    /// The renderer's own token table. An unrecognised type is skipped rather
    /// than refused, because `AutoEq`'s vocabulary drifts.
    fn parse(token: &str) -> Option<Self> {
        let token = token.to_ascii_uppercase();
        Some(match token.as_str() {
            "PK" | "PEQ" | "MODAL" => Self::Peaking,
            "LSC" | "LS" | "LSQ" => Self::LowShelf,
            "HSC" | "HS" | "HSQ" => Self::HighShelf,
            "LP" | "LPQ" => Self::LowPass,
            "HP" | "HPQ" => Self::HighPass,
            "BP" => Self::BandPass,
            "NO" => Self::Notch,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Band {
    pub kind: BandType,
    pub fc: f64,
    pub gain_db: f64,
    pub q: f64,
}

impl Band {
    /// One RBJ section, normalised by `a0`, as `[b0, b1, b2, a1, a2]`.
    ///
    /// Shelves are the Q-parameterised form rather than the slope form: that is
    /// what `LSC`/`HSC` mean to Equalizer APO and `AutoEq`, and what the renderer
    /// designs, so the two agree by construction rather than by luck.
    #[allow(
        clippy::manual_midpoint,
        reason = "the sections are transcribed term for term from the RBJ \
                  cookbook, which is how they stay checkable against the \
                  renderer's design_band; midpoint would obscure that"
    )]
    fn section(self, rate: f64) -> [f64; 5] {
        let amp = 10.0_f64.powf(self.gain_db / 40.0);
        let w = 2.0 * PI * self.fc / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * self.q);
        let (b, a) = match self.kind {
            BandType::Peaking => (
                [1.0 + alpha * amp, -2.0 * cos, 1.0 - alpha * amp],
                [1.0 + alpha / amp, -2.0 * cos, 1.0 - alpha / amp],
            ),
            BandType::LowShelf => {
                let t = 2.0 * amp.sqrt() * alpha;
                (
                    [
                        amp * ((amp + 1.0) - (amp - 1.0) * cos + t),
                        2.0 * amp * ((amp - 1.0) - (amp + 1.0) * cos),
                        amp * ((amp + 1.0) - (amp - 1.0) * cos - t),
                    ],
                    [
                        (amp + 1.0) + (amp - 1.0) * cos + t,
                        -2.0 * ((amp - 1.0) + (amp + 1.0) * cos),
                        (amp + 1.0) + (amp - 1.0) * cos - t,
                    ],
                )
            }
            BandType::HighShelf => {
                let t = 2.0 * amp.sqrt() * alpha;
                (
                    [
                        amp * ((amp + 1.0) + (amp - 1.0) * cos + t),
                        -2.0 * amp * ((amp - 1.0) + (amp + 1.0) * cos),
                        amp * ((amp + 1.0) + (amp - 1.0) * cos - t),
                    ],
                    [
                        (amp + 1.0) - (amp - 1.0) * cos + t,
                        2.0 * ((amp - 1.0) - (amp + 1.0) * cos),
                        (amp + 1.0) - (amp - 1.0) * cos - t,
                    ],
                )
            }
            BandType::LowPass => (
                [(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0],
                [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
            ),
            BandType::HighPass => (
                [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0],
                [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
            ),
            BandType::BandPass => ([alpha, 0.0, -alpha], [1.0 + alpha, -2.0 * cos, 1.0 - alpha]),
            BandType::Notch => (
                [1.0, -2.0 * cos, 1.0],
                [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
            ),
        };
        [
            b[0] / a[0],
            b[1] / a[0],
            b[2] / a[0],
            a[1] / a[0],
            a[2] / a[0],
        ]
    }
}

/// `|H(e^jw)|` in decibels for one normalised section.
fn section_db(section: [f64; 5], w: f64) -> f64 {
    let (sin, cos) = w.sin_cos();
    let (sin2, cos2) = (2.0 * w).sin_cos();
    let [b0, b1, b2, a1, a2] = section;
    let numerator = (b0 + b1 * cos + b2 * cos2).powi(2) + (b1 * sin + b2 * sin2).powi(2);
    let denominator = (1.0 + a1 * cos + a2 * cos2).powi(2) + (a1 * sin + a2 * sin2).powi(2);
    if denominator <= 0.0 || !numerator.is_finite() || !denominator.is_finite() {
        return 0.0;
    }
    10.0 * (numerator / denominator).log10()
}

/// One parsed `ParametricEQ.txt`.
#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub preamp_db: f64,
    bands: Vec<Band>,
}

impl Profile {
    /// Parse profile text on the renderer's terms: blank lines and `#` comments
    /// are skipped, a line that is not a `Filter` line is ignored, `OFF` bands
    /// are dropped, an unknown type is skipped, and a malformed number is an
    /// error rather than a silently different curve.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut profile = Self::default();
        let mut saw_any = false;
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let Some(first) = tokens.first() else {
                continue;
            };
            if first.eq_ignore_ascii_case("Preamp:") || first.eq_ignore_ascii_case("Preamp") {
                profile.preamp_db = tokens
                    .get(1)
                    .and_then(|value| value.parse::<f64>().ok())
                    .ok_or_else(|| format!("Preamp is not a number: {line}"))?;
                if !profile.preamp_db.is_finite() {
                    return Err(format!("Preamp must be a finite number: {line}"));
                }
                saw_any = true;
                continue;
            }
            if !first.eq_ignore_ascii_case("Filter") {
                continue;
            }
            // `Filter N: ON|OFF TYPE Fc <f> Hz Gain <g> dB Q <q>`, located by the
            // ON/OFF token because the index may or may not carry its colon.
            let Some(state) = tokens
                .iter()
                .position(|t| t.eq_ignore_ascii_case("ON") || t.eq_ignore_ascii_case("OFF"))
            else {
                continue;
            };
            let enabled = tokens[state].eq_ignore_ascii_case("ON");
            let Some(kind) = tokens.get(state + 1).and_then(|t| BandType::parse(t)) else {
                continue;
            };
            let fc = value_after(&tokens, "Fc", None)
                .ok_or_else(|| format!("Filter line has no readable Fc: {line}"))?;
            let gain_db = value_after(&tokens, "Gain", Some(0.0))
                .ok_or_else(|| format!("Gain is not a number: {line}"))?;
            let q = value_after(&tokens, "Q", Some(DEFAULT_Q))
                .ok_or_else(|| format!("Q is not a number: {line}"))?;
            if !fc.is_finite() || fc <= 0.0 {
                return Err(format!("Fc must be a finite positive number: {line}"));
            }
            if !q.is_finite() || q <= 0.0 {
                return Err(format!("Q must be a finite positive number: {line}"));
            }
            if !gain_db.is_finite() {
                return Err(format!("Gain must be a finite number: {line}"));
            }
            saw_any = true;
            // A disabled band is parsed and validated like any other so a typo in
            // one still reports, then dropped: only enabled bands are designed,
            // which is what the renderer counts.
            if enabled {
                profile.bands.push(Band {
                    kind,
                    fc,
                    gain_db,
                    q,
                });
            }
        }
        if !saw_any {
            return Err("No Preamp or Filter line found".into());
        }
        // The renderer refuses a cascade this long rather than shortening it, and
        // a drawn curve for a file it will not accept is worse than an error.
        if profile.bands.len() > MAX_BANDS {
            return Err(format!(
                "More than {MAX_BANDS} filters are enabled: {}",
                profile.bands.len()
            ));
        }
        Ok(profile)
    }

    /// A cascade someone else parsed, so two readings of one file can be
    /// compared as the same kind of thing. `bands` is the cascade — the bands
    /// that are switched on — because that is what this side keeps and what
    /// reaches the audio.
    #[cfg_attr(
        not(macinrender_output),
        allow(
            dead_code,
            reason = "the only other parse of this format is the renderer's, and \
                      there is no renderer to ask on a build without one"
        )
    )]
    pub fn of_bands(preamp_db: f64, bands: Vec<Band>) -> Self {
        Self { preamp_db, bands }
    }

    /// Where this reading of a profile and the renderer's differ.
    ///
    /// The renderer owns the authoritative parse, so a difference means the
    /// picture is wrong, not the sound. Reported at the first difference and
    /// named down to the band, because "they disagree" is not something a
    /// person can act on and "filter 7's Q" is.
    pub fn disagreement(&self, renderer: &Self) -> Option<String> {
        // Both sides convert the same decimal text with a correctly rounded
        // parse, so this tolerance is not for rounding — it is there so a future
        // normalisation on either side shows up as itself rather than as noise.
        const SAME: f64 = 1e-9;
        if (self.preamp_db - renderer.preamp_db).abs() > SAME {
            return Some(format!(
                "Reading a preamp of {:+.3} dB, but the renderer reads {:+.3} dB.",
                self.preamp_db, renderer.preamp_db
            ));
        }
        if self.bands.len() != renderer.bands.len() {
            return Some(format!(
                "Reading {} enabled filters, but the renderer reads {}.",
                self.bands.len(),
                renderer.bands.len()
            ));
        }
        for (index, (ours, theirs)) in self.bands.iter().zip(&renderer.bands).enumerate() {
            let band = index + 1;
            if ours.kind != theirs.kind {
                return Some(format!(
                    "Reading filter {band} as {:?}, but the renderer reads {:?}.",
                    ours.kind, theirs.kind
                ));
            }
            for (label, ours, theirs) in [
                ("Fc", ours.fc, theirs.fc),
                ("gain", ours.gain_db, theirs.gain_db),
                ("Q", ours.q, theirs.q),
            ] {
                if (ours - theirs).abs() > SAME {
                    return Some(format!(
                        "Reading filter {band}'s {label} as {ours}, but the renderer reads {theirs}."
                    ));
                }
            }
        }
        None
    }

    pub fn bands(&self) -> u32 {
        u32::try_from(self.bands.len()).unwrap_or(u32::MAX)
    }

    /// The cascade's response at `hz`, **preamp included**, so the curve is what
    /// the headphone feed receives and its peak is the renderer's
    /// `max_response_db` rather than a different quantity that resembles it.
    pub fn response_db(&self, hz: f64, rate: u32) -> f64 {
        let rate = f64::from(rate.max(1));
        let w = 2.0 * PI * hz / rate;
        self.bands.iter().fold(self.preamp_db, |total, band| {
            total + section_db(band.section(rate), w)
        })
    }
}

/// A `key` token's numeric value, or `fallback` when the key is absent. `None`
/// means the key is there and its value will not parse, which is an error.
fn value_after(tokens: &[&str], key: &str, fallback: Option<f64>) -> Option<f64> {
    match tokens.iter().position(|t| t.eq_ignore_ascii_case(key)) {
        Some(index) => tokens.get(index + 1).and_then(|v| v.parse::<f64>().ok()),
        None => fallback,
    }
}

/// A profile sampled for drawing, and the facts the renderer can be asked to
/// confirm.
#[derive(Clone, Debug)]
pub struct Curve {
    pub bands: u32,
    pub preamp_db: f32,
    pub max_response_db: f32,
    pub peak_hz: f32,
    /// Decibels at [`POINTS`] positions spaced evenly in log frequency across
    /// [`MIN_HZ`]..=[`MAX_HZ`].
    pub points: Vec<f32>,
}

impl Curve {
    /// A biquad's response depends on the rate it was designed at, so a curve is
    /// only the profile's curve for the rate the output is running; the caller
    /// keys its cache on both.
    #[cfg_attr(
        not(macinrender_output),
        allow(
            dead_code,
            reason = "the panel parses the text itself, so that both readings get \
                      the same bytes; only the renderer-gated tests read by path"
        )
    )]
    pub fn read(path: &str, rate: u32) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
        Ok(Self::of(&Profile::parse(&text)?, rate))
    }

    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "the sample index is a small integer, and a decibel readout for \
                  drawing does not need f64 range"
    )]
    pub fn of(profile: &Profile, rate: u32) -> Self {
        let mut points = Vec::with_capacity(POINTS);
        let mut peak = (f64::NEG_INFINITY, MIN_HZ);
        for index in 0..POINTS {
            let hz = MIN_HZ * (MAX_HZ / MIN_HZ).powf(index as f64 / (POINTS - 1) as f64);
            let db = profile.response_db(hz, rate);
            if db > peak.0 {
                peak = (db, hz);
            }
            points.push(db as f32);
        }
        // A high-Q band can peak between grid points, and its centre is where it
        // peaks. Adding the centres costs one evaluation each and keeps the
        // agreement with the renderer's bisection inside a tenth of a decibel.
        for band in &profile.bands {
            let hz = band.fc.clamp(MIN_HZ, MAX_HZ);
            let db = profile.response_db(hz, rate);
            if db > peak.0 {
                peak = (db, hz);
            }
        }
        Self {
            bands: profile.bands(),
            preamp_db: profile.preamp_db as f32,
            max_response_db: peak.0 as f32,
            peak_hz: peak.1 as f32,
            points,
        }
    }

    /// Fold in the additional attenuation reported by the running output.
    /// The file's preamp remains separate so it can still be checked against
    /// the renderer's original profile metadata.
    pub fn with_output_trim(&self, trim_db: f32) -> Self {
        let mut curve = self.clone();
        for db in &mut curve.points {
            *db += trim_db;
        }
        curve.max_response_db += trim_db;
        curve
    }

    /// Where `hz` sits across the plot, as a fraction of its width.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a screen fraction does not need f64 range"
    )]
    pub fn position(hz: f64) -> f32 {
        ((hz / MIN_HZ).log10() / (MAX_HZ / MIN_HZ).log10()) as f32
    }

    pub fn floor(&self) -> f32 {
        self.points.iter().copied().fold(f32::INFINITY, f32::min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published `AutoEq` preset for the Sony MDR-MV1, verbatim.
    const AUTOEQ: &str = "\
Preamp: -4.1 dB
Filter 1: ON LSC Fc 105 Hz Gain 9.7 dB Q 0.70
Filter 2: ON PK Fc 46 Hz Gain -9.2 dB Q 0.37
Filter 3: ON PK Fc 1722 Hz Gain 2.8 dB Q 0.95
Filter 4: ON PK Fc 4643 Hz Gain 9.2 dB Q 1.99
Filter 5: ON PK Fc 5779 Hz Gain -9.2 dB Q 1.50
Filter 6: ON PK Fc 128 Hz Gain 0.5 dB Q 2.09
Filter 7: ON PK Fc 242 Hz Gain -0.7 dB Q 2.04
Filter 8: ON PK Fc 705 Hz Gain 0.8 dB Q 2.45
Filter 9: ON PK Fc 2955 Hz Gain -0.9 dB Q 4.58
Filter 10: ON HSC Fc 10000 Hz Gain -2.2 dB Q 0.70
";

    #[test]
    fn a_published_preset_parses_to_its_own_filter_list() {
        let profile = Profile::parse(AUTOEQ).unwrap();
        assert_eq!(profile.bands(), 10);
        assert!((profile.preamp_db - -4.1).abs() < 1e-9);
    }

    /// One band at a time, against the textbook values its own definition fixes:
    /// a peaking filter reaches its full gain at its centre and unity far from
    /// it, and a shelf reaches its full gain at DC or Nyquist and half of it at
    /// the corner. These hold for any correct RBJ section, which is what makes
    /// them worth asserting rather than a restatement of the code.
    #[test]
    fn each_band_shape_meets_its_definition() {
        const RATE: u32 = 48_000;
        let one = |line: &str| Profile::parse(&format!("Preamp: 0 dB\n{line}\n")).unwrap();

        let peaking = one("Filter 1: ON PK Fc 1000 Hz Gain 6.0 dB Q 2.0");
        assert!((peaking.response_db(1000.0, RATE) - 6.0).abs() < 0.01);
        assert!(peaking.response_db(40.0, RATE).abs() < 0.05);
        assert!(peaking.response_db(18_000.0, RATE).abs() < 0.05);

        let low = one("Filter 1: ON LSC Fc 105 Hz Gain 9.7 dB Q 0.70");
        assert!((low.response_db(1.0, RATE) - 9.7).abs() < 0.05);
        assert!((low.response_db(105.0, RATE) - 4.85).abs() < 0.05);
        assert!(low.response_db(10_000.0, RATE).abs() < 0.05);

        let high = one("Filter 1: ON HSC Fc 10000 Hz Gain -2.2 dB Q 0.70");
        assert!((high.response_db(23_900.0, RATE) - -2.2).abs() < 0.05);
        assert!((high.response_db(10_000.0, RATE) - -1.1).abs() < 0.05);
        assert!(high.response_db(100.0, RATE).abs() < 0.05);
    }

    #[test]
    fn the_curve_reports_the_peak_the_renderer_protects_against() {
        let curve = Curve::of(&Profile::parse(AUTOEQ).unwrap(), 48_000);
        assert_eq!(curve.bands, 10);
        // Externally checked: this preset's designed response, preamp included,
        // just clears full scale in the 4-5 kHz boost. A profile whose peak rose
        // above zero would be the one the settings panel warns about.
        assert!(
            (curve.max_response_db - -0.11).abs() < 0.1,
            "peak {} dB",
            curve.max_response_db
        );
        assert!(
            (4000.0..5000.0).contains(&curve.peak_hz),
            "peak at {} Hz",
            curve.peak_hz
        );
        assert!(curve.floor() > -12.0, "floor {} dB", curve.floor());
    }

    /// The same cascade at a different rate is a different cascade: the bilinear
    /// transform warps each band's centre by a different amount, which shows up
    /// where the narrow high bands are steep. Drawing one rate's curve for
    /// another rate's output is the mistake this pins, and why the curve carries
    /// the rate it was designed at.
    #[test]
    fn the_design_rate_changes_the_response_where_the_narrow_bands_are() {
        let profile = Profile::parse(AUTOEQ).unwrap();
        let at_48 = profile.response_db(8_000.0, 48_000);
        let at_44 = profile.response_db(8_000.0, 44_100);
        assert!(
            (at_48 - at_44).abs() > 0.05,
            "48 kHz {at_48} dB, 44.1 kHz {at_44} dB"
        );
        // Low enough that the warping is negligible, and the two agree.
        assert!(
            (profile.response_db(200.0, 48_000) - profile.response_db(200.0, 44_100)).abs() < 0.01
        );
    }

    #[test]
    fn the_renderers_tolerances_are_matched_line_for_line() {
        // Off bands are dropped, unknown types are skipped, comments and stray
        // lines are ignored, a missing Q falls back, and CRLF survives.
        let profile = Profile::parse(
            "# a comment\r\nPreamp: -1.5 dB\r\n\
             Filter 1: ON PK Fc 1000 Hz Gain 3 dB Q 1.0\r\n\
             Filter 2: OFF PK Fc 2000 Hz Gain 9 dB Q 1.0\r\n\
             Filter 3: ON XYZ Fc 3000 Hz Gain 9 dB Q 1.0\r\n\
             Filter 4: ON\r\n\
             something else entirely\r\n\
             Filter 5: ON LS Fc 200 Hz Gain 2 dB\r\n",
        )
        .unwrap();
        assert_eq!(profile.bands(), 2);
        assert!((profile.preamp_db - -1.5).abs() < 1e-9);

        // A malformed number is an error, not a quietly different curve.
        assert!(Profile::parse("Preamp: loud\n").is_err());
        assert!(Profile::parse("Filter 1: ON PK Fc 0 Hz Gain 3 dB Q 1.0\n").is_err());
        assert!(Profile::parse("Filter 1: ON PK Fc 100 Hz Gain 3 dB Q -1\n").is_err());
        assert!(Profile::parse("Filter 1: ON PK Fc x Hz Gain 3 dB Q 1\n").is_err());
        assert!(Profile::parse("\n# nothing but a comment\n").is_err());
    }

    #[test]
    fn the_two_rejections_the_renderer_makes_are_made_here_too() {
        let band = "Filter 1: ON PK Fc 1000 Hz Gain 1 dB Q 1\n";
        // The renderer refuses a cascade past its fixed length rather than
        // shortening it, so a shortened curve here would picture a file it will
        // not play.
        assert!(Profile::parse(&band.repeat(MAX_BANDS)).is_ok());
        assert!(Profile::parse(&band.repeat(MAX_BANDS + 1)).is_err());
        // The limit counts the cascade, not the file: a disabled band is
        // validated and dropped, and never spends a slot.
        let off = "Filter 1: OFF PK Fc 1000 Hz Gain 1 dB Q 1\n";
        assert!(Profile::parse(&format!("{}{}", band.repeat(MAX_BANDS), off.repeat(8))).is_ok());

        // `strtod` and Rust both read these as numbers; it is the validation
        // after the parse that has to agree.
        for preamp in ["inf", "-inf", "nan"] {
            assert!(
                Profile::parse(&format!("Preamp: {preamp} dB\n{band}")).is_err(),
                "{preamp} must be refused"
            );
        }
    }

    #[test]
    fn a_disagreement_names_the_band_and_the_field_that_differs() {
        let peak = |fc, gain_db, q| Band {
            kind: BandType::Peaking,
            fc,
            gain_db,
            q,
        };
        let ours = Profile::of_bands(-3.0, vec![peak(1000.0, 6.0, 1.0), peak(80.0, -2.0, 0.7)]);
        assert_eq!(ours.disagreement(&ours), None);

        let says = |other: &Profile| ours.disagreement(other).expect("must disagree");
        assert!(says(&Profile::of_bands(-2.0, ours.bands.clone())).contains("preamp"));
        assert!(
            says(&Profile::of_bands(-3.0, vec![peak(1000.0, 6.0, 1.0)]))
                .contains("enabled filters")
        );

        // The band is named by its position in the cascade, one-based, and the
        // field by the token it was read from.
        let second = |band: Band| Profile::of_bands(-3.0, vec![ours.bands[0], band]);
        assert!(says(&second(peak(81.0, -2.0, 0.7))).contains("filter 2's Fc"));
        assert!(says(&second(peak(80.0, -2.5, 0.7))).contains("filter 2's gain"));
        assert!(says(&second(peak(80.0, -2.0, 0.71))).contains("filter 2's Q"));
        let shelf = Band {
            kind: BandType::LowShelf,
            ..ours.bands[1]
        };
        assert!(says(&second(shelf)).contains("filter 2 as Peaking"));
    }

    #[test]
    fn the_plot_places_a_decade_where_a_log_axis_would() {
        assert!(Curve::position(MIN_HZ).abs() < 1e-6);
        assert!((Curve::position(MAX_HZ) - 1.0).abs() < 1e-6);
        // 20 Hz to 20 kHz is three decades, so 200 Hz sits one third across.
        assert!((Curve::position(200.0) - 1.0 / 3.0).abs() < 1e-6);
    }
}
