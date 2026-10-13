//! The listener: a Minecraft player model in voxels.
//!
//! Proportions are the canonical ones in model units — head 8³, torso 8x12x4,
//! arms and legs 4x12x4, 32 units from sole to crown. The outer 16-unit shoulder
//! span is the scale anchor, so including the standing lower body cannot make
//! the listener's head and shoulders smaller.
//!
//! **Limbs sit on the X axis.** The torso is 8 wide by 4 deep, so the shoulder
//! line runs along X and the body faces `-Z`. Arms belong at `x = ±6u` (torso
//! half-width 4 plus arm half-width 2) and legs at `x = ±2u`, both at `z = 0`.
//! Putting them on Z instead turns the figure ninety degrees against its own
//! torso and head: from the `BACK` preset one arm then floats in the middle of
//! the silhouette, and the whole figure reads side-on.
//!
//! The head is a separate box on a neck pivot. That is the entire reason for
//! choosing this model — head tracking becomes two floats, and the body does not
//! move.
//!
//! **Two poses, one set of joints.** The ordinary scene's listener stands. The
//! room view's sits, in Minecraft's own riding pose — the one anyone who has
//! played reads as sitting — because that pose only turns whole limbs about
//! their joints: no limb is cut, so a skin maps onto it unchanged. [`Pose`] is
//! the one description of those joints, and both this figure and
//! [`super::skin`] go through it, so a skin cannot sit differently from the
//! plain figure.

use super::params;

/// Boxes produced per figure: 7 model parts plus 2 eye insets and the facing
/// wedge. Fixed so the caller never allocates.
pub const PART_COUNT: usize = 10;

/// Which palette entry a part takes. The mapping lives in `scene.rs`, which owns
/// the theme; this module stays pure geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartTone {
    Skin,
    Cloth,
    /// The seat under a seated figure: furniture, not the listener.
    Seat,
    Hat,
    Eye,
    Facing,
}

/// One box in world space. `axes` are its local X/Y/Z unit vectors; body parts
/// use the identity basis, while the complete head assembly shares one rigid
/// rotation about the neck.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Part {
    pub centre: [f32; 3],
    pub size: [f32; 3],
    pub axes: [[f32; 3]; 3],
    pub tone: PartTone,
}

const IDENTITY_AXES: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

impl Part {
    const fn axis_aligned(centre: [f32; 3], size: [f32; 3], tone: PartTone) -> Self {
        Self {
            centre,
            size,
            axes: IDENTITY_AXES,
            tone,
        }
    }

    const fn oriented(
        centre: [f32; 3],
        size: [f32; 3],
        axes: [[f32; 3]; 3],
        tone: PartTone,
    ) -> Self {
        Self {
            centre,
            size,
            axes,
            tone,
        }
    }
}

/// How the body is held. The head assembly turns about the neck the same way in
/// both; the pose only decides where the limbs point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pose {
    #[default]
    Standing,
    /// Minecraft's riding pose, from `HumanoidModel`: legs pitched forward by
    /// 1.4137 rad and splayed outward by π/10, arms pitched forward by π/5.
    /// Minecraft also rolls each leg by 0.0785 rad; on a leg that already
    /// points forward that turns it about its own length, by 4.5°, and is left
    /// out.
    Seated,
}

/// The parts a pose can turn. The skin atlas's parts map onto the same ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Limb {
    Head,
    Body,
    RightArm,
    LeftArm,
    RightLeg,
    LeftLeg,
}

/// Leg pitch of the riding pose: 1.4137167 rad, as Minecraft writes it.
const SEATED_LEG_PITCH_DEGREES: f32 = 81.0;
/// Outward leg splay of the riding pose: π/10.
const SEATED_LEG_SPLAY_DEGREES: f32 = 18.0;
/// Arm pitch of the riding pose: π/5.
const SEATED_ARM_PITCH_DEGREES: f32 = 36.0;

impl Pose {
    /// A limb's joint, in model units from the neck, and the axes the pose
    /// holds the limb at. The joints are Minecraft's: shoulders two units
    /// below the neck, hips at the bottom of the torso. The figure's right is
    /// `+X`, because it faces `-Z`.
    pub(super) fn joint(self, limb: Limb) -> ([f32; 3], [[f32; 3]; 3]) {
        let (pivot, pitch, splay) = match limb {
            Limb::Head | Limb::Body => return ([0.0; 3], IDENTITY_AXES),
            Limb::RightArm => ([5.0, -2.0, 0.0], SEATED_ARM_PITCH_DEGREES, 0.0),
            Limb::LeftArm => ([-5.0, -2.0, 0.0], SEATED_ARM_PITCH_DEGREES, 0.0),
            Limb::RightLeg => (
                [2.0, -12.0, 0.0],
                SEATED_LEG_PITCH_DEGREES,
                -SEATED_LEG_SPLAY_DEGREES,
            ),
            Limb::LeftLeg => (
                [-2.0, -12.0, 0.0],
                SEATED_LEG_PITCH_DEGREES,
                SEATED_LEG_SPLAY_DEGREES,
            ),
        };
        match self {
            Self::Standing => (pivot, IDENTITY_AXES),
            Self::Seated => (pivot, limb_axes(pitch, splay)),
        }
    }

    /// A point on `limb`, in model units from the neck, where the pose puts it.
    ///
    /// Standing returns the point untouched rather than rotating it by the
    /// identity about its joint: the ordinary scene must not move by a rounding
    /// error because the room view learned to sit.
    pub(super) fn posed(self, limb: Limb, local: [f32; 3]) -> [f32; 3] {
        if self == Self::Standing {
            return local;
        }
        let (pivot, axes) = self.joint(limb);
        let offset: [f32; 3] = std::array::from_fn(|axis| local[axis] - pivot[axis]);
        let turned = orient(offset, axes);
        std::array::from_fn(|axis| pivot[axis] + turned[axis])
    }
}

/// A limb's axes after pitching it forward — its hanging end towards `-Z` —
/// and then turning it about the vertical. Positive splay swings the hanging
/// end towards `-X`, the figure's left.
fn limb_axes(pitch_degrees: f32, splay_degrees: f32) -> [[f32; 3]; 3] {
    let (pitch_sin, pitch_cos) = pitch_degrees.to_radians().sin_cos();
    let (splay_sin, splay_cos) = splay_degrees.to_radians().sin_cos();
    [
        [splay_cos, 0.0, -splay_sin],
        [pitch_sin * splay_sin, pitch_cos, pitch_sin * splay_cos],
        [pitch_cos * splay_sin, -pitch_sin, pitch_cos * splay_cos],
    ]
}

/// A vector given in a part's local axes, in world axes.
pub(super) fn orient(local: [f32; 3], axes: [[f32; 3]; 3]) -> [f32; 3] {
    std::array::from_fn(|axis| {
        axes[0][axis] * local[0] + axes[1][axis] * local[1] + axes[2][axis] * local[2]
    })
}

/// Where a figure stands or sits, and how large it is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stance {
    pub pose: Pose,
    /// One model unit, in world units.
    pub unit: f32,
    /// World height of the neck: the head's pivot, and the origin of every
    /// model-unit coordinate.
    pub neck: f32,
}

/// Boxes in the seat under a seated figure: seat, back and four legs.
pub const SEAT_PART_COUNT: usize = 6;

impl Stance {
    /// The ordinary scene's listener, standing on `floor_y` at the actor size
    /// [`params::FIGURE_SHOULDER_WIDTH`] fixes.
    pub fn standing(floor_y: f32) -> Self {
        let unit = params::FIGURE_SHOULDER_WIDTH / 16.0;
        Self {
            pose: Pose::Standing,
            unit,
            neck: floor_y + 24.0 * unit,
        }
    }

    /// Seated, with the head's centre — the ears — at the origin.
    pub fn seated(unit: f32) -> Self {
        Self {
            pose: Pose::Seated,
            unit,
            neck: -4.0 * unit,
        }
    }

    /// A point in model units from the neck, on `limb`, in world space.
    pub(super) fn place(self, limb: Limb, local: [f32; 3]) -> [f32; 3] {
        Figure::about_neck(
            self.pose.posed(limb, local),
            self.unit,
            self.neck,
            IDENTITY_AXES,
        )
    }

    /// The seat a seated figure rests on, down to `floor_y`.
    ///
    /// Shaped to the pose rather than to furniture: its top is exactly where
    /// the thighs' underside meets the hip joint, its back meets the torso's,
    /// and it stops below the shoulders so that from the default camera, which
    /// looks over the listener's left shoulder, it covers only the torso. It
    /// is not in the room model and has no acoustic effect.
    pub fn seat(self, floor_y: f32) -> [Part; SEAT_PART_COUNT] {
        const HIP: f32 = -12.0;
        const THICKNESS: f32 = 1.5;
        let top = HIP - 2.0 * SEATED_LEG_PITCH_DEGREES.to_radians().sin();
        let unit = self.unit;
        let world_y = |y: f32| self.neck + y * unit;
        let part = |x: [f32; 2], y: [f32; 2], z: [f32; 2]| {
            Part::axis_aligned(
                [
                    f32::midpoint(x[0], x[1]) * unit,
                    f32::midpoint(y[0], y[1]),
                    f32::midpoint(z[0], z[1]) * unit,
                ],
                [(x[1] - x[0]) * unit, y[1] - y[0], (z[1] - z[0]) * unit],
                PartTone::Seat,
            )
        };
        let seat_bottom = world_y(top - THICKNESS);
        let leg = |x: f32, z: f32| part([x, x + 1.0], [floor_y, seat_bottom], [z, z + 1.0]);
        [
            part([-6.0, 6.0], [seat_bottom, world_y(top)], [-3.0, 3.0]),
            part([-6.0, 6.0], [world_y(top), world_y(HIP + 8.0)], [2.0, 3.0]),
            leg(-6.0, -3.0),
            leg(5.0, -3.0),
            leg(-6.0, 2.0),
            leg(5.0, 2.0),
        ]
    }
}

/// Listener pose.
///
/// There is no body yaw. The view is listener-relative — which is what Windows
/// Spatial Audio actually does — so the body always faces the room's front, and
/// an axis-aligned box cannot be rotated without ceasing to be axis-aligned
/// anyway. A world-locked mode would need both, and would have to give up AABBs
/// for the body; that is deliberately out of scope here.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[allow(
    clippy::struct_field_names,
    reason = "only the head rotates; the figure's body remains fixed"
)]
pub struct Figure {
    /// Head tracking, degrees. Zero until the tracking path lands.
    pub head_yaw: f32,
    /// Head tracking, degrees; positive looks up.
    pub head_pitch: f32,
    pub head_roll: f32,
}

/// Half-depth of the hat shell in model units: the head's 8 units inflated by
/// half a unit a side.
const HAT_HALF_EXTENT: f32 = 4.5;

/// How far a facing cue's rear face is sunk into that shell. Small, but
/// deliberately not zero — flush against an opaque plane is the one setting that
/// cracks open into a visible gap under any rounding.
const CUE_EMBED: f32 = 0.15;

/// Edge of an eye voxel, in model units.
const EYE_EDGE: f32 = 1.3;
/// Edge of the facing wedge. Larger than an eye so it still reads at a distance.
const WEDGE_EDGE: f32 = 1.8;

/// Where a cue of edge `edge` has to sit so its rear face lands inside the hat
/// shell and the rest of it protrudes.
const fn cue_depth(edge: f32) -> f32 {
    -(HAT_HALF_EXTENT - CUE_EMBED + edge / 2.0)
}

impl Figure {
    /// Resolve the figure into world-space boxes in `stance`.
    #[must_use]
    pub fn parts_in(self, stance: Stance) -> [Part; PART_COUNT] {
        let u = stance.unit;
        let neck = stance.neck;
        let limb = |limb: Limb, centre: [f32; 3], size: [f32; 3], tone: PartTone| Part {
            centre: stance.place(limb, centre),
            size: size.map(|edge| edge * u),
            axes: stance.pose.joint(limb).1,
            tone,
        };

        // The complete head assembly shares one rigid transform. Rotating only
        // the cue centres is not enough: an axis-aligned eye can disappear
        // inside the opaque hat at a diagonal yaw, and pitching about the neck
        // moves the head centre itself. Keeping the same basis on all four boxes
        // makes their overlap invariant under both rotations.
        let head_axes = self.head_axes();
        let head_centre = Self::about_neck([0.0, 4.0, 0.0], u, neck, head_axes);

        // Cue depth is derived from the hat's local front plane rather than
        // written down. Its rear face stays slightly embedded while its front
        // protrudes, then the shared head transform carries that relationship
        // into world space.
        let eye_left = Self::about_neck([-1.7, 4.9, cue_depth(EYE_EDGE)], u, neck, head_axes);
        let eye_right = Self::about_neck([1.7, 4.9, cue_depth(EYE_EDGE)], u, neck, head_axes);
        let wedge = Self::about_neck([0.0, 2.4, cue_depth(WEDGE_EDGE)], u, neck, head_axes);

        [
            limb(
                Limb::LeftLeg,
                [-2.0, -18.0, 0.0],
                [4.0, 12.0, 4.0],
                PartTone::Cloth,
            ),
            limb(
                Limb::RightLeg,
                [2.0, -18.0, 0.0],
                [4.0, 12.0, 4.0],
                PartTone::Cloth,
            ),
            limb(
                Limb::Body,
                [0.0, -6.0, 0.0],
                [8.0, 12.0, 4.0],
                PartTone::Cloth,
            ),
            limb(
                Limb::LeftArm,
                [-6.0, -6.0, 0.0],
                [4.0, 12.0, 4.0],
                PartTone::Skin,
            ),
            limb(
                Limb::RightArm,
                [6.0, -6.0, 0.0],
                [4.0, 12.0, 4.0],
                PartTone::Skin,
            ),
            Part::oriented(head_centre, [8.0 * u; 3], head_axes, PartTone::Skin),
            // The hat layer: the head shell inflated by half a unit a side. It
            // is where a headphone band would eventually hang.
            Part::oriented(head_centre, [9.0 * u; 3], head_axes, PartTone::Hat),
            Part::oriented(eye_left, [EYE_EDGE * u; 3], head_axes, PartTone::Eye),
            Part::oriented(eye_right, [EYE_EDGE * u; 3], head_axes, PartTone::Eye),
            // An untextured cube has no front, so this wedge is what makes the
            // facing legible — and once head tracking lands it is the thing that
            // sweeps around the ring.
            Part::oriented(wedge, [WEDGE_EDGE * u; 3], head_axes, PartTone::Facing),
        ]
    }

    /// Local X/Y/Z axes after applying the head's pitch then yaw.
    pub(super) fn head_axes(self) -> [[f32; 3]; 3] {
        let pitch = self.head_pitch.to_radians();
        let yaw = self.head_yaw.to_radians();
        let (pitch_sin, pitch_cos) = pitch.sin_cos();
        let (yaw_sin, yaw_cos) = yaw.sin_cos();

        let base = [
            [yaw_cos, 0.0, -yaw_sin],
            [yaw_sin * pitch_sin, pitch_cos, yaw_cos * pitch_sin],
            [yaw_sin * pitch_cos, -pitch_sin, yaw_cos * pitch_cos],
        ];
        let (roll_sin, roll_cos) = self.head_roll.to_radians().sin_cos();
        [
            std::array::from_fn(|i| base[0][i] * roll_cos - base[1][i] * roll_sin),
            std::array::from_fn(|i| base[0][i] * roll_sin + base[1][i] * roll_cos),
            base[2],
        ]
    }

    /// Take a point given in model units relative to the neck, apply the head's
    /// basis, and return it in world space.
    pub(super) fn about_neck(
        local: [f32; 3],
        unit: f32,
        neck: f32,
        axes: [[f32; 3]; 3],
    ) -> [f32; 3] {
        let rotated = [
            axes[0][0] * local[0] + axes[1][0] * local[1] + axes[2][0] * local[2],
            axes[0][1] * local[0] + axes[1][1] * local[1] + axes[2][1] * local[2],
            axes[0][2] * local[0] + axes[1][2] * local[1] + axes[2][2] * local[2],
        ];
        [
            rotated[0] * unit,
            neck + rotated[1] * unit,
            rotated[2] * unit,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOOR: f32 = params::ROOM_FLOOR_Y;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    fn same_point(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b.iter()).all(|(x, y)| close(*x, *y))
    }

    fn same_axes(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> bool {
        a.into_iter()
            .zip(b)
            .all(|(left, right)| same_point(left, right))
    }

    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    /// Require all three dimensions when classifying a voxel as a cube.
    fn is_cube(part: &Part) -> bool {
        close(part.size[0], part.size[1]) && close(part.size[1], part.size[2])
    }

    fn parts() -> [Part; PART_COUNT] {
        Figure::default().parts_in(Stance::standing(FLOOR))
    }

    #[test]
    fn the_standing_figure_touches_the_floor_without_moving_the_head() {
        let unit = params::FIGURE_SHOULDER_WIDTH / 16.0;
        let parts = parts();
        let lowest = parts
            .iter()
            .map(|part| part.centre[1] - part.size[1] / 2.0)
            .fold(f32::INFINITY, f32::min);
        let highest = parts
            .iter()
            .filter(|part| part.tone != PartTone::Hat)
            .map(|part| part.centre[1] + part.size[1] / 2.0)
            .fold(f32::NEG_INFINITY, f32::max);

        assert!(
            (lowest - FLOOR).abs() < 1e-6,
            "feet left the floor: {lowest}"
        );

        let head = parts
            .iter()
            .find(|part| part.tone == PartTone::Skin && is_cube(part))
            .expect("head");
        assert!(
            same_point(head.centre, [0.0; 3]),
            "head left the acoustic origin: {head:?}"
        );
        assert!(
            close(highest - FLOOR, 32.0 * unit),
            "crown is at {highest}, expected {}",
            FLOOR + 32.0 * unit
        );
        assert!(
            close(highest - FLOOR, params::ROOM_HEIGHT * 2.0 / 3.0),
            "standing body no longer occupies two thirds of the room"
        );
    }

    #[test]
    fn limbs_sit_on_the_shoulder_axis_not_the_facing_axis() {
        // The bug this pins: limbs on Z put an arm between the camera and the
        // torso at the BACK preset, and the whole figure reads side-on.
        let unit = params::FIGURE_SHOULDER_WIDTH / 16.0;
        let parts = parts();

        let mut arms: Vec<f32> = parts
            .iter()
            .filter(|part| part.tone == PartTone::Skin && !is_cube(part))
            .map(|part| part.centre[0])
            .collect();
        arms.sort_by(f32::total_cmp);
        assert_eq!(arms.len(), 2);
        assert!(close(arms[0], -6.0 * unit) && close(arms[1], 6.0 * unit));

        let mut legs: Vec<f32> = parts
            .iter()
            .filter(|part| {
                part.tone == PartTone::Cloth
                    && close(part.size[0], 4.0 * unit)
                    && close(part.size[1], 12.0 * unit)
            })
            .map(|part| part.centre[0])
            .collect();
        legs.sort_by(f32::total_cmp);
        assert_eq!(legs.len(), 2);
        assert!(close(legs[0], -2.0 * unit) && close(legs[1], 2.0 * unit));

        for part in parts
            .iter()
            .filter(|part| matches!(part.tone, PartTone::Skin | PartTone::Cloth))
        {
            assert!(
                close(part.centre[2], 0.0),
                "a body part drifted onto the facing axis: {part:?}"
            );
        }
    }

    #[test]
    fn the_torso_is_wider_than_it_is_deep_so_the_body_faces_z() {
        let unit = params::FIGURE_SHOULDER_WIDTH / 16.0;
        let torso = parts()
            .into_iter()
            .find(|part| {
                part.tone == PartTone::Cloth
                    && close(part.size[0], 8.0 * unit)
                    && close(part.size[1], 12.0 * unit)
            })
            .expect("torso");
        assert!(torso.size[0] > torso.size[2], "{torso:?}");
    }

    #[test]
    fn zero_yaw_faces_away_from_the_back_preset() {
        // Front is -Z. The BACK preset puts the camera on +Z, so the listener's
        // back is correctly turned toward it.
        let wedge = parts()
            .into_iter()
            .find(|part| part.tone == PartTone::Facing)
            .expect("wedge");
        assert!(wedge.centre[2] < 0.0, "facing cue ended up behind the head");
        assert!(
            wedge.centre[0].abs() < 1e-6,
            "facing cue is off-centre at rest"
        );

        // A quarter turn swings it onto the -X wall, not the +X one.
        let turned = Figure {
            head_yaw: 90.0,
            head_pitch: 0.0,
            head_roll: 0.0,
        }
        .parts_in(Stance::standing(FLOOR));
        let turned_wedge = turned[PART_COUNT - 1];
        assert!(turned_wedge.centre[0] < 0.0 && turned_wedge.centre[2].abs() < 1e-6);
    }

    #[test]
    fn head_yaw_rotates_only_the_head_assembly() {
        let still = parts();
        let turned = Figure {
            head_yaw: 55.0,
            head_pitch: 0.0,
            head_roll: 0.0,
        }
        .parts_in(Stance::standing(FLOOR));

        for (before, after) in still[..5].iter().zip(&turned[..5]) {
            assert_eq!(before, after, "body moved: {before:?}");
        }
        for (before, after) in still[5..].iter().zip(&turned[5..]) {
            assert!(
                !same_axes(before.axes, after.axes),
                "head part did not rotate: {before:?}"
            );
        }
        for (before, after) in still[7..].iter().zip(&turned[7..]) {
            assert!(
                !same_point(before.centre, after.centre),
                "cue did not turn: {before:?}"
            );
        }
    }

    #[test]
    fn visual_head_basis_matches_the_audio_pose_including_roll() {
        let angles = [35.0, 20.0, 45.0];
        let figure = Figure {
            head_yaw: angles[0],
            head_pitch: angles[1],
            head_roll: angles[2],
        };
        let pose = crate::head_tracking::Quaternion::from_euler(angles).conjugate();
        let axes = figure.head_axes();
        for (actual, expected) in pose
            .rotate_listener([0.0, 1.0, 0.0])
            .into_iter()
            .zip(axes[1])
        {
            assert!((actual - expected).abs() < 0.0001);
        }
        for (actual, expected) in pose
            .rotate_listener([0.0, 0.0, -1.0])
            .into_iter()
            .zip(axes[2].map(|v| -v))
        {
            assert!((actual - expected).abs() < 0.0001);
        }
    }

    #[test]
    fn head_pitch_moves_the_complete_head_assembly() {
        let level = Figure::default().parts_in(Stance::standing(FLOOR));
        let up = Figure {
            head_yaw: 0.0,
            head_pitch: 40.0,
            head_roll: 0.0,
        }
        .parts_in(Stance::standing(FLOOR));
        let down = Figure {
            head_yaw: 0.0,
            head_pitch: -40.0,
            head_roll: 0.0,
        }
        .parts_in(Stance::standing(FLOOR));

        let wedge_y = [
            down[PART_COUNT - 1].centre[1],
            level[PART_COUNT - 1].centre[1],
            up[PART_COUNT - 1].centre[1],
        ];
        assert!(wedge_y[0] < wedge_y[1] && wedge_y[1] < wedge_y[2]);

        // Pitch is about the neck rather than the cube's own centre, so the
        // head and hat move with the cues instead of staying behind as a fixed
        // shell.
        assert!(up[5].centre[2] > level[5].centre[2]);
        assert!(down[5].centre[2] < level[5].centre[2]);
        for index in 5..PART_COUNT {
            assert!(!same_axes(level[index].axes, up[index].axes));
            assert!(!same_axes(level[index].axes, down[index].axes));
        }
    }

    #[test]
    fn facing_cues_are_welded_to_the_hat_and_still_protrude() {
        // The diagonal yaw and downward pitch are the two former failures: the
        // fixed AABB swallowed one eye around 26 degrees, while rotating only
        // the cue centres left every cue detached when looking down.
        for pose in [
            Figure::default(),
            Figure {
                head_yaw: 26.22,
                head_pitch: 0.0,
                head_roll: 0.0,
            },
            Figure {
                head_yaw: 0.0,
                head_pitch: -40.0,
                head_roll: 0.0,
            },
            Figure {
                head_yaw: 55.0,
                head_pitch: 35.0,
                head_roll: 0.0,
            },
        ] {
            let parts = pose.parts_in(Stance::standing(FLOOR));
            let hat = parts
                .iter()
                .find(|part| part.tone == PartTone::Hat)
                .expect("hat");

            for cue in parts
                .iter()
                .filter(|part| matches!(part.tone, PartTone::Eye | PartTone::Facing))
            {
                assert!(
                    same_axes(cue.axes, hat.axes),
                    "cue and hat do not share one rigid rotation at {pose:?}: {cue:?}"
                );
                let delta = [
                    cue.centre[0] - hat.centre[0],
                    cue.centre[1] - hat.centre[1],
                    cue.centre[2] - hat.centre[2],
                ];
                let local = hat.axes.map(|axis| dot(delta, axis));
                for (axis, &coordinate) in local.iter().enumerate().take(2) {
                    let cue_min = coordinate - cue.size[axis] / 2.0;
                    let cue_max = coordinate + cue.size[axis] / 2.0;
                    let hat_half = hat.size[axis] / 2.0;
                    assert!(
                        cue_min < hat_half && cue_max > -hat_half,
                        "cue misses the hat on local axis {axis} at {pose:?}: {cue:?}"
                    );
                }

                let hat_front = -hat.size[2] / 2.0;
                let rear = local[2] + cue.size[2] / 2.0;
                let front = local[2] - cue.size[2] / 2.0;
                assert!(
                    rear > hat_front,
                    "cue floats clear of the head by {} at {pose:?}: {cue:?}",
                    hat_front - rear
                );
                assert!(
                    front < hat_front,
                    "cue is swallowed by the opaque hat at {pose:?}: {cue:?}"
                );
            }
        }
    }

    fn seated() -> (Stance, [Part; PART_COUNT]) {
        let stance = Stance::seated(0.01);
        (stance, Figure::default().parts_in(stance))
    }

    /// A limb's far end: the one that hangs from its joint, along local -Y.
    fn hanging_end(part: &Part) -> [f32; 3] {
        std::array::from_fn(|axis| part.centre[axis] - part.axes[1][axis] * part.size[1] / 2.0)
    }

    /// A limb's joint end, along local +Y.
    fn joint_end(part: &Part) -> [f32; 3] {
        std::array::from_fn(|axis| part.centre[axis] + part.axes[1][axis] * part.size[1] / 2.0)
    }

    #[test]
    fn the_seated_figure_keeps_its_head_on_the_listening_position() {
        let (_, parts) = seated();
        assert!(same_point(parts[5].centre, [0.0; 3]), "{:?}", parts[5]);
        assert!(same_point(parts[6].centre, [0.0; 3]));
        // Head tracking still turns only the head assembly.
        let turned = Figure {
            head_yaw: 40.0,
            head_pitch: -10.0,
            head_roll: 5.0,
        }
        .parts_in(Stance::seated(0.01));
        assert_eq!(parts[..5], turned[..5]);
    }

    #[test]
    fn seated_legs_reach_forward_and_out_and_the_arms_forward() {
        let (stance, parts) = seated();
        let unit = stance.unit;
        let hip = stance.neck - 12.0 * unit;
        let (pitch_sin, pitch_cos) = 81.0_f32.to_radians().sin_cos();
        let (splay_sin, splay_cos) = 18.0_f32.to_radians().sin_cos();
        let reach = 12.0 * unit * pitch_sin;
        for (leg, side) in [(&parts[0], -1.0), (&parts[1], 1.0)] {
            // The joint stays at the hip; the foot goes forward, out and a
            // little down.
            assert!(same_point(joint_end(leg), [side * 2.0 * unit, hip, 0.0]));
            let foot = hanging_end(leg);
            assert!(close(foot[0], side * reach.mul_add(splay_sin, 2.0 * unit)));
            assert!(close(foot[1], (-12.0 * unit).mul_add(pitch_cos, hip)));
            assert!(close(foot[2], -reach * splay_cos));
        }
        for arm in &parts[3..5] {
            let hand = hanging_end(arm);
            assert!(hand[2] < 0.0, "the hand is not in front: {hand:?}");
            assert!(hand[1] > hip, "the hand reaches below the hip: {hand:?}");
        }
        // The torso does not move between the poses.
        let standing = Figure::default().parts_in(Stance {
            pose: Pose::Standing,
            ..stance
        });
        assert_eq!(parts[2], standing[2]);
    }

    #[test]
    fn the_seat_meets_the_thighs_at_the_hip_and_the_back_at_the_torso() {
        let (stance, parts) = seated();
        let floor = -0.4;
        let seat = stance.seat(floor);
        let top = seat[0].centre[1] + seat[0].size[1] / 2.0;
        for leg in &parts[..2] {
            // The lowest edge of the leg's joint end.
            let joint = joint_end(leg);
            let underside = [-1.0_f32, 1.0]
                .into_iter()
                .map(|z| joint[1] + leg.axes[2][1] * z * leg.size[2] / 2.0)
                .fold(f32::INFINITY, f32::min);
            assert!(close(underside, top), "{underside} vs {top}");
        }
        let torso = parts[2];
        let back = seat[1];
        assert!(close(
            torso.centre[2] + torso.size[2] / 2.0,
            back.centre[2] - back.size[2] / 2.0
        ));
        // It stops below the shoulder joints, two units under the neck.
        assert!(back.centre[1] + back.size[1] / 2.0 < 2.0f32.mul_add(-stance.unit, stance.neck));
        for leg in &seat[2..] {
            assert!(close(leg.centre[1] - leg.size[1] / 2.0, floor));
        }
        assert!(seat.iter().all(|part| part.tone == PartTone::Seat));
    }

    #[test]
    fn the_hat_layer_encloses_the_head() {
        let parts = parts();
        let head = parts
            .iter()
            .find(|part| part.tone == PartTone::Skin && is_cube(part))
            .expect("head");
        let hat = parts
            .iter()
            .find(|part| part.tone == PartTone::Hat)
            .unwrap();
        assert!(same_point(head.centre, hat.centre));
        assert!(same_axes(head.axes, hat.axes));
        assert!(hat.size[0] > head.size[0]);
    }
}
