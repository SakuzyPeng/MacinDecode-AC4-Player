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
use super::mesh::{ArrowSpec, Layer, MeshBuilder, Rgb, ViewContext};
use super::params;

/// Horizontal room extents. Logic uses a square footprint but a lower ceiling.
const ROOM_HALF_WIDTH: f32 = params::ROOM_WIDTH / 2.0;
const ROOM_HALF_DEPTH: f32 = params::ROOM_DEPTH / 2.0;
/// Vertical bounds are asymmetric so the standing listener's head, rather than
/// the centre of their full body, remains at the acoustic origin.
const FLOOR_Y: f32 = params::ROOM_FLOOR_Y;
const CEILING_Y: f32 = params::ROOM_CEILING_Y;

/// A dynamic object to draw, in normalized listener-relative coordinates as the
/// render callback submitted them. X/Z map onto the square footprint while
/// elevation is remapped around the head-height origin into the lower
/// rectangular room.
#[derive(Debug, Clone, Copy, Default)]
pub struct SceneObject<'a> {
    pub position: [f32; 3],
    /// Whether the renderer is spatializing this element. An inactive one is
    /// not drawn at all — see [`build`].
    pub active: bool,
    /// Linear gain, which decides whether the object reads as sounding or as
    /// present but silent.
    pub gain: f32,
    /// Where this object has been, oldest first, in the same normalized
    /// coordinates as `position`.
    pub trail: &'a [[f32; 3]],
    /// Aligned with `trail`: which marks the object arrived at instantly rather
    /// than travelled to.
    pub trail_jumps: &'a [bool],
}

/// Everything one frame of the scene depends on.
#[derive(Debug, Clone, Copy, Default)]
pub struct SceneInput<'a> {
    pub objects: &'a [SceneObject<'a>],
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
    // An inactive element is one whose metadata is absent or incomplete, so the
    // only coordinate available for it is the origin — the listener's own head.
    // Drawing it there would assert a position it does not have, and assert it
    // in the one place guaranteed to look deliberate. Leaving it out is the
    // honest reading of "we do not know where this is".
    for object in input.objects.iter().filter(|object| object.active) {
        add_trail(mesh, object, &view);
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
        mesh.add_oriented_box(part.centre, part.size, part.axes, base, view);
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
        [0.0, f32::midpoint(FLOOR_Y, CEILING_Y), 0.0],
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
fn add_object(mesh: &mut MeshBuilder, object: &SceneObject<'_>, view: &ViewContext) {
    let [x, y, z] = object_world_position(object.position);
    let edge = params::OBJECT_EDGE;
    mesh.add_box([x, y, z], [edge; 3], object_colour(object, view), view);

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

/// An object's base colour: accent while it is audible, faded toward the stage
/// once it drops below the silence floor. Fading toward the ground is the same
/// recession air perspective uses, so silence does not need a second language.
fn object_colour(object: &SceneObject<'_>, view: &ViewContext) -> Rgb {
    let accent = Rgb::from_color32(theme::ACCENT);
    if object.gain >= params::OBJECT_SILENT_GAIN {
        accent
    } else {
        accent.lerp(view.stage, params::OBJECT_SILENT_FADE)
    }
}

/// The object's recent path, as discrete marks at a fixed time interval.
///
/// Because the interval is fixed, **the gap between marks is speed**: an OAMD
/// ramp comes out evenly spaced and a `ramp_frames == 0` jump as a single long
/// gap. A continuous ribbon would lose that and add a width channel carrying
/// nothing.
///
/// Each mark is projected onto the floor as well. That projection is not
/// decoration — at a grazing or axis-aligned view the airborne marks carry no
/// depth at all, and the projection is what still places the path on the grid.
fn add_trail(mesh: &mut MeshBuilder, object: &SceneObject<'_>, view: &ViewContext) {
    let Ok(count) = u16::try_from(object.trail.len()) else {
        return;
    };
    if count == 0 {
        return;
    }
    // Deliberately not tinted by the object's current gain. The mirror records
    // positions, not a gain history, so colouring past positions with the
    // present gain would repaint history with something that was not true when
    // the object was there. It also compounded with the age fade until a silent
    // object's trail was invisible. Age is the only thing a trail encodes.
    let colour = Rgb::from_color32(theme::ACCENT);
    let edge = params::OBJECT_EDGE * params::TRAIL_MARK_SCALE;

    for (index, point) in object.trail.iter().enumerate() {
        // Newest mark unfaded, oldest at TRAIL_FADE. This is what gives the
        // trail a direction without needing an arrowhead.
        let freshness = f32::from(u16::try_from(index).unwrap_or(u16::MAX).saturating_add(1))
            / f32::from(count);
        let faded = colour.lerp(view.stage, params::TRAIL_FADE * (1.0 - freshness));
        let [x, y, z] = object_world_position(*point);
        let arrival = trail_jump_at(object, index);
        let departure = trail_jump_at(object, index.saturating_add(1));
        if arrival || departure {
            // A jump's two ends are hollow, and unfaded. The gap between marks
            // reads as speed everywhere else in this trail, so left solid these
            // two would say the object crossed the room very fast rather than
            // that it was never in between. Unfaded because they are the marks
            // the eye is meant to find.
            jump_mark(mesh, [x, y, z], view);
        } else {
            mesh.add_box([x, y, z], [edge; 3], faded, view);
        }
        mesh.add_floor_mark(
            x,
            z,
            edge * 0.7,
            FLOOR_Y,
            faded.lerp(view.stage, 1.0 - params::FLOOR_TRAIL_WEIGHT),
            view,
        );
        if arrival && let Some(previous) = index.checked_sub(1) {
            jump_arrow(
                mesh,
                object_world_position(object.trail[previous]),
                [x, y, z],
                view,
            );
        }
    }
}

/// Edge of a jump's endpoint marker, in world units.
const JUMP_MARK_EDGE: f32 =
    params::OBJECT_EDGE * params::TRAIL_MARK_SCALE * params::JUMP_MARK_SCALE;

/// A jump's endpoint: hollow where a breadcrumb is solid, and a little larger.
fn jump_mark(mesh: &mut MeshBuilder, centre: [f32; 3], view: &ViewContext) {
    mesh.add_wire_box(
        centre,
        [JUMP_MARK_EDGE; 3],
        Rgb::from_color32(theme::ACCENT),
        params::HAIRLINE_POINTS,
        view,
    );
}

/// The direction the object went, drawn at the end it left.
///
/// This is the half of the annotation that answers "where did it go". The two
/// hollow marks say a jump happened, but with the connecting line gone — and it
/// has to be gone, because none of it was travelled — nothing else relates them.
fn jump_arrow(mesh: &mut MeshBuilder, from: [f32; 3], to: [f32; 3], view: &ViewContext) {
    let span = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let length = span[0]
        .mul_add(span[0], span[1].mul_add(span[1], span[2] * span[2]))
        .sqrt();
    if length < 1e-6 {
        return;
    }
    // Begin clear of the departure marker. Starting at its centre buries half
    // the shaft inside the wire box, and the two then read as one cluttered
    // glyph instead of a marker and a direction.
    let clearance = JUMP_MARK_EDGE * 0.75 / length;
    let start = [
        span[0].mul_add(clearance, from[0]),
        span[1].mul_add(clearance, from[1]),
        span[2].mul_add(clearance, from[2]),
    ];
    mesh.add_arrow(
        Layer::Line,
        start,
        to,
        Rgb::from_color32(theme::ACCENT),
        ArrowSpec {
            shaft_points: params::JUMP_ARROW_POINTS,
            head_points: params::JUMP_ARROW_HEAD_POINTS,
            head_degrees: params::JUMP_ARROW_HEAD_DEGREES,
            width_points: params::HAIRLINE_POINTS,
        },
        view,
    );
}

/// Whether the object arrived at trail `index` instantly, from far enough away
/// to be worth marking.
fn trail_jump_at(object: &SceneObject<'_>, index: usize) -> bool {
    let (Some(point), Some(previous)) = (
        object.trail.get(index),
        index.checked_sub(1).and_then(|i| object.trail.get(i)),
    ) else {
        // The oldest mark has no predecessor in view, so there is no jump to
        // draw even when the flag says one happened before it.
        return false;
    };
    object.trail_jumps.get(index).copied().unwrap_or(false) && travelled_far(*previous, *point)
}

/// Whether a discontinuity moved the object far enough to annotate.
///
/// Whether it *was* a discontinuity is settled in `backend::state` by the
/// bitstream itself. This is the separate, perceptual question: a stream that
/// sends instant updates for every small correction would otherwise turn the
/// whole trail into a chain of hollow marks, and nobody loses track of an object
/// that moved two hundredths of a room.
fn travelled_far(from: [f32; 3], to: [f32; 3]) -> bool {
    let span = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let squared = span[0].mul_add(span[0], span[1].mul_add(span[1], span[2] * span[2]));
    squared >= params::JUMP_MIN_DISTANCE * params::JUMP_MIN_DISTANCE
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

    /// An object the renderer is spatializing at full gain — the ordinary case
    /// the geometry tests care about.
    fn sounding(position: [f32; 3]) -> SceneObject<'static> {
        SceneObject {
            position,
            active: true,
            gain: 1.0,
            trail: &[],
            trail_jumps: &[],
        }
    }

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
        const { assert!(params::ROOM_HEIGHT < params::ROOM_WIDTH) };
        const { assert!(params::ROOM_HEIGHT < params::ROOM_DEPTH) };
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
        let one = built(&[sounding([0.3, 0.4, -0.2])]);

        assert_eq!(one.solid.len() - empty.solid.len(), 6 * 6, "six cube faces");
        assert_eq!(one.line.len() - empty.line.len(), 6, "one drop line");
        assert_eq!(one.decal.len() - empty.decal.len(), 6, "one footprint");
    }

    #[test]
    fn the_footprint_lands_directly_under_the_cube_on_the_floor() {
        let object = sounding([0.3, 0.4, -0.2]);
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
        let object = sounding([0.3, 0.4, -0.2]);
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
        assert!((f32::midpoint(lowest, highest) - expected[1]).abs() < 1e-6);
    }

    #[test]
    fn an_inactive_object_is_not_drawn_at_the_listener() {
        // An element without complete metadata has no position but the origin,
        // which is the listener's own head. Drawing it there would invent a
        // reading rather than withhold one.
        let empty = built(&[]);
        let inactive = built(&[SceneObject {
            active: false,
            ..sounding([0.3, 0.4, -0.2])
        }]);

        assert_eq!(inactive.solid.len(), empty.solid.len());
        assert_eq!(inactive.line.len(), empty.line.len());
        assert_eq!(inactive.decal.len(), empty.decal.len());
    }

    #[test]
    fn a_silent_object_keeps_its_geometry_but_fades_toward_the_stage() {
        let empty = built(&[]);
        let loud = built(&[sounding([0.3, 0.4, -0.2])]);
        let silent = built(&[SceneObject {
            gain: params::OBJECT_SILENT_GAIN / 2.0,
            ..sounding([0.3, 0.4, -0.2])
        }]);

        // Same shape: silence is a colour, not a different object.
        assert_eq!(silent.solid.len(), loud.solid.len());
        assert_eq!(silent.line.len(), loud.line.len());

        let brightness = |mesh: &MeshBuilder| {
            let face = &mesh.solid[empty.solid.len()..];
            let sum: u32 = face
                .iter()
                .map(|vertex| u32::from(vertex.colour[0]) + u32::from(vertex.colour[2]))
                .sum();
            sum / u32::try_from(face.len()).unwrap_or(1)
        };
        assert!(
            brightness(&silent) > brightness(&loud),
            "the silent object did not recede toward the stage"
        );
    }

    #[test]
    fn a_trail_adds_one_mark_and_one_floor_projection_per_breadcrumb() {
        let trail = [[-0.5, 0.2, 0.1], [-0.2, 0.2, 0.1], [0.1, 0.2, 0.1]];
        let bare = built(&[sounding([0.3, 0.4, -0.2])]);
        let trailed = built(&[SceneObject {
            trail: &trail,
            ..sounding([0.3, 0.4, -0.2])
        }]);

        assert_eq!(
            trailed.solid.len() - bare.solid.len(),
            trail.len() * 6 * 6,
            "one cube per breadcrumb"
        );
        assert_eq!(
            trailed.decal.len() - bare.decal.len(),
            trail.len() * 6,
            "one floor projection per breadcrumb"
        );
    }

    #[test]
    fn the_trail_fades_from_its_newest_mark_back_to_its_oldest() {
        // The fade is what gives the trail a direction; without it the path
        // reads the same forwards and backwards.
        let trail = [[-0.5, 0.2, 0.1], [0.1, 0.2, 0.1]];
        let empty = built(&[]);
        let trailed = built(&[SceneObject {
            trail: &trail,
            ..sounding([0.3, 0.4, -0.2])
        }]);

        // The trail is emitted before its object, so the marks start where the
        // objectless scene ends. Offsetting past a scene that already has the
        // object would land in the middle of the run.
        let marks = &trailed.solid[empty.solid.len()..];
        let brightness = |cube: &[crate::scene3d::mesh::Vertex]| {
            let sum: u32 = cube
                .iter()
                .map(|vertex| u32::from(vertex.colour[0]) + u32::from(vertex.colour[2]))
                .sum();
            sum / u32::try_from(cube.len()).unwrap_or(1)
        };
        let oldest = brightness(&marks[..36]);
        let newest = brightness(&marks[36..72]);
        assert!(
            oldest > newest,
            "the oldest mark ({oldest}) is not the most faded ({newest})"
        );
    }

    /// A wire box is twelve hairline quads; a solid box, six faces of two
    /// triangles. The arrow is three hairlines.
    const WIRE_BOX: usize = 12 * 6;
    const SOLID_BOX: usize = 6 * 6;
    const ARROW: usize = 3 * 6;

    #[test]
    fn a_jump_too_small_to_lose_track_of_keeps_ordinary_breadcrumbs() {
        // Whether it was a discontinuity is settled from the bitstream; whether
        // it is worth annotating is not. A stream that sends instant updates for
        // every small correction would otherwise become a chain of hollow marks.
        let trail = [[0.0, 0.0, 0.0], [0.05, 0.0, 0.0]];
        let object = sounding([0.05, 0.0, 0.0]);
        let travelled = built(&[SceneObject {
            trail: &trail,
            ..object
        }]);
        let jumped = built(&[SceneObject {
            trail: &trail,
            trail_jumps: &[false, true],
            ..object
        }]);

        assert_eq!(jumped.solid.len(), travelled.solid.len());
        assert_eq!(jumped.line.len(), travelled.line.len());
    }

    #[test]
    fn both_ends_of_a_jumped_breadcrumb_pair_turn_hollow() {
        let trail = [[-0.9, 0.0, 0.0], [0.9, 0.0, 0.0]];
        let object = sounding([0.9, 0.0, 0.0]);
        let travelled = built(&[SceneObject {
            trail: &trail,
            ..object
        }]);
        let jumped = built(&[SceneObject {
            trail: &trail,
            trail_jumps: &[false, true],
            ..object
        }]);

        assert_eq!(
            travelled.solid.len() - jumped.solid.len(),
            2 * SOLID_BOX,
            "the departure and the arrival both stop being ordinary marks"
        );
        assert_eq!(
            jumped.line.len() - travelled.line.len(),
            2 * WIRE_BOX + ARROW
        );
    }

    #[test]
    fn an_inactive_object_draws_no_trail_either() {
        let trail = [[-0.5, 0.2, 0.1], [0.1, 0.2, 0.1]];
        let empty = built(&[]);
        let inactive = built(&[SceneObject {
            active: false,
            trail: &trail,
            ..sounding([0.3, 0.4, -0.2])
        }]);

        assert_eq!(inactive.solid.len(), empty.solid.len());
        assert_eq!(inactive.decal.len(), empty.decal.len());
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
