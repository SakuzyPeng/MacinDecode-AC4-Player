//! Assembles the scene's geometry for one frame.
//!
//! Coordinates are the Windows listener space that `backend/source.rs` submits
//! to: `+X` right, `+Y` up, and **`-Z` is the front of the room**, because
//! `windows_render_state` maps Core/ADM `[x, y, z]` to `[x, z, -y]`. The LFE
//! slot and the `FRONT` label both live on the `-Z` wall, and the listener faces
//! that way, so the `FRONT` camera preset looks over the listener's shoulder.

use crate::theme;

use super::camera::Camera;
use super::mesh::{Layer, MeshBuilder, Rgb, ViewContext};
use super::params;

/// The room is the normalised object space, `[-1, 1]` on every axis.
const ROOM_EXTENT: f32 = 1.0;
/// Floor plane.
const FLOOR_Y: f32 = -ROOM_EXTENT;

/// A dynamic object to draw. Increment 4 fills these from the audio-thread
/// mirror; until then the caller supplies a fixed set.
#[derive(Debug, Clone, Copy)]
pub struct SceneObject {
    pub position: [f32; 3],
}

/// Build one frame of geometry into `mesh`, which is cleared first.
pub fn build(
    mesh: &mut MeshBuilder,
    camera: &Camera,
    viewport_height_points: f32,
    objects: &[SceneObject],
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
    for object in objects {
        add_object(mesh, object, &view);
    }
}

/// The twelve edges of the normalised cube, as hairlines. This is the theme's
/// 1px card border promoted into three dimensions.
fn add_room(mesh: &mut MeshBuilder, view: &ViewContext) {
    let colour = Rgb::from_color32(theme::BORDER).lerp(view.ink, 0.25);
    mesh.add_wire_box(
        [0.0; 3],
        [ROOM_EXTENT * 2.0; 3],
        colour,
        params::HAIRLINE_POINTS,
        view,
    );
}

/// The floor ruler.
///
/// Weighted in three tiers rather than drawn at one weight: at a single weight,
/// 8 and 16 divisions were indistinguishable, which means the division count was
/// carrying no information at all. Legibility comes from graduation — centre
/// axes heaviest, quarter lines medium, the rest faint — so an object's footprint
/// can be read off the grid instead of merely sitting on hatching.
fn add_floor_grid(mesh: &mut MeshBuilder, view: &ViewContext) {
    let divisions = params::ROOM_BLOCKS;
    let quarter = (divisions / 4).max(1);
    let faint = Rgb::from_color32(theme::BORDER).lerp(
        Rgb::from_color32(theme::MUTED),
        params::FLOOR_GRID_CONTRAST * 0.6,
    );
    let major = faint.lerp(view.ink, 0.22);
    let axis = faint.lerp(view.ink, 0.45);

    for step in 0..=divisions {
        let offset = -ROOM_EXTENT
            + f32::from(u16::try_from(step).unwrap_or(u16::MAX))
                * (ROOM_EXTENT * 2.0 / f32::from(u16::try_from(divisions).unwrap_or(u16::MAX)));
        let on_axis = step * 2 == divisions;
        let on_major = step % quarter == 0;
        let (colour, width) = if on_axis {
            (axis, params::HAIRLINE_POINTS * 1.4)
        } else if on_major {
            (major, params::HAIRLINE_POINTS)
        } else {
            (faint, params::HAIRLINE_POINTS * 0.6)
        };
        mesh.add_line(
            Layer::Decal,
            [offset, FLOOR_Y, -ROOM_EXTENT],
            [offset, FLOOR_Y, ROOM_EXTENT],
            colour,
            width,
            view,
        );
        mesh.add_line(
            Layer::Decal,
            [-ROOM_EXTENT, FLOOR_Y, offset],
            [ROOM_EXTENT, FLOOR_Y, offset],
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
    let [x, y, z] = object.position;
    let edge = params::OBJECT_EDGE;
    mesh.add_box(
        object.position,
        [edge; 3],
        Rgb::from_color32(theme::ACCENT),
        view,
    );

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

#[cfg(test)]
mod tests {
    use super::*;

    fn built(objects: &[SceneObject]) -> MeshBuilder {
        let mut mesh = MeshBuilder::default();
        build(&mut mesh, &Camera::default(), 600.0, objects);
        mesh
    }

    #[test]
    fn an_empty_scene_still_draws_the_room_and_its_ruler() {
        let mesh = built(&[]);
        assert!(mesh.solid.is_empty(), "no objects means no solid geometry");
        assert_eq!(mesh.line.len(), 12 * 6, "twelve room edges");
        let grid_lines = (params::ROOM_BLOCKS as usize + 1) * 2;
        assert_eq!(mesh.decal.len(), grid_lines * 6);
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
        let empty = built(&[]);
        let one = built(&[object]);

        for vertex in &one.decal[empty.decal.len()..] {
            assert!(
                (vertex.position[1] - FLOOR_Y).abs() < f32::EPSILON,
                "footprint left the floor plane"
            );
            assert!((vertex.position[0] - object.position[0]).abs() <= params::OBJECT_EDGE);
            assert!((vertex.position[2] - object.position[2]).abs() <= params::OBJECT_EDGE);
        }
    }

    #[test]
    fn the_grid_is_graduated_rather_than_uniform() {
        // Three distinct widths must appear, otherwise the division count is the
        // only thing distinguishing the ruler and that was shown not to read.
        let mesh = built(&[]);
        let mut widths: Vec<f32> = mesh
            .decal
            .chunks(6)
            .map(|quad| {
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
