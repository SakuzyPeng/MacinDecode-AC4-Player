//! Assembles the scene's geometry for one frame.
//!
//! Coordinates are the Windows listener space that `backend/source.rs` submits
//! to: `+X` right, `+Y` up, and **`-Z` is the front of the room**, because
//! `windows_render_state` maps Core/ADM `[x, y, z]` to `[x, z, -y]`. The LFE
//! slot lives on the `-Z` wall and the listener faces that way, so the `BACK`
//! camera preset at `+Z` correctly looks over the listener's shoulder.

use crate::theme;

use super::camera::Camera;
use super::figure::{Figure, PartTone};
use super::mesh::{Layer, MeshBuilder, Rgb, ViewContext};
use super::params;

/// Horizontal room extents. Logic uses a square footprint but a lower ceiling.
const ROOM_HALF_WIDTH: f32 = params::ROOM_WIDTH / 2.0;
const ROOM_HALF_DEPTH: f32 = params::ROOM_DEPTH / 2.0;
/// Vertical bounds are asymmetric so the standing listener's head, rather than
/// the centre of their full body, remains at the acoustic origin.
const FLOOR_Y: f32 = params::ROOM_FLOOR_Y;
const CEILING_Y: f32 = params::ROOM_CEILING_Y;

/// A dynamic object to draw, in normalized listener-relative coordinates.
/// Increment 4 fills these from the audio-thread mirror; until then the caller
/// supplies a fixed set. X/Z map onto the square footprint while elevation is
/// remapped around the head-height origin into the lower rectangular room.
#[derive(Debug, Clone, Copy)]
pub struct SceneObject {
    pub position: [f32; 3],
}

/// Everything one frame of the scene depends on.
#[derive(Debug, Clone, Copy, Default)]
pub struct SceneInput<'a> {
    pub objects: &'a [SceneObject],
    /// Whether the presentation carries an LFE element. The slot is drawn either
    /// way; only its occupancy changes.
    pub has_lfe: bool,
    pub figure: Figure,
}

/// Build one frame of geometry into `mesh`, which is cleared first.
pub fn build(
    mesh: &mut MeshBuilder,
    camera: &Camera,
    viewport_height_points: f32,
    input: SceneInput<'_>,
) {
    mesh.clear();
    let view = ViewContext {
        direction: camera.direction(),
        degeneracy: camera.degeneracy(),
        world_units_per_point: camera.world_units_per_point(viewport_height_points),
        ink: Rgb::from_color32(theme::INK),
        stage: Rgb::from_color32(theme::STAGE),
    };

    add_room(mesh, &view);
    add_floor_grid(mesh, &view);
    add_figure(mesh, input.figure, &view);
    add_lfe_slot(mesh, input.has_lfe, &view);
    for object in input.objects {
        add_object(mesh, object, &view);
    }
}

/// The listener.
///
/// Bases are mid-tone on purpose. Three-tone shading lerps toward `INK`, so a
/// base already near `INK` loses every distinction between faces and the figure
/// renders as one dark obelisk instead of a body. Graphite, well clear of the
/// terracotta the dynamic objects own.
fn add_figure(mesh: &mut MeshBuilder, figure: Figure, view: &ViewContext) {
    let muted = Rgb::from_color32(theme::MUTED);
    let skin = muted.lerp(Rgb::from_color32(theme::TEXT), 0.22);
    for part in figure.parts(FLOOR_Y) {
        let base = match part.tone {
            PartTone::Skin => skin,
            PartTone::Cloth => muted.lerp(view.ink, 0.50),
            // The hat encloses the head opaquely, so it *is* the head as far as
            // the eye is concerned: keep it near skin, or the figure reads as a
            // dark block balanced on a lighter body.
            PartTone::Hat => skin.lerp(view.ink, 0.16),
            PartTone::Eye => view.ink.lerp(Rgb::from_color32(theme::TEXT), 0.20),
            PartTone::Facing => Rgb::from_color32(theme::ACCENT),
        };
        mesh.add_box(part.centre, part.size, base, view);
    }
}

/// The LFE slot: a cabinet sunk into the front wall, centred, on the floor.
///
/// It gets no drop line, no gain halo and no trail. The absence of all three is
/// the signal that this element has no position — putting a cube out in the room
/// would claim one it does not have. The slot itself is permanent, so an absent
/// element leaves its outline and the layout stays learnable.
fn add_lfe_slot(mesh: &mut MeshBuilder, present: bool, view: &ViewContext) {
    let width = params::LFE_SLAB_WIDTH;
    let height = params::LFE_SLAB_HEIGHT;
    let depth = height * 0.65;
    let object_colour = Rgb::from_color32(theme::ACCENT);
    let centre = [
        0.0,
        FLOOR_Y + height / 2.0,
        -ROOM_HALF_DEPTH + depth * (1.0 - params::LFE_WALL_INSET),
    ];

    if present {
        mesh.add_box(centre, [width, height, depth], object_colour, view);
    } else {
        mesh.add_wire_box(
            centre,
            [width, height, depth],
            object_colour,
            params::HAIRLINE_POINTS * 0.8,
            view,
        );
    }
}

/// The twelve edges of the low rectangular room, as hairlines. This is the
/// theme's 1px card border promoted into three dimensions.
fn add_room(mesh: &mut MeshBuilder, view: &ViewContext) {
    let colour = Rgb::from_color32(theme::BORDER).lerp(view.ink, 0.25);
    mesh.add_wire_box(
        [0.0, (FLOOR_Y + CEILING_Y) / 2.0, 0.0],
        [params::ROOM_WIDTH, params::ROOM_HEIGHT, params::ROOM_DEPTH],
        colour,
        params::HAIRLINE_POINTS,
        view,
    );
}

/// The floor ruler.
///
/// Weighted rather than drawn at one weight. Legibility comes from graduation —
/// centre axes heaviest, quarter lines medium when the ruler is dense enough,
/// and the rest faint — so an object's footprint can be read off the grid instead
/// of merely sitting on hatching.
fn add_floor_grid(mesh: &mut MeshBuilder, view: &ViewContext) {
    let divisions = params::ROOM_GRID_DIVISIONS;
    let faint = Rgb::from_color32(theme::BORDER).lerp(
        Rgb::from_color32(theme::MUTED),
        params::FLOOR_GRID_CONTRAST * 0.6,
    );
    let major = faint.lerp(view.ink, 0.22);
    let axis = faint.lerp(view.ink, 0.45);

    for step in 0..=divisions {
        let fraction = f32::from(u16::try_from(step).unwrap_or(u16::MAX))
            / f32::from(u16::try_from(divisions).unwrap_or(u16::MAX));
        let x = -ROOM_HALF_WIDTH + fraction * params::ROOM_WIDTH;
        let z = -ROOM_HALF_DEPTH + fraction * params::ROOM_DEPTH;
        let on_axis = step * 2 == divisions;
        let on_major = step % params::GRID_SUBDIVISIONS_PER_BLOCK == 0;
        let (colour, width) = if on_axis {
            (axis, params::HAIRLINE_POINTS * 1.4)
        } else if on_major {
            (major, params::HAIRLINE_POINTS)
        } else {
            (faint, params::HAIRLINE_POINTS * 0.6)
        };
        mesh.add_line(
            Layer::Decal,
            [x, FLOOR_Y, -ROOM_HALF_DEPTH],
            [x, FLOOR_Y, ROOM_HALF_DEPTH],
            colour,
            width,
            view,
        );
        mesh.add_line(
            Layer::Decal,
            [-ROOM_HALF_WIDTH, FLOOR_Y, z],
            [ROOM_HALF_WIDTH, FLOOR_Y, z],
            colour,
            width,
            view,
        );
    }
}

/// One dynamic object: a solid cube, a hairline drop line to the floor, and the
/// footprint that drop line lands on.
///
/// The drop line and footprint are not decoration. In an orthographic view an
/// airborne cube's height and depth are ambiguous, and at an axis-aligned angle
/// the projection carries no depth at all — these two carry it instead.
fn add_object(mesh: &mut MeshBuilder, object: &SceneObject, view: &ViewContext) {
    let [x, y, z] = object_world_position(object.position);
    let edge = params::OBJECT_EDGE;
    mesh.add_box([x, y, z], [edge; 3], Rgb::from_color32(theme::ACCENT), view);

    let drop = Rgb::from_color32(theme::MUTED).lerp(Rgb::from_color32(theme::BORDER), 0.35);
    mesh.add_line(
        Layer::Line,
        [x, y - edge / 2.0, z],
        [x, FLOOR_Y, z],
        drop,
        params::HAIRLINE_POINTS * 0.8,
        view,
    );
    mesh.add_floor_mark(
        x,
        z,
        edge * 0.7,
        FLOOR_Y,
        Rgb::from_color32(theme::MUTED).lerp(view.stage, 0.4),
        view,
    );
}

/// Map the normalized acoustic coordinates into the display room. The floor is
/// farther below the head than the ceiling is above it, so elevation uses two
/// linear halves and keeps zero exactly at the listener's head.
fn object_world_position([x, y, z]: [f32; 3]) -> [f32; 3] {
    let elevation = y.clamp(-1.0, 1.0);
    let world_y = if elevation >= 0.0 {
        elevation * CEILING_Y
    } else {
        elevation * -FLOOR_Y
    };
    [
        x.clamp(-1.0, 1.0) * ROOM_HALF_WIDTH,
        world_y,
        z.clamp(-1.0, 1.0) * ROOM_HALF_DEPTH,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built(objects: &[SceneObject]) -> MeshBuilder {
        with_input(SceneInput {
            objects,
            ..SceneInput::default()
        })
    }

    fn with_input(input: SceneInput<'_>) -> MeshBuilder {
        let mut mesh = MeshBuilder::default();
        build(&mut mesh, &Camera::default(), 600.0, input);
        mesh
    }

    #[test]
    fn a_source_less_scene_is_still_a_room_a_ruler_and_a_listener() {
        // The point of the cross-platform view: DecodePhase::Unavailable now
        // shows someone standing in an empty room, not an empty text box.
        let mesh = built(&[]);
        let figure_faces = super::super::figure::PART_COUNT * 6 * 6;
        assert_eq!(
            mesh.solid.len(),
            figure_faces,
            "the listener and nothing else"
        );
        assert_eq!(
            mesh.line.len(),
            (12 + 12) * 6,
            "room edges plus the empty LFE slot"
        );
        let grid_lines = (params::ROOM_GRID_DIVISIONS as usize + 1) * 2;
        assert_eq!(mesh.decal.len(), grid_lines * 6);
    }

    #[test]
    fn the_room_is_a_low_cuboid_with_the_head_at_elevation_zero() {
        assert!(params::ROOM_HEIGHT < params::ROOM_WIDTH);
        assert!(params::ROOM_HEIGHT < params::ROOM_DEPTH);
        assert!((CEILING_Y - FLOOR_Y - params::ROOM_HEIGHT).abs() < 1e-6);
        assert!((params::FIGURE_SHOULDER_WIDTH * 3.0 - params::ROOM_HEIGHT).abs() < 1e-6);

        assert!((object_world_position([0.0, 0.0, 0.0])[1]).abs() < 1e-6);
        assert!((object_world_position([0.0, 1.0, 0.0])[1] - CEILING_Y).abs() < 1e-6);
        assert!((object_world_position([0.0, -1.0, 0.0])[1] - FLOOR_Y).abs() < 1e-6);
    }

    #[test]
    fn the_lfe_slot_is_drawn_whether_or_not_the_element_is_present() {
        let absent = built(&[]);
        let present = with_input(SceneInput {
            has_lfe: true,
            ..SceneInput::default()
        });
        // Occupied: a solid cabinet, and the outline is gone.
        assert_eq!(present.solid.len() - absent.solid.len(), 6 * 6);
        assert_eq!(absent.line.len() - present.line.len(), 12 * 6);
    }

    #[test]
    fn the_lfe_cabinet_sits_on_the_floor_against_the_front_wall() {
        let absent = built(&[]);
        let present = with_input(SceneInput {
            has_lfe: true,
            ..SceneInput::default()
        });
        let cabinet = &present.solid[absent.solid.len()..];

        let lowest = cabinet
            .iter()
            .map(|vertex| vertex.position[1])
            .fold(f32::INFINITY, f32::min);
        assert!((lowest - FLOOR_Y).abs() < 1e-6, "cabinet floats: {lowest}");

        // Front is -Z, so the cabinet must straddle the -Z wall, and it must be
        // wider than it is deep — the non-cubic shape is what says "not one of
        // the dynamic objects".
        let nearest = cabinet
            .iter()
            .map(|vertex| vertex.position[2])
            .fold(f32::INFINITY, f32::min);
        assert!(
            nearest <= -ROOM_HALF_DEPTH + 1e-6,
            "cabinet left the wall: {nearest}"
        );
        const { assert!(params::LFE_SLAB_WIDTH > params::LFE_SLAB_HEIGHT) };
    }

    #[test]
    fn every_object_brings_a_cube_a_drop_line_and_a_footprint() {
        let empty = built(&[]);
        let one = built(&[SceneObject {
            position: [0.3, 0.4, -0.2],
        }]);

        assert_eq!(one.solid.len() - empty.solid.len(), 6 * 6, "six cube faces");
        assert_eq!(one.line.len() - empty.line.len(), 6, "one drop line");
        assert_eq!(one.decal.len() - empty.decal.len(), 6, "one footprint");
    }

    #[test]
    fn the_footprint_lands_directly_under_the_cube_on_the_floor() {
        let object = SceneObject {
            position: [0.3, 0.4, -0.2],
        };
        let world = object_world_position(object.position);
        let empty = built(&[]);
        let one = built(&[object]);

        for vertex in &one.decal[empty.decal.len()..] {
            assert!(
                (vertex.position[1] - FLOOR_Y).abs() < f32::EPSILON,
                "footprint left the floor plane"
            );
            assert!((vertex.position[0] - world[0]).abs() <= params::OBJECT_EDGE);
            assert!((vertex.position[2] - world[2]).abs() <= params::OBJECT_EDGE);
        }
    }

    #[test]
    fn object_elevation_is_remapped_into_the_low_room() {
        let object = SceneObject {
            position: [0.3, 0.4, -0.2],
        };
        let expected = object_world_position(object.position);
        let empty = built(&[]);
        let one = built(&[object]);
        let cube = &one.solid[empty.solid.len()..];
        let lowest = cube
            .iter()
            .map(|vertex| vertex.position[1])
            .fold(f32::INFINITY, f32::min);
        let highest = cube
            .iter()
            .map(|vertex| vertex.position[1])
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(((lowest + highest) / 2.0 - expected[1]).abs() < 1e-6);
    }

    #[test]
    fn the_grid_is_graduated_rather_than_uniform() {
        // The 4×4 Logic ruler, its centre axes, and the internal fine grid must
        // remain three independently readable weights. Check the two line
        // directions separately: away from a symmetric 45° camera, the same
        // world-space width has a different component span on X and Z.
        let mesh = built(&[]);
        for direction in 0..2 {
            let mut widths: Vec<f32> = mesh
                .decal
                .chunks(6)
                .enumerate()
                .filter(|(index, _)| index % 2 == direction)
                .map(|(_, quad)| {
                    let span = (quad[1].position[0] - quad[0].position[0]).abs()
                        + (quad[1].position[2] - quad[0].position[2]).abs();
                    (span * 10_000.0).round() / 10_000.0
                })
                .collect();
            widths.sort_by(f32::total_cmp);
            widths.dedup();
            assert_eq!(widths.len(), 3, "expected three tiers, got {widths:?}");
        }
    }
}
