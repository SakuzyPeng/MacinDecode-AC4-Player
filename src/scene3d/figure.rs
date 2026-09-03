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
    Hat,
    Eye,
    Facing,
}

/// One axis-aligned box in world space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Part {
    pub centre: [f32; 3],
    pub size: [f32; 3],
    pub tone: PartTone,
}

/// Listener pose.
///
/// There is no body yaw. The view is listener-relative — which is what Windows
/// Spatial Audio actually does — so the body always faces the room's front, and
/// an axis-aligned box cannot be rotated without ceasing to be axis-aligned
/// anyway. A world-locked mode would need both, and would have to give up AABBs
/// for the body; that is deliberately out of scope here.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Figure {
    /// Head tracking, degrees. Zero until the tracking path lands.
    pub head_yaw: f32,
    /// Head tracking, degrees; positive looks up.
    pub head_pitch: f32,
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
    /// Resolve the standing pose into world-space boxes on `floor_y`.
    #[must_use]
    pub fn parts(self, floor_y: f32) -> [Part; PART_COUNT] {
        // One model unit. The 16-unit outer shoulder span, not the 32-unit full
        // height, is what determines it.
        let u = params::FIGURE_SHOULDER_WIDTH / 16.0;
        let leg_height = 12.0 * u;
        let torso_height = 12.0 * u;
        let head_height = 8.0 * u;

        let leg_centre = floor_y + leg_height / 2.0;
        let torso_centre = floor_y + leg_height + torso_height / 2.0;
        // The neck: where the head turns, and Minecraft's own pivot.
        let neck = floor_y + leg_height + torso_height;
        let head_centre = neck + head_height / 2.0;

        let leg = [4.0 * u, leg_height, 4.0 * u];
        let arm = [4.0 * u, torso_height, 4.0 * u];

        // Facing cues rotate about the neck; the head and hat are centred on
        // that axis, so rotation leaves them where they are and they stay
        // axis-aligned.
        //
        // Their depth is derived from the hat's front plane rather than written
        // down, because both ways of getting it wrong look like a modelling
        // mistake: the hat is opaque here — Minecraft's is a texture with
        // transparency — so a cue set behind the plane is swallowed whole and
        // the figure has no readable front, while one set clear of it floats off
        // the face with daylight behind it.
        let eye_left = self.about_neck([-1.7, 4.9, cue_depth(EYE_EDGE)], u, neck);
        let eye_right = self.about_neck([1.7, 4.9, cue_depth(EYE_EDGE)], u, neck);
        let wedge = self.about_neck([0.0, 2.4, cue_depth(WEDGE_EDGE)], u, neck);

        [
            Part {
                centre: [-2.0 * u, leg_centre, 0.0],
                size: leg,
                tone: PartTone::Cloth,
            },
            Part {
                centre: [2.0 * u, leg_centre, 0.0],
                size: leg,
                tone: PartTone::Cloth,
            },
            Part {
                centre: [0.0, torso_centre, 0.0],
                size: [8.0 * u, torso_height, 4.0 * u],
                tone: PartTone::Cloth,
            },
            Part {
                centre: [-6.0 * u, torso_centre, 0.0],
                size: arm,
                tone: PartTone::Skin,
            },
            Part {
                centre: [6.0 * u, torso_centre, 0.0],
                size: arm,
                tone: PartTone::Skin,
            },
            Part {
                centre: [0.0, head_centre, 0.0],
                size: [head_height; 3],
                tone: PartTone::Skin,
            },
            // The hat layer: the head shell inflated by half a unit a side. It
            // is where a headphone band would eventually hang.
            Part {
                centre: [0.0, head_centre, 0.0],
                size: [9.0 * u; 3],
                tone: PartTone::Hat,
            },
            Part {
                centre: eye_left,
                size: [EYE_EDGE * u; 3],
                tone: PartTone::Eye,
            },
            Part {
                centre: eye_right,
                size: [EYE_EDGE * u; 3],
                tone: PartTone::Eye,
            },
            // An untextured cube has no front, so this wedge is what makes the
            // facing legible — and once head tracking lands it is the thing that
            // sweeps around the ring.
            Part {
                centre: wedge,
                size: [WEDGE_EDGE * u; 3],
                tone: PartTone::Facing,
            },
        ]
    }

    /// Take a point given in model units relative to the neck, apply the head's
    /// pitch then yaw, and return it in world space.
    fn about_neck(self, local: [f32; 3], unit: f32, neck: f32) -> [f32; 3] {
        let pitch = self.head_pitch.to_radians();
        let yaw = self.head_yaw.to_radians();

        // Pitch about X. The front is -Z, so a standard rotation raises the
        // face for a positive angle, matching the documented sense.
        let pitched = [
            local[0],
            local[1] * pitch.cos() - local[2] * pitch.sin(),
            local[1] * pitch.sin() + local[2] * pitch.cos(),
        ];
        // Yaw about Y, chosen so that zero keeps the front pointing at -Z.
        let rotated = [
            pitched[0] * yaw.cos() + pitched[2] * yaw.sin(),
            pitched[1],
            -pitched[0] * yaw.sin() + pitched[2] * yaw.cos(),
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

    /// Require all three dimensions when classifying a voxel as a cube.
    fn is_cube(part: &Part) -> bool {
        close(part.size[0], part.size[1]) && close(part.size[1], part.size[2])
    }

    fn parts() -> [Part; PART_COUNT] {
        Figure::default().parts(FLOOR)
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
        }
        .parts(FLOOR);
        let turned_wedge = turned[PART_COUNT - 1];
        assert!(turned_wedge.centre[0] < 0.0 && turned_wedge.centre[2].abs() < 1e-6);
    }

    #[test]
    fn head_yaw_moves_only_the_head_and_its_cues() {
        let still = parts();
        let turned = Figure {
            head_yaw: 55.0,
            head_pitch: 0.0,
        }
        .parts(FLOOR);

        for (before, after) in still.iter().zip(turned.iter()) {
            match before.tone {
                PartTone::Eye | PartTone::Facing => {
                    assert!(
                        !same_point(before.centre, after.centre),
                        "cue did not turn: {before:?}"
                    );
                }
                // The head and hat are centred on the neck axis, so turning
                // leaves them exactly where they were — and keeps them axis
                // aligned, which is why the box itself is never rotated.
                _ => assert!(
                    same_point(before.centre, after.centre),
                    "body moved: {before:?}"
                ),
            }
        }
    }

    #[test]
    fn head_pitch_raises_and_lowers_the_facing_wedge() {
        let level = Figure::default().parts(FLOOR)[PART_COUNT - 1].centre[1];
        let up = Figure {
            head_yaw: 0.0,
            head_pitch: 40.0,
        }
        .parts(FLOOR)[PART_COUNT - 1]
            .centre[1];
        let down = Figure {
            head_yaw: 0.0,
            head_pitch: -40.0,
        }
        .parts(FLOOR)[PART_COUNT - 1]
            .centre[1];
        assert!(up > level && level > down, "{down} / {level} / {up}");
    }

    #[test]
    fn facing_cues_are_welded_to_the_hat_and_still_protrude() {
        // Both ways of missing look like a modelling mistake, and one of them
        // shipped: the wedge's rear face sat half a unit clear of the shell, so
        // the nose floated in front of the face with daylight behind it.
        let parts = parts();
        let hat = parts
            .iter()
            .find(|part| part.tone == PartTone::Hat)
            .expect("hat");
        let hat_front = hat.centre[2] - hat.size[2] / 2.0;

        for cue in parts
            .iter()
            .filter(|part| matches!(part.tone, PartTone::Eye | PartTone::Facing))
        {
            let rear = cue.centre[2] + cue.size[2] / 2.0;
            let front = cue.centre[2] - cue.size[2] / 2.0;
            assert!(
                rear > hat_front,
                "cue floats clear of the head by {}: {cue:?}",
                hat_front - rear
            );
            assert!(
                front < hat_front,
                "cue is swallowed by the opaque hat shell: {cue:?}"
            );
        }
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
        assert!(hat.size[0] > head.size[0]);
    }
}
