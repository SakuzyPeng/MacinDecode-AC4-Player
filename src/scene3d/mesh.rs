//! Pure geometry for the object scene: colour maths, axis-aligned boxes, and
//! camera-facing quad-expanded lines.
//!
//! Everything here is CPU-side and free of `wgpu` types so it stays unit
//! testable in a headless environment — `gpu.rs` only uploads what this module
//! produces. Shading also happens here rather than in a shader: the scene is
//! rebuilt every frame anyway (objects move), so it costs nothing, and it keeps
//! every visual parameter in Rust where a test can reach it.

use eframe::egui::Color32;

use super::params;

/// Linear-ish RGB used for the colour maths. Kept separate from
/// [`Color32`] so lerps do not repeatedly round through 8 bits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb {
    red: f32,
    green: f32,
    blue: f32,
}

impl Rgb {
    #[allow(
        clippy::cast_precision_loss,
        reason = "theme channels are 8-bit and convert exactly into f32"
    )]
    pub fn from_color32(colour: Color32) -> Self {
        Self {
            red: f32::from(colour.r()) / 255.0,
            green: f32::from(colour.g()) / 255.0,
            blue: f32::from(colour.b()) / 255.0,
        }
    }

    #[must_use]
    pub fn lerp(self, other: Self, amount: f32) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        Self {
            red: self.red + (other.red - self.red) * amount,
            green: self.green + (other.green - self.green) * amount,
            blue: self.blue + (other.blue - self.blue) * amount,
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "channels are clamped to 0..=1 before scaling into a u8"
    )]
    fn to_bytes(self) -> [u8; 4] {
        [
            (self.red.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
            (self.green.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
            (self.blue.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
            255,
        ]
    }
}

/// One flat-shaded vertex. The fragment shader is a passthrough, so the colour
/// here is final.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub colour: [u8; 4],
}

/// Everything the builder needs to know about the current view: which way the
/// camera looks, how degenerate that view is, and how wide a hairline has to be
/// in world units to land on the intended number of screen points.
#[derive(Debug, Clone, Copy)]
pub struct ViewContext {
    /// Unit vector from the view target toward the eye.
    pub direction: [f32; 3],
    /// `1.0` when the view is exactly axis-aligned, `0.0` at a general angle.
    pub degeneracy: f32,
    /// World units spanned by one egui point at the current zoom.
    pub world_units_per_point: f32,
    /// Palette anchors, pulled from the theme by the caller.
    pub ink: Rgb,
    pub stage: Rgb,
}

/// The three vertex streams, one per pipeline. They are separate because the
/// pipelines differ in face culling and depth bias, not because the geometry
/// differs in kind.
#[derive(Debug, Default)]
pub struct MeshBuilder {
    /// Back-face culled boxes.
    pub solid: Vec<Vertex>,
    /// Camera-facing line quads; culling must be off because their winding
    /// flips with the view direction.
    pub line: Vec<Vertex>,
    /// Geometry coplanar with the floor; culling off and depth-biased.
    pub decal: Vec<Vertex>,
}

/// An annotation arrow's proportions, all in screen points except the head
/// angle. Bundled so [`MeshBuilder::add_arrow`] keeps a readable signature and
/// the values stay together with the constants that supply them.
#[derive(Debug, Clone, Copy)]
pub struct ArrowSpec {
    pub shaft_points: f32,
    pub head_points: f32,
    pub head_degrees: f32,
    pub width_points: f32,
}

/// Which stream a primitive belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Solid,
    Line,
    Decal,
}

/// The twelve edges of a box, as index pairs into its eight corners.
const EDGES: [(usize, usize); 12] = [
    (0, 1),
    (1, 2),
    (2, 3),
    (3, 0),
    (4, 5),
    (5, 6),
    (6, 7),
    (7, 4),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];

impl MeshBuilder {
    pub fn clear(&mut self) {
        self.solid.clear();
        self.line.clear();
        self.decal.clear();
    }

    fn stream(&mut self, layer: Layer) -> &mut Vec<Vertex> {
        match layer {
            Layer::Solid => &mut self.solid,
            Layer::Line => &mut self.line,
            Layer::Decal => &mut self.decal,
        }
    }

    fn triangle(&mut self, layer: Layer, a: [f32; 3], b: [f32; 3], c: [f32; 3], colour: [u8; 4]) {
        let stream = self.stream(layer);
        for position in [a, b, c] {
            stream.push(Vertex { position, colour });
        }
    }

    fn quad(
        &mut self,
        layer: Layer,
        a: [f32; 3],
        b: [f32; 3],
        c: [f32; 3],
        d: [f32; 3],
        colour: [u8; 4],
    ) {
        self.triangle(layer, a, b, c, colour);
        self.triangle(layer, a, c, d, colour);
    }

    /// An axis-aligned box. All six faces are emitted with outward-facing
    /// counter-clockwise winding; the GPU culls the away-facing ones, which in a
    /// general view leaves exactly three — the number
    /// [`visible_face_count`] pins down.
    pub fn add_box(&mut self, centre: [f32; 3], size: [f32; 3], base: Rgb, view: &ViewContext) {
        self.add_oriented_box(
            centre,
            size,
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            base,
            view,
        );
    }

    /// A box whose local X/Y/Z unit vectors are the columns in `axes`.
    /// Winding and face tones follow the transformed geometry, so a rigidly
    /// rotated assembly remains solid under back-face culling.
    pub fn add_oriented_box(
        &mut self,
        centre: [f32; 3],
        size: [f32; 3],
        axes: [[f32; 3]; 3],
        base: Rgb,
        view: &ViewContext,
    ) {
        let (x0, x1) = (-size[0] / 2.0, size[0] / 2.0);
        let (y0, y1) = (-size[1] / 2.0, size[1] / 2.0);
        let (z0, z1) = (-size[2] / 2.0, size[2] / 2.0);

        let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
            (
                [0.0, 1.0, 0.0],
                [[x0, y1, z1], [x1, y1, z1], [x1, y1, z0], [x0, y1, z0]],
            ),
            (
                [0.0, -1.0, 0.0],
                [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]],
            ),
            (
                [1.0, 0.0, 0.0],
                [[x1, y0, z1], [x1, y0, z0], [x1, y1, z0], [x1, y1, z1]],
            ),
            (
                [-1.0, 0.0, 0.0],
                [[x0, y0, z0], [x0, y0, z1], [x0, y1, z1], [x0, y1, z0]],
            ),
            (
                [0.0, 0.0, 1.0],
                [[x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]],
            ),
            (
                [0.0, 0.0, -1.0],
                [[x1, y0, z0], [x0, y0, z0], [x0, y1, z0], [x1, y1, z0]],
            ),
        ];

        for (local_normal, local_corners) in faces {
            let normal = orient(local_normal, axes);
            let corners = local_corners.map(|corner| add(centre, orient(corner, axes)));
            let toned = base.lerp(view.ink, 1.0 - face_tone(normal));
            let colour = faded(toned, centre, view).to_bytes();
            self.quad(
                Layer::Solid,
                corners[0],
                corners[1],
                corners[2],
                corners[3],
                colour,
            );
        }
    }

    /// A hairline, expanded into a camera-facing quad.
    ///
    /// WebGPU has no line width — `LineList` is always one *physical* pixel,
    /// which on a 2x display is half the width of egui's own 1px strokes. So
    /// lines become geometry, and the width is derived from the current
    /// orthographic height so it stays constant through zoom and DPI alike.
    pub fn add_line(
        &mut self,
        layer: Layer,
        from: [f32; 3],
        to: [f32; 3],
        colour: Rgb,
        width_points: f32,
        view: &ViewContext,
    ) {
        let along = sub(to, from);
        let across = cross(along, view.direction);
        let length = norm(across);
        if length < 1e-9 {
            // The line points straight at the camera and has no screen extent.
            return;
        }
        let half = width_points * view.world_units_per_point / 2.0;
        let offset = scale(across, half / length);

        let centre = scale(add(from, to), 0.5);
        let bytes = faded(colour, centre, view).to_bytes();
        self.quad(
            layer,
            sub(from, offset),
            add(from, offset),
            add(to, offset),
            sub(to, offset),
            bytes,
        );
    }

    /// A fixed-size annotation arrow: shaft plus two barbs, three hairlines.
    ///
    /// Sized in screen points rather than world units because it annotates
    /// rather than measures. An arrow that grew with the zoom would look broken,
    /// and one that grew with the distance it spans would read as a path — which
    /// is exactly the misreading it exists to prevent.
    pub fn add_arrow(
        &mut self,
        layer: Layer,
        from: [f32; 3],
        toward: [f32; 3],
        colour: Rgb,
        spec: ArrowSpec,
        view: &ViewContext,
    ) {
        let direction = sub(toward, from);
        let span = norm(direction);
        if span < 1e-9 {
            return;
        }
        let along = scale(direction, 1.0 / span);
        let across = cross(along, view.direction);
        let across_length = norm(across);
        if across_length < 1e-9 {
            // Pointing straight at the camera: the arrow has no screen extent,
            // and its two endpoints are on top of each other anyway.
            return;
        }
        let across = scale(across, 1.0 / across_length);

        let unit = view.world_units_per_point;
        // Never overshoot what it annotates: on a short span the arrow shrinks
        // rather than sticking out past its own target.
        let shaft = (spec.shaft_points * unit).min(span * 0.9);
        let barb = (spec.head_points * unit).min(shaft);
        let tip = add(from, scale(along, shaft));
        let (sin, cos) = spec.head_degrees.to_radians().sin_cos();
        let back = add(tip, scale(along, -barb * cos));
        let side = scale(across, barb * sin);

        self.add_line(layer, from, tip, colour, spec.width_points, view);
        self.add_line(layer, tip, add(back, side), colour, spec.width_points, view);
        self.add_line(layer, tip, sub(back, side), colour, spec.width_points, view);
    }

    /// The outline of an axis-aligned box: the six edges where a front-facing
    /// side meets a back-facing one, or four at an axis-aligned view.
    ///
    /// Unlike [`Self::add_wire_box`] this is exactly the boundary of the box's
    /// projection, so drawn around a concentric smaller box it is guaranteed to
    /// enclose that box and never cross it. A full wire box cannot promise
    /// that: its near corner projects close to the centre of the inner box's
    /// silhouette and the three edges meeting there run straight across the
    /// faces — which is where an object's scene number is printed.
    pub fn add_silhouette_box(
        &mut self,
        centre: [f32; 3],
        size: [f32; 3],
        colour: Rgb,
        width_points: f32,
        view: &ViewContext,
    ) {
        for axis in 0..3 {
            let (across, along) = ((axis + 1) % 3, (axis + 2) % 3);
            for (side, end) in [(-1.0, -1.0), (-1.0, 1.0), (1.0, -1.0), (1.0, 1.0)] {
                // An edge is on the outline when exactly one of the two faces
                // meeting there is turned toward the eye. Exactly edge-on
                // (`dot == 0`) counts as turned away, which is what leaves an
                // axis-aligned view with the four edges bounding its single
                // visible face rather than with nothing drawn at all.
                let facing: f32 = side * view.direction[across];
                let ending: f32 = end * view.direction[along];
                if (facing > 0.0) == (ending > 0.0) {
                    continue;
                }
                let mut from = centre;
                from[across] += side * size[across] / 2.0;
                from[along] += end * size[along] / 2.0;
                let mut to = from;
                from[axis] -= size[axis] / 2.0;
                to[axis] += size[axis] / 2.0;
                self.add_line(Layer::Line, from, to, colour, width_points, view);
            }
        }
    }

    /// The twelve edges of an axis-aligned box, as hairlines.
    pub fn add_wire_box(
        &mut self,
        centre: [f32; 3],
        size: [f32; 3],
        colour: Rgb,
        width_points: f32,
        view: &ViewContext,
    ) {
        let [cx, cy, cz] = centre;
        let (x0, x1) = (cx - size[0] / 2.0, cx + size[0] / 2.0);
        let (y0, y1) = (cy - size[1] / 2.0, cy + size[1] / 2.0);
        let (z0, z1) = (cz - size[2] / 2.0, cz + size[2] / 2.0);
        let corners = [
            [x0, y0, z0],
            [x1, y0, z0],
            [x1, y0, z1],
            [x0, y0, z1],
            [x0, y1, z0],
            [x1, y1, z0],
            [x1, y1, z1],
            [x0, y1, z1],
        ];
        for (from, to) in EDGES {
            self.add_line(
                Layer::Line,
                corners[from],
                corners[to],
                colour,
                width_points,
                view,
            );
        }
    }

    /// A flat square lying in the floor plane, for the decal stream.
    pub fn add_floor_mark(
        &mut self,
        x: f32,
        z: f32,
        size: f32,
        floor_y: f32,
        colour: Rgb,
        view: &ViewContext,
    ) {
        let half = size / 2.0;
        let bytes = faded(colour, [x, floor_y, z], view).to_bytes();
        self.quad(
            Layer::Decal,
            [x - half, floor_y, z + half],
            [x + half, floor_y, z + half],
            [x + half, floor_y, z - half],
            [x - half, floor_y, z - half],
            bytes,
        );
    }
}

/// Tone for a face, chosen by the dominant axis of its outward normal.
fn face_tone(normal: [f32; 3]) -> f32 {
    if normal[1].abs() > 0.5 {
        params::TONE_TOP
    } else if normal[0].abs() > 0.5 {
        params::TONE_LEFT
    } else {
        params::TONE_RIGHT
    }
}

/// Aerial perspective: lerp toward the stage ground with view depth.
///
/// Near an axis-aligned view the projection carries no depth information at all
/// and three-tone shading has collapsed to one tone, so this ramps up to become
/// the primary depth cue rather than a subtle finishing touch.
fn faded(colour: Rgb, point: [f32; 3], view: &ViewContext) -> Rgb {
    let strength =
        params::AIR_PERSPECTIVE * (1.0 + view.degeneracy * params::DEGENERATE_VIEW_BOOST);
    if strength <= 0.0 {
        return colour;
    }
    let depth = dot(point, view.direction);
    let amount = (0.5 - depth / params::AIR_PERSPECTIVE_SPAN).clamp(0.0, 1.0);
    colour.lerp(view.stage, strength * amount)
}

/// How many faces of an axis-aligned box face the viewer along `direction`.
///
/// Three at a general angle; two or one as the view approaches an axis. The
/// degenerate cases are reachable at any time because the camera is free, so
/// this is a documented behaviour rather than an angle to avoid.
#[must_use]
pub fn visible_face_count(direction: [f32; 3]) -> usize {
    const EPSILON: f32 = 5e-3;
    direction.iter().filter(|axis| axis.abs() > EPSILON).count()
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn orient(local: [f32; 3], axes: [[f32; 3]; 3]) -> [f32; 3] {
    [
        axes[0][0] * local[0] + axes[1][0] * local[1] + axes[2][0] * local[2],
        axes[0][1] * local[0] + axes[1][1] * local[1] + axes[2][1] * local[2],
        axes[0][2] * local[0] + axes[1][2] * local[1] + axes[2][2] * local[2],
    ]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f32; 3], factor: f32) -> [f32; 3] {
    [a[0] * factor, a[1] * factor, a[2] * factor]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(direction: [f32; 3], world_units_per_point: f32) -> ViewContext {
        ViewContext {
            direction,
            degeneracy: 0.0,
            world_units_per_point,
            ink: Rgb::from_color32(Color32::from_rgb(51, 42, 31)),
            stage: Rgb::from_color32(Color32::from_rgb(248, 243, 234)),
        }
    }

    #[test]
    fn a_box_emits_six_outward_wound_faces() {
        let mut mesh = MeshBuilder::default();
        mesh.add_box(
            [0.0, 0.0, 0.0],
            [2.0, 2.0, 2.0],
            Rgb::from_color32(Color32::from_rgb(206, 122, 59)),
            &view([0.577, 0.577, 0.577], 0.01),
        );
        assert_eq!(mesh.solid.len(), 6 * 6, "six faces, two triangles each");

        // Every face's winding must produce a normal pointing away from the
        // centre, otherwise back-face culling would hide the wrong three.
        for face in mesh.solid.chunks(6) {
            let (a, b, c) = (face[0].position, face[1].position, face[2].position);
            let normal = cross(sub(b, a), sub(c, a));
            let outward = dot(normal, a);
            assert!(
                outward > 0.0,
                "winding faces inward for a face at {a:?} (normal {normal:?})"
            );
        }
    }

    /// The rule that picks outline edges has one failure mode worth a test of
    /// its own: written as a sign product rather than as this exclusive-or, an
    /// exactly axis-aligned direction multiplies by a zero component, satisfies
    /// nothing, and silently draws no outline at all — at the TOP, BACK and SIDE
    /// presets, which are buttons the user can press.
    #[test]
    fn a_boxs_outline_is_six_edges_generally_and_four_down_an_axis_but_never_none() {
        let colour = Rgb::from_color32(Color32::from_rgb(206, 122, 59));
        for (direction, edges, what) in [
            ([0.577, 0.577, 0.577], 6, "isometric"),
            ([0.0, 0.0, 1.0], 4, "straight down the Z axis"),
            ([0.0, 1.0, 0.0], 4, "straight down at the floor"),
            ([1.0, 0.0, 0.0], 4, "straight along the X axis"),
            ([0.0, -0.707, 0.707], 6, "perpendicular to one axis only"),
        ] {
            let mut mesh = MeshBuilder::default();
            mesh.add_silhouette_box(
                [0.0, 0.0, 0.0],
                [2.0; 3],
                colour,
                1.0,
                &view(direction, 0.01),
            );
            assert_eq!(
                mesh.line.len(),
                edges * 6,
                "{what}: expected {edges} outline edges"
            );
        }
    }

    /// Why the halo draws an outline instead of a wire box, as a property.
    ///
    /// The projection is affine, so a concentric box scaled by `k` projects to
    /// the inner box's projection scaled by `k` about the same centre — and a
    /// convex region scaled about an interior point strictly contains itself.
    /// The outline is that projection's boundary, so it encloses the cube and
    /// can never cross the scene number printed on its faces. A wire box has no
    /// such guarantee: its near-corner edges are interior to the projection.
    #[test]
    fn a_larger_concentric_outline_is_the_smaller_one_scaled_about_the_centre() {
        let colour = Rgb::from_color32(Color32::from_rgb(206, 122, 59));
        let centre = [0.3, -0.2, 0.1];
        let scale = 2.4;

        let mut inner = MeshBuilder::default();
        let mut outer = MeshBuilder::default();
        let context = view([0.4, 0.3, 0.866], 0.01);
        inner.add_silhouette_box(centre, [0.11; 3], colour, 1.0, &context);
        outer.add_silhouette_box(centre, [0.11 * scale; 3], colour, 1.0, &context);

        assert_eq!(
            inner.line.len(),
            outer.line.len(),
            "the same faces are turned toward the eye, so the same edges are chosen"
        );
        assert!(!inner.line.is_empty(), "the fixture must actually draw");
        // Per-edge midpoints rather than raw vertices: a hairline is expanded
        // into a quad of a fixed screen width, and that width is the same for
        // both boxes rather than scaled with them. The six vertices of each
        // quad average back to the edge's midpoint, which cancels it.
        for (small, large) in inner.line.chunks(6).zip(outer.line.chunks(6)) {
            let midpoint = |quad: &[Vertex], axis: usize| {
                quad.iter().map(|vertex| vertex.position[axis]).sum::<f32>() / 6.0
            };
            for axis in 0..3 {
                let want = (midpoint(small, axis) - centre[axis]).mul_add(scale, centre[axis]);
                assert!(
                    (midpoint(large, axis) - want).abs() < 1e-5,
                    "outline edge at {:?} is not {scale}x {:?} about {centre:?}",
                    midpoint(large, axis),
                    midpoint(small, axis)
                );
            }
        }
    }

    #[test]
    fn an_oriented_box_rotates_its_extents_without_reversing_winding() {
        let mut mesh = MeshBuilder::default();
        mesh.add_oriented_box(
            [0.0, 0.0, 0.0],
            [2.0, 4.0, 6.0],
            [[0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
            Rgb::from_color32(Color32::from_rgb(206, 122, 59)),
            &view([0.577, 0.577, 0.577], 0.01),
        );

        let bounds = (0..3).map(|axis| {
            mesh.solid
                .iter()
                .map(|vertex| vertex.position[axis])
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), value| {
                    (low.min(value), high.max(value))
                })
        });
        assert_eq!(
            bounds.collect::<Vec<_>>(),
            vec![(-3.0, 3.0), (-2.0, 2.0), (-1.0, 1.0)]
        );

        for face in mesh.solid.chunks(6) {
            let (a, b, c) = (face[0].position, face[1].position, face[2].position);
            let normal = cross(sub(b, a), sub(c, a));
            assert!(
                dot(normal, a) > 0.0,
                "rotated face winding points inward at {a:?}"
            );
        }
    }

    #[test]
    fn visible_faces_are_three_in_general_and_collapse_on_an_axis() {
        assert_eq!(visible_face_count([0.577, 0.577, 0.577]), 3);
        assert_eq!(visible_face_count([0.0, 1.0, 0.0]), 1, "a true top view");
        assert_eq!(visible_face_count([0.0, 0.0, 1.0]), 1, "a true front view");
        assert_eq!(
            visible_face_count([0.707, 0.0, 0.707]),
            2,
            "level with the floor but off-axis in plan"
        );
    }

    #[test]
    fn hairline_width_tracks_zoom_so_it_stays_constant_on_screen() {
        // The same request at two zoom levels must produce quads whose world
        // widths differ by exactly the zoom ratio — that is what keeps the line
        // one point wide on screen either way.
        let widths = [0.01_f32, 0.04].map(|world_units_per_point| {
            let mut mesh = MeshBuilder::default();
            mesh.add_line(
                Layer::Line,
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                Rgb::from_color32(Color32::from_rgb(154, 139, 118)),
                params::HAIRLINE_POINTS,
                &view([0.0, 1.0, 0.0], world_units_per_point),
            );
            let quad = &mesh.line;
            (quad[1].position[2] - quad[0].position[2]).abs()
        });

        assert!((widths[0] - params::HAIRLINE_POINTS * 0.01).abs() < 1e-6);
        assert!((widths[1] / widths[0] - 4.0).abs() < 1e-4, "{widths:?}");
    }

    fn arrow_spec() -> ArrowSpec {
        ArrowSpec {
            shaft_points: 16.0,
            head_points: 6.0,
            head_degrees: 32.0,
            width_points: params::HAIRLINE_POINTS,
        }
    }

    #[test]
    fn an_arrow_is_three_hairlines_measured_on_screen_rather_than_in_the_world() {
        // It annotates rather than measures, so it has to stay the same size on
        // screen at any zoom. An arrow that grew with the view would read as
        // broken; one that grew with the distance it spans would read as a path,
        // which is the exact misreading it exists to prevent.
        let tips = [0.01_f32, 0.04].map(|world_units_per_point| {
            let mut mesh = MeshBuilder::default();
            mesh.add_arrow(
                Layer::Line,
                [0.0, 0.0, 0.0],
                [10.0, 0.0, 0.0],
                Rgb::from_color32(Color32::from_rgb(206, 122, 59)),
                arrow_spec(),
                &view([0.0, 1.0, 0.0], world_units_per_point),
            );
            assert_eq!(mesh.line.len(), 3 * 6, "shaft plus two barbs");
            mesh.line[..6]
                .iter()
                .map(|vertex| vertex.position[0])
                .fold(f32::NEG_INFINITY, f32::max)
        });

        assert!((tips[0] - 16.0 * 0.01).abs() < 1e-5, "{tips:?}");
        assert!((tips[1] / tips[0] - 4.0).abs() < 1e-4, "{tips:?}");
    }

    #[test]
    fn an_arrow_shrinks_rather_than_overshooting_what_it_annotates() {
        let mut mesh = MeshBuilder::default();
        mesh.add_arrow(
            Layer::Line,
            [0.0, 0.0, 0.0],
            [0.1, 0.0, 0.0],
            Rgb::from_color32(Color32::from_rgb(206, 122, 59)),
            arrow_spec(),
            &view([0.0, 1.0, 0.0], 0.01),
        );
        let tip = mesh.line[..6]
            .iter()
            .map(|vertex| vertex.position[0])
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(tip < 0.1, "the arrow reached past its own target: {tip}");
    }

    #[test]
    fn an_arrow_pointing_at_the_camera_is_dropped() {
        let mut mesh = MeshBuilder::default();
        mesh.add_arrow(
            Layer::Line,
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            Rgb::from_color32(Color32::from_rgb(206, 122, 59)),
            arrow_spec(),
            &view([0.0, 1.0, 0.0], 0.01),
        );
        assert!(mesh.line.is_empty());
    }

    #[test]
    fn a_line_pointing_at_the_camera_is_dropped_rather_than_degenerate() {
        let mut mesh = MeshBuilder::default();
        mesh.add_line(
            Layer::Line,
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            Rgb::from_color32(Color32::from_rgb(154, 139, 118)),
            1.0,
            &view([0.0, 1.0, 0.0], 0.01),
        );
        assert!(mesh.line.is_empty());
    }

    #[test]
    fn air_perspective_pushes_distant_geometry_toward_the_stage_ground() {
        let context = view([0.0, 0.0, 1.0], 0.01);
        let base = Rgb::from_color32(Color32::from_rgb(206, 122, 59));
        let near = faded(base, [0.0, 0.0, 1.0], &context);
        let far = faded(base, [0.0, 0.0, -1.0], &context);
        assert!(
            far.red > near.red && far.green > near.green,
            "far geometry should sit closer to the light ground: {near:?} vs {far:?}"
        );
    }

    #[test]
    fn a_degenerate_view_deepens_the_only_remaining_depth_cue() {
        let point = [0.0, 0.0, -1.0];
        let base = Rgb::from_color32(Color32::from_rgb(206, 122, 59));
        let general = faded(base, point, &view([0.577, 0.577, 0.577], 0.01));
        let degenerate = faded(
            base,
            point,
            &ViewContext {
                degeneracy: 1.0,
                ..view([0.577, 0.577, 0.577], 0.01)
            },
        );
        assert!(degenerate.red > general.red);
    }
}
