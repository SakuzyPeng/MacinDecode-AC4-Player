//! Sensor-side prediction. The clock model estimates excess holding time only:
//! its lower envelope absorbs the unknown minimum link/fusion delay.
use crate::head_tracking::Quaternion;
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

pub const MAX_PREDICTION: f64 = 0.025;
pub const MAX_ROTATION_DEG: f64 = 5.0;
#[derive(Clone, Debug)]
pub struct Motion {
    pub instance: u64,
    pub session: u64,
    pub reference: u64,
    pub sequence: u64,
    pub delivery: u64,
    pub device_ms: Option<u64>,
    pub clock_epoch: u64,
    pub clock_kind: u32,
    pub received_at: Instant,
    pub physical: Quaternion,
    pub gyro: Option<[f64; 3]>,
    pub acceleration: Option<[f64; 3]>,
    pub profile: u8,
    pub orientation_source: &'static str,
}
#[derive(Default)]
pub struct Inbox {
    pub reset: bool,
    pub overrun: u64,
    pub samples: VecDeque<Motion>,
}
impl Inbox {
    pub fn push(&mut self, sample: Motion) {
        if self.samples.len() == 256 {
            self.samples.pop_front();
            self.overrun += 1;
        }
        self.samples.push_back(sample);
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Normal,
    Warming,
    Predicting,
    Quiet,
    MissingTimestamp,
    MissingGyro,
    ClockUnstable,
    InconsistentMotion,
    Impact,
    Stale,
    Held,
}
#[cfg(posebridge_input)]
impl State {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "Ordinary tracking",
            Self::Warming => "Warming up sensor timing",
            Self::Predicting => "Enhanced tracking",
            Self::Quiet => "Enhanced · low rotational activity",
            Self::MissingTimestamp => "Ordinary · device timestamps unavailable",
            Self::MissingGyro => "Ordinary · same-frame angular velocity unavailable",
            Self::ClockUnstable => "Ordinary · device clock unstable",
            Self::InconsistentMotion => "Ordinary · gyro/pose disagreement",
            Self::Impact => "Ordinary · acceleration outside the prediction range",
            Self::Stale => "Frozen · motion data expired",
            Self::Held => "Orientation held",
        }
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Diagnostics {
    pub state: State,
    pub clock_ready: bool,
    pub drift_ppm: Option<f64>,
    pub excess_age_ms: Option<f64>,
    pub prediction_ms: f64,
    pub prediction_degrees: f64,
    pub gyro: Option<[f64; 3]>,
    pub acceleration: Option<[f64; 3]>,
    pub residual_degrees: Option<f64>,
    pub samples: u64,
    pub coalesced: u64,
    pub overruns: u64,
    pub orientation_source: &'static str,
}
#[derive(Clone, Copy)]
struct Anchor {
    delivery: u64,
    bucket: u64,
    device: f64,
    received: f64,
}
struct Clock {
    origin_device: u64,
    origin_host: Instant,
    last_device: u64,
    samples: u64,
    anchors: VecDeque<Anchor>,
    slope: f64,
    offset: f64,
    stable: bool,
    drift_ppm: Option<f64>,
    last_fit: f64,
}
impl Clock {
    fn new(device: u64, host: Instant) -> Self {
        Self {
            origin_device: device,
            origin_host: host,
            last_device: device,
            samples: 0,
            anchors: VecDeque::new(),
            slope: 1.,
            offset: 0.,
            stable: true,
            drift_ppm: None,
            last_fit: -1.,
        }
    }
    fn observe(&mut self, m: &Motion, anchor: bool) -> bool {
        let Some(device) = m.device_ms else {
            return false;
        };
        if device < self.last_device
            || device < self.origin_device
            || m.received_at < self.origin_host
        {
            return false;
        }
        if self.samples > 0 && (device == self.last_device || device - self.last_device > 250) {
            return false;
        }
        self.samples += 1;
        self.last_device = device;
        if !anchor {
            return true;
        }
        let t = seconds_ms(device - self.origin_device);
        let r = m.received_at.duration_since(self.origin_host).as_secs_f64();
        let anchor = Anchor {
            delivery: m.delivery,
            bucket: (device - self.origin_device) / 1000,
            device: t,
            received: r,
        };
        if self
            .anchors
            .back()
            .is_some_and(|a| a.delivery == m.delivery)
        {
            self.anchors.pop_back();
        }
        self.anchors.push_back(anchor);
        while self.anchors.front().is_some_and(|a| r - a.received > 10.)
            || self.anchors.len() > 2048
        {
            self.anchors.pop_front();
        }
        if r - self.last_fit >= 1. {
            self.last_fit = r;
            let mut bins = BTreeMap::<u64, Anchor>::new();
            for &a in &self.anchors {
                let old = bins.entry(a.bucket).or_insert(a);
                if a.received - self.slope * a.device < old.received - self.slope * old.device {
                    *old = a;
                }
            }
            if bins.len() >= 5 {
                let points: Vec<_> = bins.into_values().collect();
                let mut slopes = Vec::new();
                for (i, a) in points.iter().enumerate() {
                    for b in &points[i + 1..] {
                        if b.device - a.device >= 2. {
                            slopes.push((b.received - a.received) / (b.device - a.device));
                        }
                    }
                }
                if !slopes.is_empty() {
                    slopes.sort_by(f64::total_cmp);
                    let slope = slopes[slopes.len() / 2];
                    self.drift_ppm = Some((slope - 1.) * 1e6);
                    self.stable = (0.995..=1.005).contains(&slope);
                    if self.stable {
                        self.slope = slope;
                    }
                }
            }
        }
        self.offset = self
            .anchors
            .iter()
            .map(|a| a.received - self.slope * a.device)
            .fold(f64::INFINITY, f64::min);
        true
    }
    fn ready(&self) -> bool {
        self.stable
            && self.samples >= 8
            && self.last_device.saturating_sub(self.origin_device) >= 300
    }
    fn age(&self, m: &Motion, now: Instant) -> Option<f64> {
        let time = m.device_ms?;
        if !self.ready() || time < self.origin_device {
            return None;
        }
        let mapped = self.slope * seconds_ms(time - self.origin_device) + self.offset;
        Some(
            (now.saturating_duration_since(self.origin_host)
                .as_secs_f64()
                - mapped)
                .max(0.),
        )
    }
}
#[allow(
    clippy::cast_precision_loss,
    reason = "Convert bounded relative milliseconds, never the absolute device calendar"
)]
fn seconds_ms(value: u64) -> f64 {
    value as f64 / 1000.
}
fn norm(v: [f64; 3]) -> f64 {
    v.iter().map(|x| x * x).sum::<f64>().sqrt()
}
fn good_gyro(v: [f64; 3]) -> bool {
    v.iter()
        .all(|x| x.is_finite() && x.abs() < 2000_f64.to_radians())
}
fn distance(a: Quaternion, b: Quaternion) -> f64 {
    let a = a.normalized();
    let b = b.normalized();
    2. * a
        .0
        .iter()
        .zip(b.0)
        .map(|(x, y)| x * y)
        .sum::<f64>()
        .abs()
        .clamp(0., 1.)
        .acos()
        .to_degrees()
}
fn integrate(q: Quaternion, w: [f64; 3], seconds: f64) -> Quaternion {
    let speed = norm(w);
    if speed < 1e-12 || seconds <= 0. {
        return q;
    }
    let half = speed * seconds / 2.;
    let scale = half.sin() / speed;
    q.multiply(Quaternion([
        half.cos(),
        w[0] * scale,
        w[1] * scale,
        w[2] * scale,
    ]))
    .normalized()
}
fn valid_q(q: Quaternion) -> bool {
    q.0.iter().all(|v| v.is_finite()) && q.0.iter().map(|v| v * v).sum::<f64>() > 1e-12
}

pub struct Enhancement {
    latest: Option<Motion>,
    previous: Option<Motion>,
    identity: Option<(u64, u64, u64)>,
    clock_identity: Option<(u32, u64, u8)>,
    reference: Quaternion,
    clock: Option<Clock>,
    issue: State,
    quiet_since: Option<u64>,
    quiet: bool,
    was_ready: bool,
    transition: Option<(Instant, Quaternion)>,
    diagnostics: Diagnostics,
}
impl Default for Enhancement {
    fn default() -> Self {
        Self {
            latest: None,
            previous: None,
            identity: None,
            clock_identity: None,
            reference: Quaternion::default(),
            clock: None,
            issue: State::Warming,
            quiet_since: None,
            quiet: false,
            was_ready: false,
            transition: None,
            diagnostics: Diagnostics::default(),
        }
    }
}
impl Enhancement {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    fn clear_timing(&mut self) {
        self.clock = None;
        self.previous = None;
        self.quiet_since = None;
        self.quiet = false;
        self.issue = State::Warming;
    }
    pub fn ingest(&mut self, inbox: Inbox, presented: Quaternion) {
        if inbox.reset {
            self.reset();
        }
        self.diagnostics.overruns = self.diagnostics.overruns.saturating_add(inbox.overrun);
        if inbox.overrun > 0 {
            self.clear_timing();
        }
        if let Some(last) = inbox.samples.back() {
            let identity = (last.instance, last.session, last.reference);
            if self.identity != Some(identity) {
                self.clear_timing();
                self.identity = Some(identity);
                self.reference = last.physical.multiply(presented.conjugate()).normalized();
            }
        }
        self.diagnostics.coalesced = self.diagnostics.coalesced.saturating_add(
            u64::try_from(inbox.samples.len().saturating_sub(1)).unwrap_or(u64::MAX),
        );
        let mut samples = inbox.samples.into_iter().peekable();
        while let Some(m) = samples.next() {
            let anchor = samples
                .peek()
                .is_none_or(|next| next.delivery != m.delivery);
            if self.identity != Some((m.instance, m.session, m.reference)) {
                continue;
            }
            if self.latest.as_ref().is_some_and(|p| {
                p.instance == m.instance
                    && p.session == m.session
                    && p.reference == m.reference
                    && p.sequence >= m.sequence
            }) {
                continue;
            }
            self.accept(m, anchor);
        }
    }
    #[allow(
        clippy::too_many_lines,
        reason = "Validate one coherent motion sample before allowing prediction"
    )]
    fn accept(&mut self, m: Motion, anchor: bool) {
        self.diagnostics.samples += 1;
        self.diagnostics.gyro = m.gyro;
        self.diagnostics.acceleration = m.acceleration;
        self.diagnostics.orientation_source = m.orientation_source;
        self.diagnostics.residual_degrees = None;
        if !valid_q(m.physical) {
            self.clear_timing();
            self.issue = State::InconsistentMotion;
            self.latest = None;
            return;
        }
        let clock_key = (m.clock_kind, m.clock_epoch, m.profile);
        if self.clock_identity != Some(clock_key) {
            self.clear_timing();
            self.clock_identity = Some(clock_key);
        }
        self.issue = State::Warming;
        if let Some(time) = m.device_ms {
            let clock = self
                .clock
                .get_or_insert_with(|| Clock::new(time, m.received_at));
            if !clock.observe(&m, anchor) {
                self.clear_timing();
                self.issue = State::ClockUnstable;
            }
        } else {
            self.clear_timing();
            self.issue = State::MissingTimestamp;
        }
        let mut quiet = false;
        match m.gyro {
            Some(w) if good_gyro(w) => {
                if let Some(previous) = &self.previous
                    && let (Some(old), Some(new), Some(old_w)) =
                        (previous.device_ms, m.device_ms, previous.gyro)
                    && new > old
                    && good_gyro(old_w)
                {
                    let dt = seconds_ms(new - old);
                    if dt > 0.25 {
                        self.clear_timing();
                        self.issue = State::Warming;
                    } else {
                        let mean = std::array::from_fn(|i| old_w[i].midpoint(w[i]));
                        let residual = distance(integrate(previous.physical, mean, dt), m.physical);
                        self.diagnostics.residual_degrees = Some(residual);
                        if residual > 5. {
                            self.issue = State::InconsistentMotion;
                        } else {
                            quiet = norm(w) < 1_f64.to_radians()
                                && distance(previous.physical, m.physical) / dt < 1.;
                            if let Some(a) = m.acceleration {
                                quiet &=
                                    a.iter().all(|v| v.is_finite()) && (norm(a) - 1.).abs() < 0.1;
                            }
                        }
                    }
                }
            }
            _ => self.issue = State::MissingGyro,
        }
        if let Some(a) = m.acceleration
            && (!a.iter().all(|v| v.is_finite()) || !(0.5..=1.5).contains(&norm(a)))
        {
            self.issue = State::Impact;
            quiet = false;
        }
        if quiet {
            if let Some(t) = m.device_ms {
                let since = self.quiet_since.get_or_insert(t);
                self.quiet = t.saturating_sub(*since) >= 500;
            }
        } else {
            self.quiet_since = None;
            self.quiet = false;
        }
        self.previous = Some(m.clone());
        self.latest = Some(m);
    }
    pub fn measured(&self) -> Option<Quaternion> {
        self.latest.as_ref().map(|m| m.physical)
    }
    pub fn expired(&self, now: Instant, max_age: Duration) -> bool {
        self.latest.as_ref().is_some_and(|m| {
            let host = now.saturating_duration_since(m.received_at).as_secs_f64();
            let mapped = self
                .clock
                .as_ref()
                .and_then(|c| c.age(m, now))
                .unwrap_or(0.);
            host.max(mapped) >= max_age.as_secs_f64()
        })
    }
    pub fn recenter(&mut self, now: Instant, max_age: Duration) {
        if !self.expired(now, max_age)
            && let Some(m) = &self.latest
        {
            self.reference = m.physical;
            self.was_ready = false;
            self.transition = None;
        }
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "Explicit control-step inputs keep time and source validity testable"
    )]
    pub fn apply(
        &mut self,
        normal: Quaternion,
        presented: Quaternion,
        normal_active: bool,
        enabled: bool,
        held: bool,
        now: Instant,
        max_age: Duration,
    ) -> (Quaternion, bool) {
        self.diagnostics.prediction_ms = 0.;
        self.diagnostics.prediction_degrees = 0.;
        self.diagnostics.excess_age_ms = None;
        self.diagnostics.clock_ready = self.clock.as_ref().is_some_and(Clock::ready);
        self.diagnostics.drift_ppm = self.clock.as_ref().and_then(|c| c.drift_ppm);
        if held {
            self.was_ready = false;
            self.transition = None;
            self.diagnostics.state = State::Held;
            return (presented, false);
        }
        if !normal_active {
            self.was_ready = false;
            self.transition = None;
            self.diagnostics.state = State::Stale;
            return (presented, true);
        }
        let Some(m) = &self.latest else {
            self.was_ready = false;
            self.transition = None;
            self.diagnostics.state = if enabled {
                State::Warming
            } else {
                State::Normal
            };
            return (normal, false);
        };
        let host_age = now.saturating_duration_since(m.received_at).as_secs_f64();
        let age = self.clock.as_ref().and_then(|c| c.age(m, now));
        self.diagnostics.excess_age_ms = age.map(|a| (a - host_age).max(0.) * 1000.);
        if enabled && (host_age.max(age.unwrap_or(0.)) >= max_age.as_secs_f64()) {
            self.was_ready = false;
            self.transition = None;
            self.diagnostics.state = State::Stale;
            return (presented, true);
        }
        let issue = if self.clock.as_ref().is_none_or(|c| c.stable) {
            self.issue
        } else {
            State::ClockUnstable
        };
        let ready = enabled
            && age.is_some()
            && issue == State::Warming
            && self.diagnostics.residual_degrees.is_some();
        let mut target = normal;
        if ready {
            let mut horizon = if self.quiet {
                0.
            } else {
                age.unwrap_or(0.).min(MAX_PREDICTION)
            };
            if let Some(w) = m.gyro {
                let speed = norm(w);
                if speed > 1e-12 {
                    horizon = horizon.min(MAX_ROTATION_DEG.to_radians() / speed);
                }
                target = self
                    .reference
                    .conjugate()
                    .multiply(integrate(m.physical, w, horizon))
                    .normalized();
                self.diagnostics.prediction_ms = horizon * 1000.;
                self.diagnostics.prediction_degrees = speed * horizon * 180. / std::f64::consts::PI;
            }
        }
        self.diagnostics.state = if !enabled {
            State::Normal
        } else if ready {
            if self.quiet {
                State::Quiet
            } else {
                State::Predicting
            }
        } else {
            issue
        };
        if ready != self.was_ready {
            self.was_ready = ready;
            self.transition = Some((now, presented));
        }
        if let Some((started, from)) = self.transition {
            let amount = (now.saturating_duration_since(started).as_secs_f64() / 0.05).min(1.);
            if amount >= 1. {
                self.transition = None;
            }
            (from.slerp(target, amount), false)
        } else {
            (target, false)
        }
    }
    pub const fn diagnostics(&self) -> Diagnostics {
        self.diagnostics
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn yaw(degrees: f64) -> Quaternion {
        let h = degrees.to_radians() / 2.;
        Quaternion([h.cos(), 0., 0., h.sin()])
    }
    fn motion(
        start: Instant,
        index: u64,
        time_ms: u64,
        angle: f64,
        speed: f64,
        delay_ms: u64,
    ) -> Motion {
        Motion {
            instance: 1,
            session: 1,
            reference: 1,
            sequence: index + 1,
            delivery: index + 1,
            device_ms: Some(time_ms),
            clock_epoch: 1,
            clock_kind: 2,
            received_at: start + Duration::from_millis(time_ms + delay_ms),
            physical: yaw(angle),
            gyro: Some([0., 0., speed.to_radians()]),
            acceleration: Some([0., 0., 1.]),
            profile: 0xe4,
            orientation_source: "Native quaternion",
        }
    }
    fn feed(e: &mut Enhancement, m: Motion, shown: Quaternion) {
        let mut input = Inbox::default();
        input.push(m);
        e.ingest(input, shown);
    }
    #[test]
    fn twenty_hz_prediction_reduces_error_with_delivery_jitter() {
        let start = Instant::now();
        let mut e = Enhancement::default();
        let mut normal = Quaternion::default();
        let mut enhanced = normal;
        let mut current = normal;
        let mut next = 0u64;
        let mut sum_normal = 0.;
        let mut sum_enhanced = 0.;
        let mut count = 0u32;
        for step in 0..2000u32 {
            let ms = u64::from(step) * 5;
            let now = start + Duration::from_millis(ms);
            while next * 50 + 20 + [0, 5, 15, 0][(next % 4) as usize] <= ms {
                current = yaw(seconds_ms(next * 50) * 90.);
                feed(
                    &mut e,
                    motion(
                        start,
                        next,
                        next * 50,
                        seconds_ms(next * 50) * 90.,
                        90.,
                        20 + [0, 5, 15, 0][(next % 4) as usize],
                    ),
                    enhanced,
                );
                next += 1;
            }
            let (target, frozen) = e.apply(
                current,
                enhanced,
                true,
                true,
                false,
                now,
                Duration::from_millis(100),
            );
            assert!(!frozen);
            let a = 1. - (-0.005_f64 / 0.010).exp();
            normal = normal.slerp(current, a);
            enhanced = enhanced.slerp(target, a);
            if step > 400 {
                let truth = yaw(seconds_ms(ms) * 90.);
                sum_normal += distance(truth, normal).powi(2);
                sum_enhanced += distance(truth, enhanced).powi(2);
                count += 1;
            }
            assert!(e.diagnostics().prediction_degrees <= 5. + 1e-8);
        }
        let rms_normal = (sum_normal / f64::from(count)).sqrt();
        let rms_enhanced = (sum_enhanced / f64::from(count)).sqrt();
        eprintln!("20 Hz RMS: ordinary={rms_normal:.4} deg enhanced={rms_enhanced:.4} deg");
        assert!(
            rms_enhanced < rms_normal * 0.8,
            "{rms_enhanced} / {rms_normal}"
        );
        assert!(e.diagnostics().clock_ready);
    }
    #[test]
    fn stationary_noise_freezes_and_mode_off_preserves_the_original_path() {
        let start = Instant::now();
        let mut e = Enhancement::default();
        let q = Quaternion::default();
        for i in 0..30 {
            let m = motion(start, i, i * 50, 0., 0.2, 0);
            let now = m.received_at;
            feed(&mut e, m, q);
            let (result, _) = e.apply(q, q, true, true, false, now, Duration::from_millis(100));
            assert!(distance(result, q) < 0.1);
        }
        assert_eq!(e.diagnostics().state, State::Quiet);
        let now = start + Duration::from_millis(2000);
        let (result, frozen) = e.apply(
            q,
            yaw(12.),
            false,
            true,
            false,
            now,
            Duration::from_millis(100),
        );
        assert!(frozen);
        assert_eq!(result, yaw(12.));
        let (result, _) = e.apply(
            yaw(4.),
            q,
            true,
            false,
            false,
            now,
            Duration::from_millis(100),
        );
        assert_eq!(result, yaw(4.));
        assert_eq!(e.diagnostics().state, State::Normal);
    }
    #[test]
    fn rotation_cap_stop_reversal_and_same_sample_never_accumulate() {
        let start = Instant::now();
        let mut e = Enhancement::default();
        let mut shown = Quaternion::default();
        for i in 0..30 {
            let m = motion(start, i, i * 50, seconds_ms(i * 50) * 1000., 1000., 0);
            let at = m.received_at;
            feed(&mut e, m.clone(), shown);
            shown = e
                .apply(
                    m.physical,
                    shown,
                    true,
                    true,
                    false,
                    at,
                    Duration::from_millis(100),
                )
                .0;
        }
        let at = start + Duration::from_millis(1475);
        let shown = e
            .apply(
                yaw(1450.),
                shown,
                true,
                true,
                false,
                at,
                Duration::from_millis(100),
            )
            .0;
        assert!(e.diagnostics().prediction_degrees <= 5. + 1e-8);
        feed(&mut e, motion(start, 30, 1500, 1500., 0., 0), shown);
        e.apply(
            yaw(1500.),
            shown,
            true,
            true,
            false,
            start + Duration::from_millis(1520),
            Duration::from_millis(100),
        );
        assert!(e.diagnostics().prediction_ms <= 25.);
        let (a, frozen) = e.apply(
            yaw(1500.),
            shown,
            true,
            true,
            false,
            start + Duration::from_millis(1700),
            Duration::from_millis(100),
        );
        assert!(frozen);
        assert_eq!(a, shown);
        // The repeated old sequence cannot refresh reception or the clock model.
        feed(&mut e, motion(start, 30, 1700, 1500., 0., 0), shown);
        assert!(
            e.apply(
                yaw(1500.),
                shown,
                true,
                true,
                false,
                start + Duration::from_millis(1700),
                Duration::from_millis(100)
            )
            .1
        );
    }
    #[test]
    fn clock_epoch_history_overrun_missing_fields_and_impacts_degrade() {
        let start = Instant::now();
        let mut e = Enhancement::default();
        let q = Quaternion::default();
        for i in 0..15 {
            feed(&mut e, motion(start, i, i * 50, 0., 0., 0), q);
        }
        let mut m = motion(start, 15, 750, 0., 0., 0);
        m.clock_epoch = 2;
        feed(&mut e, m, q);
        e.apply(
            q,
            q,
            true,
            true,
            false,
            start + Duration::from_millis(750),
            Duration::from_millis(100),
        );
        assert!(!e.diagnostics().clock_ready);
        let mut input = Inbox {
            overrun: 3,
            ..Inbox::default()
        };
        input.push(motion(start, 16, 800, 0., 0., 0));
        e.ingest(input, q);
        assert_eq!(e.diagnostics().overruns, 3);
        let mut m = motion(start, 17, 850, 0., 0., 0);
        m.device_ms = None;
        feed(&mut e, m, q);
        e.apply(
            q,
            q,
            true,
            true,
            false,
            start + Duration::from_millis(850),
            Duration::from_millis(100),
        );
        assert_eq!(e.diagnostics().state, State::MissingTimestamp);
        let mut m = motion(start, 18, 900, 0., 0., 0);
        m.gyro = None;
        feed(&mut e, m, q);
        e.apply(
            q,
            q,
            true,
            true,
            false,
            start + Duration::from_millis(900),
            Duration::from_millis(100),
        );
        assert_eq!(e.diagnostics().state, State::MissingGyro);
        let mut m = motion(start, 19, 950, 0., 0., 0);
        m.acceleration = Some([0., 0., 2.]);
        feed(&mut e, m, q);
        e.apply(
            q,
            q,
            true,
            true,
            false,
            start + Duration::from_millis(950),
            Duration::from_millis(100),
        );
        assert_eq!(e.diagnostics().state, State::Impact);
        e.recenter(
            start + Duration::from_millis(950),
            Duration::from_millis(100),
        );
        assert!(e.measured().is_some());
    }
    #[test]
    fn clock_uses_latest_sample_per_batch_and_detects_unreasonable_drift() {
        let start = Instant::now();
        let mut clock = Clock::new(0, start);
        for i in 0..1600 {
            let mut m = motion(start, i, i * 5, 0., 0., 0);
            m.delivery = i / 8;
            m.received_at = start + Duration::from_millis((i / 8) * 40 + 35);
            assert!(clock.observe(&m, i % 8 == 7));
        }
        assert!(clock.ready());
        assert!(clock.anchors.len() <= 200);
        let last = motion(start, 1599, 7995, 0., 0., 0);
        assert!(
            clock
                .age(&last, start + Duration::from_millis(8000))
                .unwrap()
                < 0.01
        );
        let mut bad = Clock::new(0, start);
        for i in 0..200 {
            let mut m = motion(start, i, i * 50, 0., 0., 0);
            m.received_at = start + Duration::from_millis(i * 51);
            bad.observe(&m, true);
        }
        assert!(!bad.ready());
    }

    #[test]
    fn batched_twenty_hz_compound_rotation_improves_against_independent_truth() {
        let start = Instant::now();
        // Constant body rotation about a non-cardinal axis: closed-form truth,
        // independent of the predictor's integration and Euler conversions.
        let axis = [0.36, -0.48, 0.8];
        let truth = |ms: u64| {
            let half = seconds_ms(ms) * 90f64.to_radians() / 2.;
            Quaternion([
                half.cos(),
                axis[0] * half.sin(),
                axis[1] * half.sin(),
                axis[2] * half.sin(),
            ])
        };
        let mut enhanced = Enhancement::default();
        feed(
            &mut enhanced,
            motion(start, 0, 0, 0., 90., 0),
            Quaternion::default(),
        );
        let mut shown = Quaternion::default();
        let mut measured = shown;
        let mut next = 2u64;
        let mut ordinary_error = 0.;
        let mut enhanced_error = 0.;
        for ms in (5..10_000u64).step_by(5) {
            let delay = 15 + (next % 3) * 5;
            if ms >= next * 50 + delay {
                let mut inbox = Inbox::default();
                for index in [next - 1, next] {
                    let mut m = motion(start, index, index * 50, 0., 0., 0);
                    m.physical = truth(index * 50);
                    m.gyro = Some(axis.map(|v| v * 90f64.to_radians()));
                    m.delivery = next;
                    m.received_at = start + Duration::from_millis(next * 50 + delay);
                    inbox.push(m);
                }
                enhanced.ingest(inbox, shown);
                measured = truth(next * 50);
                next += 2;
            }
            shown = enhanced
                .apply(
                    measured,
                    shown,
                    true,
                    true,
                    false,
                    start + Duration::from_millis(ms),
                    Duration::from_millis(150),
                )
                .0;
            if ms > 1000 {
                ordinary_error += distance(truth(ms), measured).powi(2);
                enhanced_error += distance(truth(ms), shown).powi(2);
            }
        }
        assert!(
            enhanced_error < ordinary_error * 0.8f64.powi(2),
            "batched RMS ratio {}",
            (enhanced_error / ordinary_error).sqrt()
        );
    }

    #[test]
    fn switches_recenter_hold_and_clock_staleness_use_measured_pose() {
        let start = Instant::now();
        let mut e = Enhancement::default();
        let max_age = Duration::from_millis(100);
        for i in 0..20 {
            feed(
                &mut e,
                motion(start, i, i * 50, seconds_ms(i * 50) * 90., 90., 0),
                yaw(0.),
            );
        }
        let now = start + Duration::from_millis(975);
        let normal = yaw(85.5);
        // Enable and disable start from exactly the presented rotation.
        assert_eq!(
            e.apply(normal, normal, true, true, false, now, max_age).0,
            normal
        );
        let predicted = e
            .apply(
                normal,
                normal,
                true,
                true,
                false,
                now + Duration::from_millis(50),
                max_age,
            )
            .0;
        assert!((distance(predicted, normal) - 2.25).abs() < 1e-5);
        assert_eq!(
            e.apply(
                normal,
                predicted,
                true,
                false,
                false,
                now + Duration::from_millis(51),
                max_age
            )
            .0,
            predicted
        );
        assert_eq!(
            e.apply(normal, predicted, true, true, true, now, max_age).0,
            predicted
        );
        e.recenter(now, max_age);
        assert_eq!(e.reference, e.measured().unwrap());
        // Delayed arrivals have zero reception age but a stale mapped age.
        let delayed = motion(start, 20, 1000, 90., 90., 200);
        let now = delayed.received_at;
        feed(&mut e, delayed, predicted);
        assert!(e.expired(now, max_age));
        let reference = e.reference;
        e.recenter(now, max_age);
        assert_eq!(e.reference, reference);
        let (result, frozen) = e.apply(normal, predicted, true, true, false, now, max_age);
        assert!(frozen);
        assert_eq!(result, predicted);
        assert!(e.diagnostics.prediction_ms.abs() < f64::EPSILON);
        // A timestamp duplicate cannot keep a trusted model alive.
        let duplicate = motion(start, 21, 1000, 90., 90., 210);
        feed(&mut e, duplicate, predicted);
        assert!(e.clock.is_none());
        assert_eq!(e.issue, State::ClockUnstable);
    }
}
