//! The room view: the room renderer's modelled room, at its own proportions.
//!
//! The ordinary scene is a panner's diagram — a normalised box, elevation
//! remapped around the head — because no output it draws for has a room. The
//! room renderer does: a shoebox in metres, a fixed listening position, and a
//! rule that pulls any source the Scene asks to put beyond a wall back inside it
//! along its own direction. What is heard is that position, so that is the only
//! one this view places an object at. The position the Scene asked for is not
//! drawn: two cubes per object would make every reader tell them apart.
//!
//! Everything here is geometry in plain arrays, like the rest of `scene3d`
//! outside [`super::gpu`], so it is tested without an adapter.
//!
//! # Coordinates
//!
//! Room coordinates are the renderer's: metres from a corner, x front–back,
//! y right-to-left, z up. World coordinates are the scene's: `+X` right, `+Y`
//! up, `-Z` front, with the listener's ears at the origin as in the ordinary
//! scene. One uniform scale fits the room's longer floor side to the width the
//! ordinary room is drawn at, so the camera's framing carries over. Actors keep
//! their own world sizes, as [`super::params`] requires: the room is a ruler.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the scene draws the room view once it is wired into scene::build; until then only the tests call it"
    )
)]

use crate::room_package::{RoomGeometry, Surface};

use super::figure::Stance;
use super::params;

/// Width, in metres, of one module of wall treatment: the pitch panels are laid
/// at, whatever share of it a surface's absorption fills.
const PANEL_PITCH_METRES: f64 = 0.6;
/// Panels stop this far short of the floor and the ceiling.
const PANEL_INSET_METRES: f64 = 0.1;
/// The floor ruler's finest step, counted out from the listener.
const GRID_STEP_METRES: f64 = 0.5;

/// The seated listener's model unit, in metres, at most: a 1.6 m figure.
const SEATED_UNIT_METRES: f64 = 0.05;

/// Below this a direction component counts as zero, as the renderer counts it.
const DIRECTION_EPSILON: f64 = 1e-15;

/// One line of the floor ruler.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridLine {
    pub from: [f32; 3],
    pub to: [f32; 3],
    pub weight: GridWeight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridWeight {
    /// Through the listener: the two axes everything is measured from.
    Axis,
    /// A whole metre from the listener.
    Metre,
    /// A half metre between them.
    Half,
}

/// A piece of an edge of the Scene's unit box, and whether a source on it would
/// sound where the Scene puts it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxSegment {
    pub from: [f32; 3],
    pub to: [f32; 3],
    pub reachable: bool,
}

/// A room, at one scene unit, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomView {
    dimensions: [f64; 3],
    listener: [f64; 3],
    clearance: f64,
    minimum_distance: f64,
    coverage: [f64; 6],
    metres_per_unit: f64,
    /// World units per metre.
    scale: f64,
}

impl RoomView {
    pub fn new(room: &RoomGeometry, metres_per_unit: f64) -> Self {
        let [front, side, _] = room.dimensions;
        Self {
            dimensions: room.dimensions,
            listener: room.listener,
            clearance: room.boundary_clearance,
            minimum_distance: room.minimum_distance,
            coverage: Surface::ALL.map(|surface| room.mean_absorption(surface)),
            metres_per_unit,
            scale: f64::from(params::ROOM_WIDTH) / front.max(side),
        }
    }

    /// The listener, seated with the head's centre on the listening position.
    ///
    /// The person is real size: a model unit is 0.05 m, a 1.6 m figure, so the
    /// room reads at its true scale around them. The seat top sits about 18
    /// units below the head's centre (16 to the hip, two more to the thighs'
    /// underside), so a unit is also capped at a 24th of the ear height, which
    /// keeps the seat at least a quarter of that height off the floor in a room
    /// whose listening position is low.
    pub fn stance(&self) -> Stance {
        let metres = SEATED_UNIT_METRES.min(self.listener[2] / 24.0);
        Stance::seated(narrow(metres * self.scale))
    }

    /// The floor's height in world coordinates.
    pub fn floor_y(&self) -> f32 {
        narrow(-self.listener[2] * self.scale)
    }

    /// A point in the room, in world coordinates.
    pub fn world(&self, point: [f64; 3]) -> [f32; 3] {
        let [x, y, z] = std::array::from_fn(|axis| point[axis] - self.listener[axis]);
        world_direction([x, y, z]).map(|value| narrow(value * self.scale))
    }

    /// Where the renderer puts a source the Scene places at `position`.
    ///
    /// `position` is what the scene view carries: normalised listener space,
    /// `+X` right, `+Y` up, `-Z` front. The arithmetic is `MacinRender`'s own
    /// (`mradm-dsp` `room::geometry::position`): scale by the scene unit, keep
    /// the source at least the minimum distance away, and stop it the boundary
    /// clearance short of the first wall its ray meets. Head-locked sources go
    /// through the same rule. A source at the listener's own position has no
    /// direction; the renderer keeps the one it had, which a single frame does
    /// not know, so this places it straight ahead, the renderer's own starting
    /// direction.
    pub fn place(&self, [x, y, z]: [f32; 3]) -> [f32; 3] {
        let relative = [-f64::from(z), -f64::from(x), f64::from(y)];
        let length = norm(relative);
        let direction = if length > DIRECTION_EPSILON {
            relative.map(|value| value / length)
        } else {
            [1.0, 0.0, 0.0]
        };
        let requested = length * self.metres_per_unit;
        let mut maximum = f64::INFINITY;
        for (axis, component) in direction.into_iter().enumerate() {
            if component > DIRECTION_EPSILON {
                maximum = maximum.min(
                    (self.dimensions[axis] - self.clearance - self.listener[axis]) / component,
                );
            } else if component < -DIRECTION_EPSILON {
                maximum = maximum.min((self.clearance - self.listener[axis]) / component);
            }
        }
        let distance = requested.max(self.minimum_distance).min(maximum);
        self.world(std::array::from_fn(|axis| {
            self.listener[axis] + direction[axis] * distance
        }))
    }

    /// A surface's four corners, in world coordinates, going round its edge.
    pub fn corners(&self, surface: Surface) -> [[f32; 3]; 4] {
        let axis = surface.axis();
        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
        let at = if surface.at_far_end() {
            self.dimensions[axis]
        } else {
            0.0
        };
        [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|(a, b)| {
            let mut point = [0.0; 3];
            point[axis] = at;
            point[u] = a * self.dimensions[u];
            point[v] = b * self.dimensions[v];
            self.world(point)
        })
    }

    /// Whether the camera sees this surface from inside the room.
    ///
    /// A surface turned towards the camera is the one between the camera and
    /// the room, so it is cut away to its edges; the far surfaces are the ones
    /// drawn whole. `towards_eye` is the view target's direction to the eye,
    /// which decides it exactly for the orthographic camera and, at the
    /// perspective camera's distance, all but exactly for that one too.
    pub fn shows_inner_face(surface: Surface, towards_eye: [f32; 3]) -> bool {
        let inward = world_direction(inward_normal(surface));
        let dot = inward
            .iter()
            .zip(towards_eye)
            .map(|(a, b)| a * f64::from(b))
            .sum::<f64>();
        dot > 0.0
    }

    /// The surface's absorption averaged over its bands: how much of each
    /// module its treatment fills.
    pub fn coverage(&self, surface: Surface) -> f64 {
        self.coverage[surface as usize]
    }

    /// The treatment on a surface, as quads in world coordinates.
    ///
    /// It reads the surface's absorption and nothing else. Walls carry vertical
    /// panels at a fixed pitch, each as wide as the absorbed share of its
    /// module, so a wall absorbing nearly everything reads as fully covered and
    /// a reflective one as bare boards between narrow strips; the ceiling
    /// carries square tiles the same way. The floor carries none: it is the
    /// ruler, and its absorption is for its tone to show. There are no doors,
    /// windows, diffusers or traps, because the model has none.
    pub fn panels(&self, surface: Surface) -> Vec<[[f32; 3]; 4]> {
        let fill = self.coverage(surface).clamp(0.0, 1.0);
        if surface == Surface::Floor || fill <= 0.0 {
            return Vec::new();
        }
        let axis = surface.axis();
        let at = if surface.at_far_end() {
            self.dimensions[axis]
        } else {
            0.0
        };
        let quad = |(u, u0, u1): (usize, f64, f64), (v, v0, v1): (usize, f64, f64)| {
            [(u0, v0), (u1, v0), (u1, v1), (u0, v1)].map(|(a, b)| {
                let mut point = [0.0; 3];
                point[axis] = at;
                point[u] = a;
                point[v] = b;
                self.world(point)
            })
        };
        if surface == Surface::Ceiling {
            let along_x = modules(self.dimensions[0], fill);
            let along_y = modules(self.dimensions[1], fill);
            along_x
                .iter()
                .flat_map(|&(x0, x1)| {
                    along_y
                        .iter()
                        .map(move |&(y0, y1)| quad((0, x0, x1), (1, y0, y1)))
                })
                .collect()
        } else {
            // A wall runs along whichever floor axis it is not perpendicular to.
            let run = 1 - axis;
            let height = self.dimensions[2];
            modules(self.dimensions[run], fill)
                .into_iter()
                .map(|(a, b)| {
                    quad(
                        (run, a, b),
                        (2, PANEL_INSET_METRES, height - PANEL_INSET_METRES),
                    )
                })
                .collect()
        }
    }

    /// The floor ruler, counted out from the listener so that distances read
    /// off it are distances from the ears' position, and clipped to the room.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "a few dozen half-metre steps from a listener inside the room, so \
                  the counts are small and never negative"
    )]
    pub fn floor_lines(&self) -> Vec<GridLine> {
        let mut lines = Vec::new();
        for axis in 0..2 {
            let across = 1 - axis;
            let origin = self.listener[axis];
            let below = (origin / GRID_STEP_METRES).floor() as i64;
            let above = ((self.dimensions[axis] - origin) / GRID_STEP_METRES).floor() as i64;
            for step in -below..=above {
                let at = origin + step as f64 * GRID_STEP_METRES;
                let weight = match step {
                    0 => GridWeight::Axis,
                    _ if step % 2 == 0 => GridWeight::Metre,
                    _ => GridWeight::Half,
                };
                let mut from = [0.0; 3];
                let mut to = [0.0; 3];
                from[axis] = at;
                to[axis] = at;
                to[across] = self.dimensions[across];
                lines.push(GridLine {
                    from: self.world(from),
                    to: self.world(to),
                    weight,
                });
            }
        }
        lines
    }

    /// The edges of the Scene's unit box — the ordinary scene's whole room —
    /// centred on the ears at the current scene unit, each split where it
    /// leaves the volume a source can sound in. That volume is the room less
    /// the boundary clearance: the listener is inside it and it is convex, so a
    /// point in it is on an unobstructed ray and sounds where it is asked to.
    ///
    /// The minimum distance is not drawn: it only matters at a scene unit
    /// shorter than itself, where the whole box shrinks into the listener.
    pub fn unit_box(&self) -> Vec<BoxSegment> {
        let half = self.metres_per_unit;
        let corner = |index: usize| -> [f64; 3] {
            std::array::from_fn(|axis| {
                let sign = if index >> axis & 1 == 1 { 1.0 } else { -1.0 };
                self.listener[axis] + sign * half
            })
        };
        let low = [self.clearance; 3];
        let high: [f64; 3] = std::array::from_fn(|axis| self.dimensions[axis] - self.clearance);
        let mut segments = Vec::new();
        for start in 0..8_usize {
            for axis in 0..3 {
                if start >> axis & 1 == 1 {
                    continue;
                }
                let (from, to) = (corner(start), corner(start | 1 << axis));
                let (enter, leave) = clip(from, to, low, high);
                let at = |t: f64| -> [f64; 3] {
                    std::array::from_fn(|i| from[i] + (to[i] - from[i]) * t)
                };
                let mut push = |t0: f64, t1: f64, reachable: bool| {
                    if t1 > t0 {
                        segments.push(BoxSegment {
                            from: self.world(at(t0)),
                            to: self.world(at(t1)),
                            reachable,
                        });
                    }
                };
                if enter < leave {
                    push(0.0, enter, false);
                    push(enter, leave, true);
                    push(leave, 1.0, false);
                } else {
                    push(0.0, 1.0, false);
                }
            }
        }
        segments
    }
}

/// The room's inward normal at a surface, in room axes.
fn inward_normal(surface: Surface) -> [f64; 3] {
    let mut normal = [0.0; 3];
    normal[surface.axis()] = if surface.at_far_end() { -1.0 } else { 1.0 };
    normal
}

/// A direction in room axes, turned into world axes.
fn world_direction([x, y, z]: [f64; 3]) -> [f64; 3] {
    [-y, z, -x]
}

/// A room position, narrowed for drawing.
#[allow(
    clippy::cast_possible_truncation,
    reason = "world coordinates of a room tens of metres across, far inside f32's range"
)]
fn narrow(value: f64) -> f32 {
    value as f32
}

fn norm(v: [f64; 3]) -> f64 {
    v[0].hypot(v[1]).hypot(v[2])
}

/// The panels along `length`: a whole number of modules, centred, each filled
/// to `fill` of its pitch about its own centre.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a whole, non-negative number of 0.6 m modules along one wall"
)]
fn modules(length: f64, fill: f64) -> Vec<(f64, f64)> {
    // A wall a whole number of modules long must not lose one to rounding.
    let count = (length / PANEL_PITCH_METRES + 1e-9).floor();
    let start = (length - count * PANEL_PITCH_METRES) / 2.0;
    let width = PANEL_PITCH_METRES * fill;
    (0..count as usize)
        .map(|index| {
            let centre = start + (index as f64 + 0.5) * PANEL_PITCH_METRES;
            (centre - width / 2.0, centre + width / 2.0)
        })
        .collect()
}

/// The part of the segment from `from` to `to` inside the box `low..high`, as
/// parameters along it. Empty when the first is not below the second.
fn clip(from: [f64; 3], to: [f64; 3], low: [f64; 3], high: [f64; 3]) -> (f64, f64) {
    let (mut enter, mut leave) = (0.0_f64, 1.0_f64);
    for axis in 0..3 {
        let span = to[axis] - from[axis];
        if span.abs() <= DIRECTION_EPSILON {
            if from[axis] < low[axis] || from[axis] > high[axis] {
                return (1.0, 0.0);
            }
            continue;
        }
        let a = (low[axis] - from[axis]) / span;
        let b = (high[axis] - from[axis]) / span;
        enter = enter.max(a.min(b));
        leave = leave.min(a.max(b));
    }
    (enter, leave)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> RoomGeometry {
        RoomGeometry::parse(include_str!(
            "../../tests/fixtures/room-reference-7x6x3.2-corrected-v1.json"
        ))
        .unwrap()
    }

    fn view(metres_per_unit: f64) -> RoomView {
        RoomView::new(&reference(), metres_per_unit)
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5)
    }

    /// World units per metre in the reference room: its 7 m side spans the
    /// ordinary room's width.
    fn k() -> f32 {
        params::ROOM_WIDTH / 7.0
    }

    #[test]
    fn the_ears_are_the_origin_and_the_axes_turn_into_the_scene_s() {
        let view = view(1.5);
        assert!(close(view.world([3.4, 3.0, 1.2]), [0.0; 3]));
        // A metre in front, to the left, and above the listener.
        assert!(close(view.world([4.4, 3.0, 1.2]), [0.0, 0.0, -k()]));
        assert!(close(view.world([3.4, 4.0, 1.2]), [-k(), 0.0, 0.0]));
        assert!(close(view.world([3.4, 3.0, 2.2]), [0.0, k(), 0.0]));
    }

    #[test]
    fn a_source_inside_the_walls_sounds_where_the_scene_puts_it() {
        let view = view(1.5);
        // Front-left at ear height: listener space is -Z front, +X right.
        let front_left = [-1.0, 0.0, -1.0];
        assert!(close(
            view.place(front_left),
            front_left.map(|value| value * 1.5 * k())
        ));
    }

    #[test]
    fn a_source_beyond_a_wall_is_pulled_in_along_its_own_ray() {
        let view = view(1.5);
        // ADM (0.5, -0.5, -1): rear-right and below the ears. At 1.5 m per
        // unit it would sit 0.3 m under the floor; the renderer stops it the
        // 0.15 m clearance above the floor on the same ray.
        let placed = view.place([0.5, -1.0, 0.5]);
        let ray = [-0.5_f64, -0.5, -1.0];
        let length = norm(ray);
        let distance = (0.15 - 1.2) / (ray[2] / length);
        let expected = view.world(std::array::from_fn(|axis| {
            [3.4, 3.0, 1.2][axis] + ray[axis] / length * distance
        }));
        assert!(close(placed, expected), "{placed:?} vs {expected:?}");
        assert!((placed[1] - -1.05 * k()).abs() < 1e-5);
        // Overhead clears the ceiling at 1.5 m and meets it at 2.1 m.
        assert!(close(view.place([0.0, 1.0, 0.0]), [0.0, 1.5 * k(), 0.0]));
        assert!(close(
            RoomView::new(&reference(), 2.1).place([0.0, 1.0, 0.0]),
            [0.0, 1.85 * k(), 0.0]
        ));
    }

    #[test]
    fn a_source_at_the_ears_is_pushed_out_straight_ahead() {
        assert!(close(view(1.5).place([0.0; 3]), [0.0, 0.0, -0.25 * k()]));
        // And one closer than the minimum distance keeps its own direction.
        assert!(close(
            view(1.5).place([0.1, 0.0, 0.0]),
            [0.25 * k(), 0.0, 0.0]
        ));
    }

    #[test]
    fn the_surfaces_between_the_camera_and_the_room_are_cut_away() {
        // From behind the listener's left shoulder and above, as the default
        // camera looks: back, left and ceiling face the camera.
        let towards_eye = [-0.36_f32, 0.52, 0.86];
        let drawn = Surface::ALL.map(|surface| RoomView::shows_inner_face(surface, towards_eye));
        assert_eq!(drawn, [false, true, true, false, true, false]);
        // From below and in front, the reverse.
        let opposite = towards_eye.map(|value| -value);
        let drawn = Surface::ALL.map(|surface| RoomView::shows_inner_face(surface, opposite));
        assert_eq!(drawn, [true, false, false, true, false, true]);
    }

    #[test]
    fn surfaces_lie_on_the_room_s_walls() {
        let view = view(1.5);
        let front = view.corners(Surface::Front);
        // The front wall is 3.6 m ahead of the ears.
        assert!(
            front
                .iter()
                .all(|corner| (corner[2] + 3.6 * k()).abs() < 1e-5)
        );
        let floor = view.corners(Surface::Floor);
        assert!(
            floor
                .iter()
                .all(|corner| (corner[1] + 1.2 * k()).abs() < 1e-5)
        );
    }

    #[test]
    fn treatment_fills_its_modules_by_the_surface_s_absorption() {
        let view = view(1.5);
        // The front wall runs the 6 m width: ten 0.6 m modules, each 94 % full.
        let panels = view.panels(Surface::Front);
        assert_eq!(panels.len(), 10);
        let width = (panels[0][1][0] - panels[0][0][0]).abs();
        assert!((width - 0.6 * 0.94 * k()).abs() < 1e-5);
        // The side walls run the 7 m length: eleven modules, centred.
        assert_eq!(view.panels(Surface::Right).len(), 11);
        // The ceiling is tiled both ways; the floor is the ruler.
        assert_eq!(view.panels(Surface::Ceiling).len(), 11 * 10);
        assert!(view.panels(Surface::Floor).is_empty());

        let mut bare = reference();
        bare.absorption[1] = [0.0; 8];
        bare.absorption[2] = [0.2; 8];
        let bare = RoomView::new(&bare, 1.5);
        assert!(bare.panels(Surface::Front).is_empty());
        let strip = bare.panels(Surface::Right)[0];
        let width = (strip[1][2] - strip[0][2]).abs();
        assert!((width - 0.6 * 0.2 * k()).abs() < 1e-5);
    }

    #[test]
    fn the_floor_ruler_is_counted_from_the_listener() {
        let lines = view(1.5).floor_lines();
        let axes = lines
            .iter()
            .filter(|line| line.weight == GridWeight::Axis)
            .collect::<Vec<_>>();
        assert_eq!(axes.len(), 2);
        // Both axes pass under the ears.
        assert!(
            axes.iter()
                .all(|line| line.from[0].abs() < 1e-5 || line.from[2].abs() < 1e-5)
        );
        // Front–back: 3.4 m behind and 3.6 m ahead give 6 + 7 lines plus the
        // axis; left–right: 3 m each way gives 6 + 6 plus the axis.
        assert_eq!(lines.len(), 14 + 13);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.weight == GridWeight::Metre)
                .count(),
            3 + 3 + 3 + 3
        );
        assert!(
            lines
                .iter()
                .all(|line| (line.from[1] - line.to[1]).abs() < 1e-6)
        );
    }

    #[test]
    fn the_listener_sits_at_real_size_with_the_seat_on_the_floor() {
        use crate::scene3d::figure::Figure;

        let view = view(1.5);
        let stance = view.stance();
        assert!((stance.unit - 0.05 * k()).abs() < 1e-7);
        assert!((view.floor_y() + 1.2 * k()).abs() < 1e-6);
        let seat = stance.seat(view.floor_y());
        let lowest = seat
            .iter()
            .map(|part| part.centre[1] - part.size[1] / 2.0)
            .fold(f32::INFINITY, f32::min);
        assert!((lowest - view.floor_y()).abs() < 1e-6);
        // Seat top: 1.2 m less 16 units to the hip and the thighs' underside.
        let top = seat[0].centre[1] + seat[0].size[1] / 2.0;
        let expected = narrow(1.2 - 0.05 * (16.0 + 2.0 * 81.0_f64.to_radians().sin()));
        assert!(
            (top - view.floor_y() - expected * k()).abs() < 1e-5,
            "{top}"
        );
        // The head is centred on the listening position.
        let head = Figure::default().parts_in(stance)[5];
        assert!(close(head.centre, [0.0; 3]));

        // A listening position half a metre up still leaves room for the seat.
        let mut low = reference();
        low.listener[2] = 0.5;
        let low = RoomView::new(&low, 1.5);
        let stance = low.stance();
        assert!((stance.unit - narrow(0.5 / 24.0) * k()).abs() < 1e-7);
        let seat = stance.seat(low.floor_y());
        let top = seat[0].centre[1] + seat[0].size[1] / 2.0;
        assert!(top - low.floor_y() >= 0.5 / 4.0 * k() - 1e-6);
    }

    #[test]
    fn the_unit_box_shows_where_it_leaves_the_reachable_room() {
        // At 1.5 m the box reaches 0.3 m under the floor: its bottom four edges
        // are out entirely, and each vertical edge is out below the clearance.
        let segments = view(1.5).unit_box();
        let out = segments
            .iter()
            .filter(|segment| !segment.reachable)
            .collect::<Vec<_>>();
        assert_eq!(out.len(), 4 + 4);
        let clearance_y = -1.05 * k();
        let verticals = out
            .iter()
            .filter(|segment| (segment.from[1] - segment.to[1]).abs() > 1e-6)
            .collect::<Vec<_>>();
        assert_eq!(verticals.len(), 4);
        assert!(
            verticals
                .iter()
                .all(|segment| { (segment.from[1].max(segment.to[1]) - clearance_y).abs() < 1e-5 })
        );
        // At 1 m the whole box is inside.
        assert!(view(1.0).unit_box().iter().all(|segment| segment.reachable));
        assert_eq!(view(1.0).unit_box().len(), 12);
    }
}
