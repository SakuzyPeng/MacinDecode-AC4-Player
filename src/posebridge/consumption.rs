use crate::head_tracking::Quaternion;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Sample {
    pub instance: u64,
    pub session: u64,
    pub reference: u64,
    pub sequence: u64,
    pub angles: [f32; 3],
    pub fresh: bool,
    pub age: Duration,
    pub queried_at: Instant,
}
impl Sample {
    pub fn valid_at(&self, now: Instant, max_age: Duration) -> bool {
        self.fresh
            && self.angles.iter().all(|v| v.is_finite())
            && self
                .age
                .saturating_add(now.saturating_duration_since(self.queried_at))
                < max_age
    }
}
#[derive(Default)]
pub struct Consumer {
    identity: Option<(u64, u64, u64)>,
    sequence: u64,
    reference: Quaternion,
    pub active: bool,
    pub goal: Quaternion,
}
impl Consumer {
    /// New physical samples (even identical angles) refresh the render keepalive.
    /// A repeated query only advances expiry, never sampling or recovery.
    pub fn update(
        &mut self,
        sample: Option<&Sample>,
        now: Instant,
        max_age: Duration,
        presented: Quaternion,
        recenter: bool,
    ) -> bool {
        let Some(sample) = sample.filter(|p| p.valid_at(now, max_age)) else {
            self.active = false;
            self.goal = presented;
            return false;
        };
        let identity = (sample.instance, sample.session, sample.reference);
        let changed = self.identity != Some(identity);
        let raw = Quaternion::from_euler(sample.angles);
        if changed {
            self.reference = raw.multiply(presented.conjugate()).normalized();
            self.identity = Some(identity);
            self.sequence = 0;
        }
        if recenter && (changed || sample.sequence >= self.sequence) {
            self.reference = raw;
            self.goal = Quaternion::default();
        }
        if !changed && sample.sequence <= self.sequence {
            return false;
        }
        self.sequence = sample.sequence;
        self.active = true;
        self.goal = self.reference.conjugate().multiply(raw).normalized();
        true
    }
    pub fn has_reference(&self) -> bool {
        self.identity.is_some()
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(now: Instant) -> Sample {
        Sample {
            instance: 1,
            session: 1,
            reference: 1,
            sequence: 1,
            angles: [30., 20., 10.],
            fresh: true,
            age: Duration::ZERO,
            queried_at: now,
        }
    }
    fn same(a: Quaternion, b: Quaternion) {
        let dot =
            a.0.into_iter()
                .zip(b.0)
                .map(|(x, y)| x * y)
                .sum::<f64>()
                .abs();
        assert!((1.0 - dot).abs() < 1e-6, "{a:?} != {b:?}");
    }
    #[test]
    fn expiry_precedes_dedup_and_duplicates_cannot_resume() {
        let now = Instant::now();
        let max = Duration::from_millis(100);
        let mut p = sample(now);
        let mut c = Consumer::default();
        let shown = Quaternion::from_euler([15., 0., 0.]);
        assert!(c.update(Some(&p), now, max, shown, false));
        same(c.goal, shown);
        assert!(!c.update(Some(&p), now + max, max, shown, false));
        assert!(!c.active);
        assert!(c.has_reference());
        same(c.goal, shown);
        p.queried_at = now + max;
        assert!(!c.update(Some(&p), now + max, max, shown, false));
        assert!(!c.active);
        p.sequence += 1;
        assert!(c.update(Some(&p), now + max, max, shown, false));
        assert!(c.active);
    }
    #[test]
    fn new_reference_preserves_presentation_and_recenter_uses_current_sample() {
        let now = Instant::now();
        let max = Duration::from_millis(100);
        let mut p = sample(now);
        let mut c = Consumer::default();
        let shown = Quaternion::from_euler([-179., 20., 10.]);
        c.update(Some(&p), now, max, shown, false);
        for angles in [
            [179., 0., 0.],
            [-179., 0., 0.],
            [40., 85., -20.],
            [40., -85., -20.],
        ] {
            p.reference += 1;
            p.angles = angles;
            assert!(c.update(Some(&p), now, max, shown, false));
            same(c.goal, shown);
        }
        assert!(!c.update(Some(&p), now, max, shown, true));
        same(c.goal, Quaternion::default());
        p.session += 1;
        assert!(c.update(Some(&p), now, max, shown, false));
        same(c.goal, shown);
    }
    #[test]
    fn host_queue_time_counts_and_stopped_pose_is_never_fresh() {
        let now = Instant::now();
        let mut p = sample(now);
        p.age = Duration::from_millis(99);
        assert!(p.valid_at(now, Duration::from_millis(100)));
        assert!(!p.valid_at(now + Duration::from_millis(1), Duration::from_millis(100)));
        let mut consumer = Consumer::default();
        consumer.reset();
        assert!(!consumer.has_reference());
        p.age = Duration::ZERO;
        p.fresh = false;
        assert!(!p.valid_at(now, Duration::from_millis(500)));
    }
}
