//! Which sensor axis points where on the head, worked out from motion.
//!
//! The three combo boxes on the device page cannot be checked by reading them.
//! A wrong choice is only found by moving, and what it looks like is nodding
//! that reads as a sideways tilt — which is what happened to this project's own
//! first mounting, and is recorded in the manual because listening was the only
//! way it was caught.
//!
//! It is arithmetic instead. A physical motion turns about exactly one head
//! axis: a nod about right, a shake about up, a tilt about forward. The
//! mounting names, for each of those head directions, the sensor axis that
//! points along it. So when a nod shows up as a turn about some *other*
//! canonical axis, the entry it showed up on is holding the sensor axis the nod
//! really turned about — and that value is what the nod's own entry should
//! hold. Three motions name all three entries.
//!
//! Nothing here reaches a device, and nothing here writes a mounting: it
//! reports one, and the listener applies it.
use crate::head_tracking::Quaternion;

/// The head directions, in the order `Device::mounting` stores them. It is also
/// the canonical pose order — X right, Y forward, Z up — which is the whole
/// reason a motion's axis index can be used as a mounting index.
pub const AXIS_ORDER: [&str; 3] = ["Right", "Forward", "Up"];

/// How far a motion has to turn before it is allowed to name an axis. Below
/// this the reading is mostly the listener breathing.
pub const MINIMUM_DEGREES: f32 = 20.0;

/// How much of a turn has to be about its dominant axis. A nod that also shakes
/// names two axes equally well, which means it names neither.
pub const MINIMUM_PURITY: f32 = 0.85;

/// One of the three motions the check asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Motion {
    Nod,
    Shake,
    Tilt,
}

impl Motion {
    /// The order the check walks them in, which is the order they are easiest
    /// to perform without thinking about the previous one.
    pub const ALL: [Self; 3] = [Self::Nod, Self::Shake, Self::Tilt];

    /// The head axis this motion turns about, and so the mounting entry it
    /// identifies. Nodding is about right, shaking about up, tilting about
    /// forward.
    pub const fn axis(self) -> usize {
        match self {
            Self::Nod => 0,
            Self::Tilt => 1,
            Self::Shake => 2,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Nod => "Nod",
            Self::Shake => "Shake",
            Self::Tilt => "Tilt",
        }
    }

    /// The positive direction, because the sign is half the answer: a motion
    /// performed the other way names the same axis with the wrong sign, and
    /// comes back as a left-handed basis rather than as a silent error.
    pub const fn instruction(self) -> &'static str {
        match self {
            Self::Nod => "Look up, then return to level.",
            Self::Shake => "Turn to look left, then return to centre.",
            Self::Tilt => "Tip your head toward your right shoulder, then return.",
        }
    }

    /// The angle this motion reads on when the mounting is correct.
    pub const fn reads_as(self) -> &'static str {
        match self {
            Self::Nod => "Pitch",
            Self::Shake => "Yaw",
            Self::Tilt => "Roll",
        }
    }

    /// What a turn about `axis` is called, so a wrong result can say what it
    /// came out as rather than only that it was wrong.
    pub const fn angle_of(axis: usize) -> &'static str {
        match axis {
            0 => "Pitch",
            1 => "Roll",
            _ => "Yaw",
        }
    }
}

/// What a motion did to the presented pose.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Observation {
    /// The canonical axis turned most about: 0 right, 1 forward, 2 up.
    pub axis: usize,
    /// Whether the turn was in that axis' positive direction.
    pub positive: bool,
    pub degrees: f32,
    /// How much of the turn was about that one axis, `0..=1`.
    pub purity: f32,
}

impl Observation {
    /// Whether this is worth believing. A vague motion is refused rather than
    /// averaged in, because the result of this check is written into a setting.
    pub fn usable(self) -> bool {
        self.degrees >= MINIMUM_DEGREES && self.purity >= MINIMUM_PURITY
    }
}

/// The turn from `start` to `end`, read in the frame `start` was already in.
///
/// Body-relative rather than world-relative: the axis wanted is fixed to the
/// head, so it must not come back rotated by wherever the listener happened to
/// be facing when the motion began.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a bounded angle in degrees and a unit ratio"
)]
pub fn observe(start: Quaternion, end: Quaternion) -> Observation {
    let delta = start.conjugate().multiply(end).normalized();
    let [w, x, y, z] = delta.0;
    // A quaternion and its negation are the same rotation, so pick the half
    // that turns the short way. Otherwise the sign read off the axis would be
    // the sign of an arbitrary representative.
    let (w, vector) = if w < 0.0 {
        (-w, [-x, -y, -z])
    } else {
        (w, [x, y, z])
    };
    let norm = vector.iter().map(|v| v * v).sum::<f64>().sqrt();
    // atan2 rather than acos: near zero rotation the latter loses every digit.
    let degrees = (2.0 * norm.atan2(w)).to_degrees() as f32;
    let axis = (0..3).max_by(|&a, &b| vector[a].abs().total_cmp(&vector[b].abs()));
    let axis = axis.unwrap_or(0);
    Observation {
        axis,
        positive: vector[axis] >= 0.0,
        degrees,
        purity: if norm > 0.0 {
            (vector[axis].abs() / norm) as f32
        } else {
            0.0
        },
    }
}

/// The sensor axis each head direction turned out to be, as the motions find
/// them.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Calibration {
    found: [Option<i8>; 3],
}

impl Calibration {
    /// Record what `motion` revealed, under the mounting that was in use while
    /// it was performed.
    ///
    /// The mounting cannot change underneath this: the axis combo boxes are
    /// disabled while the device is tracking, which is the only time a motion
    /// can be observed at all.
    ///
    /// Reports whether the observation was clean enough to be used.
    pub fn record(&mut self, mounting: [i8; 3], motion: Motion, observation: Observation) -> bool {
        if !observation.usable() {
            return false;
        }
        let sensor_axis = mounting[observation.axis];
        self.found[motion.axis()] = Some(if observation.positive {
            sensor_axis
        } else {
            -sensor_axis
        });
        true
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The mounting the three motions describe, once they describe one.
    ///
    /// Every rejection here is a motion performed wrongly rather than a
    /// mounting that cannot exist, so each says what to do again.
    pub fn resolve(self) -> Result<[i8; 3], &'static str> {
        let [Some(right), Some(forward), Some(up)] = self.found else {
            return Err("Perform all three motions.");
        };
        let mounting = [right, forward, up];
        let mut seen = [false; 3];
        for axis in mounting {
            let Some(slot) = seen.get_mut(usize::from(axis.unsigned_abs()).wrapping_sub(1)) else {
                return Err("A motion named an axis that is not X, Y or Z.");
            };
            if *slot {
                return Err("Two motions named the same sensor axis. Repeat them one at a time.");
            }
            *slot = true;
        }
        if cross(unit(right), unit(forward)) != unit(up) {
            return Err("These three are left-handed. Check the motion directions and repeat.");
        }
        Ok(mounting)
    }
}

/// A signed axis index as a unit vector, so handedness is a cross product
/// rather than a table of the six valid orderings.
fn unit(axis: i8) -> [i8; 3] {
    let mut vector = [0; 3];
    if let Some(slot) = vector.get_mut(usize::from(axis.unsigned_abs()).wrapping_sub(1)) {
        *slot = axis.signum();
    }
    vector
}

fn cross(a: [i8; 3], b: [i8; 3]) -> [i8; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The identity mounting, so a test that is about the motion is not also
    /// about a mapping.
    const IDENTITY: [i8; 3] = [1, 2, 3];

    /// The mounting the manual records for this project's own WT901BLE68,
    /// reached after the identity one made nodding read as tilt.
    const RECORDED: [i8; 3] = [-2, 1, 3];

    fn turn(motion: Motion, degrees: f32) -> Quaternion {
        Quaternion::from_euler(match motion {
            Motion::Nod => [0.0, degrees, 0.0],
            Motion::Shake => [degrees, 0.0, 0.0],
            Motion::Tilt => [0.0, 0.0, degrees],
        })
    }

    #[test]
    fn each_motion_turns_about_its_own_axis_in_its_own_direction() {
        for motion in Motion::ALL {
            let observation = observe(Quaternion::default(), turn(motion, 35.0));
            assert_eq!(observation.axis, motion.axis(), "{motion:?}");
            assert!(observation.positive, "{motion:?}");
            assert!((observation.degrees - 35.0).abs() < 0.01, "{observation:?}");
            assert!(observation.purity > 0.999, "{observation:?}");
            assert!(observation.usable());
            let reversed = observe(Quaternion::default(), turn(motion, -35.0));
            assert_eq!(reversed.axis, motion.axis());
            assert!(!reversed.positive, "{motion:?}");
            assert!((reversed.degrees - 35.0).abs() < 0.01, "{reversed:?}");
        }
    }

    /// The axis has to be head-fixed. If it were read in world space, a
    /// listener who happened to be facing away from centre would nod and be
    /// told they had shaken.
    #[test]
    fn the_axis_is_read_from_where_the_motion_started() {
        for facing in [0.0, 90.0, -120.0, 179.0] {
            let start = Quaternion::from_euler([facing, 0.0, 0.0]);
            for motion in Motion::ALL {
                let observation = observe(start, start.multiply(turn(motion, 30.0)));
                assert_eq!(
                    observation.axis,
                    motion.axis(),
                    "{motion:?} facing {facing}"
                );
                assert!(observation.positive, "{motion:?} facing {facing}");
                assert!((observation.degrees - 30.0).abs() < 0.01);
            }
        }
    }

    #[test]
    fn a_motion_too_small_or_too_mixed_names_nothing() {
        let small = observe(Quaternion::default(), turn(Motion::Nod, 5.0));
        assert!(!small.usable(), "{small:?}");
        let mixed = observe(
            Quaternion::default(),
            Quaternion::from_euler([30.0, 30.0, 0.0]),
        );
        assert!(mixed.purity < MINIMUM_PURITY, "{mixed:?}");
        assert!(!mixed.usable());
        let mut calibration = Calibration::default();
        assert!(!calibration.record(IDENTITY, Motion::Nod, small));
        assert_eq!(calibration.resolve(), Err("Perform all three motions."));
        // A still listener is a zero rotation, not a division by zero.
        let still = observe(Quaternion::default(), Quaternion::default());
        assert!(still.purity.abs() < f32::EPSILON);
        assert!(!still.usable());
    }

    /// A mounting that is already right reports itself, so the check can say
    /// "nothing to change" rather than hand back a rearrangement of the same
    /// three axes.
    #[test]
    fn a_correct_mounting_resolves_to_itself() {
        for mounting in [IDENTITY, RECORDED, [3, -1, -2], [-1, -2, 3]] {
            let mut calibration = Calibration::default();
            for motion in Motion::ALL {
                let observation = observe(Quaternion::default(), turn(motion, 40.0));
                assert!(calibration.record(mounting, motion, observation));
            }
            assert_eq!(calibration.resolve(), Ok(mounting), "{mounting:?}");
        }
    }

    /// The case the manual records: the identity mounting was in use, and
    /// nodding came out as tilt. Feeding that back has to produce the mounting
    /// that was eventually found by ear.
    #[test]
    fn a_wrong_mounting_is_corrected_from_how_its_motions_came_out() {
        // Under IDENTITY the sensor is worn as RECORDED describes, so each
        // physical motion appears on whichever canonical axis IDENTITY assigns
        // to the sensor axis that motion really used.
        let mut calibration = Calibration::default();
        for motion in Motion::ALL {
            let wanted = RECORDED[motion.axis()];
            let appeared = IDENTITY
                .iter()
                .position(|entry| entry.abs() == wanted.abs())
                .expect("the sensor axis is one of the three");
            let aligned = IDENTITY[appeared].signum() == wanted.signum();
            assert!(calibration.record(
                IDENTITY,
                motion,
                Observation {
                    axis: appeared,
                    positive: aligned,
                    degrees: 40.0,
                    purity: 1.0,
                },
            ));
        }
        assert_eq!(calibration.resolve(), Ok(RECORDED));
    }

    #[test]
    fn a_reversed_or_repeated_motion_is_refused_rather_than_written() {
        let observation = |axis, positive| Observation {
            axis,
            positive,
            degrees: 40.0,
            purity: 1.0,
        };
        // Nodding down instead of up: the axis is right, the sign is not, and
        // the three no longer form a right-handed basis.
        let mut reversed = Calibration::default();
        reversed.record(IDENTITY, Motion::Nod, observation(0, false));
        reversed.record(IDENTITY, Motion::Shake, observation(2, true));
        reversed.record(IDENTITY, Motion::Tilt, observation(1, true));
        assert_eq!(
            reversed.resolve(),
            Err("These three are left-handed. Check the motion directions and repeat.")
        );
        // Two motions that both came out on the same axis describe no basis.
        let mut repeated = Calibration::default();
        repeated.record(IDENTITY, Motion::Nod, observation(0, true));
        repeated.record(IDENTITY, Motion::Shake, observation(0, true));
        repeated.record(IDENTITY, Motion::Tilt, observation(1, true));
        assert_eq!(
            repeated.resolve(),
            Err("Two motions named the same sensor axis. Repeat them one at a time.")
        );
        // A mounting entry left unchosen cannot name an axis.
        let mut unset = Calibration::default();
        unset.record([0, 2, 3], Motion::Nod, observation(0, true));
        unset.record([0, 2, 3], Motion::Shake, observation(2, true));
        unset.record([0, 2, 3], Motion::Tilt, observation(1, true));
        assert_eq!(
            unset.resolve(),
            Err("A motion named an axis that is not X, Y or Z.")
        );
        repeated.clear();
        assert_eq!(repeated, Calibration::default());
    }

    #[test]
    fn the_three_motions_name_three_directions_and_three_angles() {
        let axes: Vec<_> = Motion::ALL.map(Motion::axis).into();
        assert_eq!(axes.len(), 3);
        for (axis, name) in AXIS_ORDER.iter().enumerate() {
            assert!(axes.contains(&axis), "no motion turns about {name}");
            assert!(!name.is_empty());
            let motion = Motion::ALL[axes.iter().position(|a| *a == axis).unwrap()];
            assert_eq!(Motion::angle_of(axis), motion.reads_as());
        }
        for motion in Motion::ALL {
            assert!(!motion.label().is_empty());
            assert!(motion.instruction().ends_with('.'));
        }
    }
}
