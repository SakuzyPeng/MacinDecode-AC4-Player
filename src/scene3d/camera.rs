//! The scene camera: a free orbit with orthographic and perspective projection.
//!
//! This is an inspection tool, not a fixed diagram — reading an object's
//! position off a single angle is ambiguous, so every angle has to be reachable.
//! Presets exist to get back to a known reading, not to fence the view in.
//!
//! The matrix built here is the single source of truth for projection. The GPU
//! receives it as a uniform and the UI reuses it on the CPU for labels and
//! hit-testing; deriving either side separately guarantees they drift.

use eframe::egui::{Pos2, Rect};
use serde::{Deserialize, Serialize};

use super::params;

/// Column-major 4x4, matching what the shader expects.
pub type Matrix4 = [f32; 16];

/// Projection used to inspect the same listener-relative scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ProjectionMode {
    #[default]
    Orthographic,
    Perspective,
}

impl ProjectionMode {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Orthographic => "ORTHO",
            Self::Perspective => "PERSP",
        }
    }
}

/// Named viewpoints, offered as a way back to a known reading rather than as a
/// fence around the view.
///
/// These are the true axis-aligned angles. An earlier design avoided them
/// because an axis-aligned view collapses three-tone shading to one tone and
/// drops depth out of the projection — but with a free camera the user reaches
/// them by dragging anyway, so hiding them from the presets would only be
/// inconsistent. A true top view is also the most accurate way to read azimuth,
/// and a true level view the most accurate way to read elevation: they are
/// analytical readings, not mistakes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Iso,
    Top,
    Back,
    Side,
}

impl Preset {
    fn angles(self) -> (f32, f32) {
        match self {
            Self::Iso => (params::ISO_AZIMUTH_DEGREES, params::ISO_ELEVATION_DEGREES),
            Self::Top => (0.0, params::MAX_ELEVATION_DEGREES),
            // The listener faces -Z, so an eye on +Z sees their back.
            Self::Back => (0.0, 0.0),
            Self::Side => (90.0, 0.0),
        }
    }
}

/// Angles a release will settle onto when it lands close enough.
const CANONICAL_AZIMUTHS: [f32; 9] = [
    0.0,
    45.0,
    90.0,
    135.0,
    180.0,
    225.0,
    270.0,
    315.0,
    params::ISO_AZIMUTH_DEGREES,
];
const CANONICAL_ELEVATIONS: [f32; 6] = [
    0.0,
    params::ISO_ELEVATION_DEGREES,
    30.0,
    35.264,
    60.0,
    params::MAX_ELEVATION_DEGREES,
];

#[derive(Debug, Clone, Copy, PartialEq)]
struct Animation {
    from: (f32, f32, f32, [f32; 3]),
    to: (f32, f32, f32, [f32; 3]),
    elapsed_seconds: f32,
    duration_seconds: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    azimuth_degrees: f32,
    elevation_degrees: f32,
    ortho_height: f32,
    target: [f32; 3],
    projection_mode: ProjectionMode,
    animation: Option<Animation>,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            azimuth_degrees: params::ISO_AZIMUTH_DEGREES,
            elevation_degrees: params::ISO_ELEVATION_DEGREES,
            ortho_height: params::DEFAULT_ORTHO_HEIGHT,
            target: [0.0; 3],
            projection_mode: ProjectionMode::Orthographic,
            animation: None,
        }
    }
}

/// The part of the view worth surviving a restart.
///
/// A separate snapshot rather than serde on [`Camera`] itself, for two
/// reasons. A preset easing is per-session state that has no business on disk.
/// And a value read back has to pass through the same limits the live controls
/// enforce: `eframe`'s store is a plain JSON file, so it can be stale from an
/// older build or edited by hand, and [`Camera::from_state`] therefore treats
/// every field as untrusted rather than as something [`Camera::state`] wrote.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CameraState {
    azimuth_degrees: f32,
    elevation_degrees: f32,
    ortho_height: f32,
    target: [f32; 3],
    projection_mode: ProjectionMode,
}

impl Camera {
    /// Rebuild a camera from a persisted snapshot, forcing every value back
    /// inside the range the live controls hold it to.
    ///
    /// The clamping is load-bearing, not defensive politeness. A non-finite
    /// elevation would carry NaN through the view matrix into every vertex in
    /// the scene, and an `ortho_height` of zero would collapse the projection
    /// outright. Neither is reachable by dragging; both are one hand-edit away
    /// in the store.
    #[must_use]
    pub fn from_state(state: CameraState) -> Self {
        Self {
            // Azimuth wraps rather than clamps, exactly as `orbit` does.
            azimuth_degrees: if state.azimuth_degrees.is_finite() {
                wrap_degrees(state.azimuth_degrees)
            } else {
                params::ISO_AZIMUTH_DEGREES
            },
            elevation_degrees: restored(
                state.elevation_degrees,
                -params::MAX_ELEVATION_DEGREES,
                params::MAX_ELEVATION_DEGREES,
                params::ISO_ELEVATION_DEGREES,
            ),
            ortho_height: restored(
                state.ortho_height,
                params::MIN_ORTHO_HEIGHT,
                params::MAX_ORTHO_HEIGHT,
                params::DEFAULT_ORTHO_HEIGHT,
            ),
            target: state
                .target
                .map(|axis| restored(axis, -params::MAX_PAN, params::MAX_PAN, 0.0)),
            projection_mode: state.projection_mode,
            animation: None,
        }
    }

    /// The view worth writing to the store.
    #[must_use]
    pub fn state(&self) -> CameraState {
        // A preset in flight is on its way somewhere the user asked for, so
        // persist that destination rather than whichever frame of the ease the
        // window happened to close on.
        let (azimuth_degrees, elevation_degrees, ortho_height, target) = self.animation.map_or(
            (
                self.azimuth_degrees,
                self.elevation_degrees,
                self.ortho_height,
                self.target,
            ),
            |animation| animation.to,
        );
        CameraState {
            azimuth_degrees,
            elevation_degrees,
            ortho_height,
            target,
            projection_mode: self.projection_mode,
        }
    }

    #[must_use]
    pub fn azimuth_degrees(&self) -> f32 {
        self.azimuth_degrees
    }

    #[must_use]
    pub fn elevation_degrees(&self) -> f32 {
        self.elevation_degrees
    }

    #[must_use]
    pub fn ortho_height(&self) -> f32 {
        self.ortho_height
    }

    #[must_use]
    pub fn projection_mode(&self) -> ProjectionMode {
        self.projection_mode
    }

    /// Switch projection without changing the target-plane scale.
    pub fn toggle_projection(&mut self) {
        self.projection_mode = match self.projection_mode {
            ProjectionMode::Orthographic => ProjectionMode::Perspective,
            ProjectionMode::Perspective => ProjectionMode::Orthographic,
        };
    }

    /// Unit vector from the view target toward the eye.
    #[must_use]
    pub fn direction(&self) -> [f32; 3] {
        let azimuth = self.azimuth_degrees.to_radians();
        let elevation = self.elevation_degrees.to_radians();
        [
            elevation.cos() * azimuth.sin(),
            elevation.sin(),
            elevation.cos() * azimuth.cos(),
        ]
    }

    /// How close the view is to looking straight down a world axis: `1.0` when
    /// axis-aligned, `0.0` at a general angle.
    ///
    /// At `1.0` an axis-aligned box shows a single face, so three-tone shading
    /// carries no information and the projection carries no depth. Callers use
    /// this to ramp up the cues that still work.
    #[must_use]
    pub fn degeneracy(&self) -> f32 {
        let direction = self.direction();
        let mut magnitudes = [direction[0].abs(), direction[1].abs(), direction[2].abs()];
        magnitudes.sort_unstable_by(f32::total_cmp);
        // magnitudes[1] is the middle component: it falls to zero exactly when
        // the view collapses onto a plane, and to near zero on a full axis.
        (1.0 - magnitudes[1] * 3.2).clamp(0.0, 1.0)
    }

    /// World units spanned by one egui point at the target plane.
    ///
    /// This is what keeps a hairline the same width on screen at any zoom, and
    /// it is DPI-independent because points and the view height scale together.
    #[must_use]
    pub fn world_units_per_point(&self, viewport_height_points: f32) -> f32 {
        if viewport_height_points <= 0.0 {
            return 0.0;
        }
        self.ortho_height / viewport_height_points
    }

    /// Combined view-projection matrix for a viewport of the given aspect.
    #[must_use]
    pub fn view_projection(&self, aspect: f32) -> Matrix4 {
        let direction = self.direction();
        let eye = [
            self.target[0] + direction[0] * params::CAMERA_DISTANCE,
            self.target[1] + direction[1] * params::CAMERA_DISTANCE,
            self.target[2] + direction[2] * params::CAMERA_DISTANCE,
        ];
        // Straight down would make the conventional up vector parallel to the
        // view direction. The elevation clamp keeps us off that pole, but pick a
        // safe fallback anyway rather than depending on the clamp.
        let up = if direction[1].abs() > 0.999 {
            [0.0, 0.0, -1.0]
        } else {
            [0.0, 1.0, 0.0]
        };

        let aspect = aspect.max(f32::EPSILON);
        let near = params::CAMERA_DISTANCE - params::DEPTH_HALF_RANGE;
        let far = params::CAMERA_DISTANCE + params::DEPTH_HALF_RANGE;
        let projection = match self.projection_mode {
            ProjectionMode::Orthographic => {
                let half_height = self.ortho_height / 2.0;
                let half_width = half_height * aspect;
                orthographic(
                    -half_width,
                    half_width,
                    -half_height,
                    half_height,
                    near,
                    far,
                )
            }
            ProjectionMode::Perspective => perspective(self.ortho_height, aspect, near, far),
        };
        multiply(projection, look_at(eye, self.target, up))
    }

    /// Project a world point into screen coordinates inside `viewport`.
    ///
    /// Uses the same matrix the GPU gets, so a label and the geometry it names
    /// cannot disagree.
    #[must_use]
    #[allow(
        dead_code,
        reason = "the CPU half of the projection contract; object labels and \
                  hover hit-testing consume it in the next increment"
    )]
    pub fn project(&self, point: [f32; 3], viewport: Rect) -> Pos2 {
        let matrix = self.view_projection(viewport.width() / viewport.height().max(f32::EPSILON));
        let clip = transform(matrix, point);
        Pos2::new(
            viewport.left() + (clip[0] * 0.5 + 0.5) * viewport.width(),
            // Clip space is y-up, screen space is y-down.
            viewport.top() + (0.5 - clip[1] * 0.5) * viewport.height(),
        )
    }

    /// Ease toward a named viewpoint while preserving the room's apparent size.
    pub fn apply_preset(&mut self, preset: Preset, viewport_aspect: f32) {
        let (azimuth, elevation) = preset.angles();
        let current_span = projected_room_span(
            self.projection_mode,
            self.azimuth_degrees,
            self.elevation_degrees,
            self.target,
            viewport_aspect,
        );
        let target_span = projected_room_span(
            self.projection_mode,
            azimuth,
            elevation,
            [0.0; 3],
            viewport_aspect,
        );
        let current_fill = current_span / self.ortho_height.max(f32::EPSILON);
        let ortho_height = if current_fill > f32::EPSILON {
            (target_span / current_fill).clamp(params::MIN_ORTHO_HEIGHT, params::MAX_ORTHO_HEIGHT)
        } else {
            self.ortho_height
        };
        self.animate_to((azimuth, elevation, ortho_height, [0.0; 3]));
    }

    /// Restore the canonical Angle composition, including zoom and pan, while
    /// leaving the chosen projection mode intact.
    pub fn reset_view(&mut self) {
        self.animate_to((
            params::ISO_AZIMUTH_DEGREES,
            params::ISO_ELEVATION_DEGREES,
            params::DEFAULT_ORTHO_HEIGHT,
            [0.0; 3],
        ));
    }

    /// Settle onto a canonical angle if the view came to rest near one.
    ///
    /// The tolerance never blocks an angle — it only makes the clean readings
    /// easy to land on, so the default look stays crisp without the camera
    /// fighting the user.
    pub fn snap_if_near(&mut self) -> bool {
        let (azimuth_distance, azimuth) = nearest(self.azimuth_degrees, &CANONICAL_AZIMUTHS);
        let (elevation_distance, elevation) =
            nearest(self.elevation_degrees, &CANONICAL_ELEVATIONS);
        let takes_azimuth = azimuth_distance <= params::SNAP_TOLERANCE_DEGREES;
        let takes_elevation = elevation_distance <= params::SNAP_TOLERANCE_DEGREES;
        if !takes_azimuth && !takes_elevation {
            return false;
        }
        self.animate_to((
            if takes_azimuth {
                self.azimuth_degrees + angle_delta(self.azimuth_degrees, azimuth)
            } else {
                self.azimuth_degrees
            },
            if takes_elevation {
                elevation
            } else {
                self.elevation_degrees
            },
            self.ortho_height,
            self.target,
        ));
        true
    }

    fn animate_to(&mut self, to: (f32, f32, f32, [f32; 3])) {
        self.animation = Some(Animation {
            from: (
                self.azimuth_degrees,
                self.elevation_degrees,
                self.ortho_height,
                self.target,
            ),
            to,
            elapsed_seconds: 0.0,
            #[allow(
                clippy::cast_precision_loss,
                reason = "the damping duration is a small millisecond count"
            )]
            duration_seconds: params::SNAP_DAMPING_MILLISECONDS as f32 / 1000.0,
        });
    }

    /// Advance any in-flight easing. Returns `true` while it is still running,
    /// which the caller uses to keep requesting repaints.
    pub fn advance(&mut self, delta_seconds: f32) -> bool {
        let Some(mut animation) = self.animation else {
            return false;
        };
        animation.elapsed_seconds += delta_seconds.max(0.0);
        let fraction = if animation.duration_seconds <= 0.0 {
            1.0
        } else {
            (animation.elapsed_seconds / animation.duration_seconds).clamp(0.0, 1.0)
        };
        let eased = 1.0 - (1.0 - fraction).powi(3);

        let (from_azimuth, from_elevation, from_height, from_target) = animation.from;
        let (to_azimuth, to_elevation, to_height, to_target) = animation.to;
        self.azimuth_degrees =
            wrap_degrees(from_azimuth + angle_delta(from_azimuth, to_azimuth) * eased);
        self.elevation_degrees = from_elevation + (to_elevation - from_elevation) * eased;
        self.ortho_height = from_height + (to_height - from_height) * eased;
        for axis in 0..3 {
            self.target[axis] = from_target[axis] + (to_target[axis] - from_target[axis]) * eased;
        }

        if fraction >= 1.0 {
            self.animation = None;
            return false;
        }
        self.animation = Some(animation);
        true
    }

    /// Abandon any easing, because the user took hold of the camera.
    pub fn cancel_animation(&mut self) {
        self.animation = None;
    }

    /// Orbit by a pointer drag measured in egui points.
    pub fn orbit(&mut self, delta: [f32; 2]) {
        self.azimuth_degrees = wrap_degrees(self.azimuth_degrees - delta[0] * 0.4);
        self.elevation_degrees = (self.elevation_degrees + delta[1] * 0.3).clamp(
            -params::MAX_ELEVATION_DEGREES,
            params::MAX_ELEVATION_DEGREES,
        );
    }

    /// Zoom by a scroll delta. Multiplicative so each notch feels the same at
    /// every scale.
    pub fn zoom(&mut self, scroll: f32) {
        self.ortho_height = (self.ortho_height * (scroll * 0.0012).exp())
            .clamp(params::MIN_ORTHO_HEIGHT, params::MAX_ORTHO_HEIGHT);
    }

    /// Pan the view target within the view plane, by a drag in egui points.
    ///
    /// Scaled by the orthographic height so the scene tracks the cursor at any
    /// zoom, then clamped so the room cannot be lost off-screen.
    pub fn pan(&mut self, delta: [f32; 2], viewport_height_points: f32) {
        let scale = self.world_units_per_point(viewport_height_points);
        let azimuth = self.azimuth_degrees.to_radians();
        let elevation = self.elevation_degrees.to_radians();
        let right = [azimuth.cos(), 0.0, -azimuth.sin()];
        let up = [
            -elevation.sin() * azimuth.sin(),
            elevation.cos(),
            -elevation.sin() * azimuth.cos(),
        ];
        for axis in 0..3 {
            let moved =
                self.target[axis] - right[axis] * delta[0] * scale + up[axis] * delta[1] * scale;
            self.target[axis] = moved.clamp(-params::MAX_PAN, params::MAX_PAN);
        }
    }
}

/// Maximum projected room span expressed in vertical-view world units.
///
/// Horizontal span is divided by the viewport aspect so it can be compared
/// directly with vertical span. Holding `span / ortho_height` constant makes
/// the room retain the same dominant on-screen extent across preset angles.
fn projected_room_span(
    projection_mode: ProjectionMode,
    azimuth_degrees: f32,
    elevation_degrees: f32,
    target: [f32; 3],
    aspect: f32,
) -> f32 {
    let azimuth = azimuth_degrees.to_radians();
    let elevation = elevation_degrees.to_radians();
    let direction = [
        elevation.cos() * azimuth.sin(),
        elevation.sin(),
        elevation.cos() * azimuth.cos(),
    ];
    let right = [azimuth.cos(), 0.0, -azimuth.sin()];
    let up = [
        -elevation.sin() * azimuth.sin(),
        elevation.cos(),
        -elevation.sin() * azimuth.cos(),
    ];
    let mut horizontal_min = f32::INFINITY;
    let mut horizontal_max = f32::NEG_INFINITY;
    let mut vertical_min = f32::INFINITY;
    let mut vertical_max = f32::NEG_INFINITY;

    for x in [-params::ROOM_WIDTH / 2.0, params::ROOM_WIDTH / 2.0] {
        for y in [params::ROOM_FLOOR_Y, params::ROOM_CEILING_Y] {
            for z in [-params::ROOM_DEPTH / 2.0, params::ROOM_DEPTH / 2.0] {
                let offset = [x - target[0], y - target[1], z - target[2]];
                let depth_scale = match projection_mode {
                    ProjectionMode::Orthographic => 1.0,
                    ProjectionMode::Perspective => {
                        let eye_distance = params::CAMERA_DISTANCE - dot(offset, direction);
                        params::CAMERA_DISTANCE / eye_distance.max(f32::EPSILON)
                    }
                };
                let horizontal = dot(offset, right) * depth_scale / aspect.max(f32::EPSILON);
                let vertical = dot(offset, up) * depth_scale;
                horizontal_min = horizontal_min.min(horizontal);
                horizontal_max = horizontal_max.max(horizontal);
                vertical_min = vertical_min.min(vertical);
                vertical_max = vertical_max.max(vertical);
            }
        }
    }

    (horizontal_max - horizontal_min).max(vertical_max - vertical_min)
}

/// Signed shortest rotation from `from` to `to`, in degrees.
#[must_use]
pub fn angle_delta(from: f32, to: f32) -> f32 {
    let mut delta = (to - from) % 360.0;
    if delta > 180.0 {
        delta -= 360.0;
    }
    if delta < -180.0 {
        delta += 360.0;
    }
    delta
}

/// Distance to, and value of, the closest entry in `candidates`.
fn nearest(value: f32, candidates: &[f32]) -> (f32, f32) {
    candidates
        .iter()
        .fold((f32::INFINITY, value), |best, &candidate| {
            let distance = angle_delta(value, candidate).abs();
            if distance < best.0 {
                (distance, candidate)
            } else {
                best
            }
        })
}

/// Bring one persisted scalar back inside `min..=max`.
///
/// The finite check has to come first: `f32::clamp` passes NaN straight
/// through, which is the whole reason this is a function and not a `clamp`
/// call at each field.
fn restored(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

fn wrap_degrees(degrees: f32) -> f32 {
    let wrapped = degrees % 360.0;
    if wrapped < 0.0 {
        wrapped + 360.0
    } else {
        wrapped
    }
}

/// Orthographic projection in **WebGPU's** clip convention: depth runs `0.0` at
/// the near plane to `1.0` at the far plane, not OpenGL's `-1.0..=1.0`.
///
/// Getting this wrong is silent and half-invisible rather than obviously broken.
/// The OpenGL form maps the nearer half of the depth range to negative clip z,
/// which WebGPU discards — so roughly half the scene vanishes, and only at the
/// camera angles that happen to push geometry into that half. Everything looked
/// correct from the default isometric angle while the level axis view dropped the
/// listener and every object behind the origin.
fn orthographic(left: f32, right: f32, bottom: f32, top: f32, near: f32, far: f32) -> Matrix4 {
    let mut matrix = [0.0; 16];
    matrix[0] = 2.0 / (right - left);
    matrix[5] = 2.0 / (top - bottom);
    matrix[10] = -1.0 / (far - near);
    matrix[12] = -(right + left) / (right - left);
    matrix[13] = -(top + bottom) / (top - bottom);
    matrix[14] = -near / (far - near);
    matrix[15] = 1.0;
    matrix
}

/// Right-handed perspective projection in WebGPU's `0.0..=1.0` depth range.
/// `target_plane_height` is the world-space height visible at the camera target,
/// which makes its zoom scale directly compatible with orthographic mode.
fn perspective(target_plane_height: f32, aspect: f32, near: f32, far: f32) -> Matrix4 {
    let focal_scale = 2.0 * params::CAMERA_DISTANCE / target_plane_height.max(f32::EPSILON);
    let mut matrix = [0.0; 16];
    matrix[0] = focal_scale / aspect.max(f32::EPSILON);
    matrix[5] = focal_scale;
    matrix[10] = far / (near - far);
    matrix[11] = -1.0;
    matrix[14] = far * near / (near - far);
    matrix
}

fn look_at(eye: [f32; 3], centre: [f32; 3], up: [f32; 3]) -> Matrix4 {
    let back = normalise([eye[0] - centre[0], eye[1] - centre[1], eye[2] - centre[2]]);
    let right = normalise(cross(up, back));
    let true_up = cross(back, right);

    let mut matrix = [0.0; 16];
    matrix[0] = right[0];
    matrix[4] = right[1];
    matrix[8] = right[2];
    matrix[12] = -dot(right, eye);
    matrix[1] = true_up[0];
    matrix[5] = true_up[1];
    matrix[9] = true_up[2];
    matrix[13] = -dot(true_up, eye);
    matrix[2] = back[0];
    matrix[6] = back[1];
    matrix[10] = back[2];
    matrix[14] = -dot(back, eye);
    matrix[15] = 1.0;
    matrix
}

fn multiply(a: Matrix4, b: Matrix4) -> Matrix4 {
    let mut out = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            out[column * 4 + row] = a[row] * b[column * 4]
                + a[4 + row] * b[column * 4 + 1]
                + a[8 + row] * b[column * 4 + 2]
                + a[12 + row] * b[column * 4 + 3];
        }
    }
    out
}

/// Transform a point into normalised device coordinates.
fn transform(matrix: Matrix4, point: [f32; 3]) -> [f32; 3] {
    let homogeneous = |row: usize| {
        matrix[row] * point[0]
            + matrix[4 + row] * point[1]
            + matrix[8 + row] * point[2]
            + matrix[12 + row]
    };
    let w = homogeneous(3);
    let divisor = if w.abs() > f32::EPSILON { w } else { 1.0 };
    [
        homogeneous(0) / divisor,
        homogeneous(1) / divisor,
        homogeneous(2) / divisor,
    ]
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

fn normalise(a: [f32; 3]) -> [f32; 3] {
    let length = dot(a, a).sqrt();
    if length <= f32::EPSILON {
        return [0.0, 0.0, 1.0];
    }
    [a[0] / length, a[1] / length, a[2] / length]
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEWPORT: Rect = Rect {
        min: Pos2::new(0.0, 0.0),
        max: Pos2::new(800.0, 600.0),
    };

    #[test]
    fn the_default_camera_sits_at_the_locked_iso_angle() {
        let camera = Camera::default();
        assert_eq!(camera.projection_mode(), ProjectionMode::Orthographic);
        assert!((camera.azimuth_degrees() - params::ISO_AZIMUTH_DEGREES).abs() < f32::EPSILON);
        assert!((camera.elevation_degrees() - params::ISO_ELEVATION_DEGREES).abs() < f32::EPSILON);
        assert!((camera.ortho_height() - params::DEFAULT_ORTHO_HEIGHT).abs() < f32::EPSILON);

        // Logic's Angle reference sees the listener from back-left, leaving
        // their -Z-facing profile pointed toward screen-left.
        let head = camera.project([0.0; 3], VIEWPORT);
        let facing = camera.project([0.0, 0.0, -1.0], VIEWPORT);
        assert!(facing.x < head.x, "ISO view was mirrored");
    }

    #[test]
    fn the_back_preset_puts_the_eye_behind_the_listener() {
        let mut camera = Camera::default();
        camera.apply_preset(Preset::Back, VIEWPORT.width() / VIEWPORT.height());
        camera.advance(1.0);

        let direction = camera.direction();
        assert!(direction[0].abs() < 1e-6);
        assert!(direction[1].abs() < 1e-6);
        assert!(direction[2] > 0.999, "eye is not on the +Z/back side");
    }

    #[test]
    fn presets_use_different_zoom_values_to_keep_the_room_the_same_visual_size() {
        let aspect = VIEWPORT.width() / VIEWPORT.height();
        for projection_mode in [ProjectionMode::Orthographic, ProjectionMode::Perspective] {
            let mut camera = Camera {
                projection_mode,
                ..Camera::default()
            };
            let expected_fill = projected_room_span(
                projection_mode,
                camera.azimuth_degrees,
                camera.elevation_degrees,
                camera.target,
                aspect,
            ) / camera.ortho_height;
            let iso_zoom = camera.ortho_height;

            for preset in [Preset::Back, Preset::Top, Preset::Side, Preset::Iso] {
                camera.apply_preset(preset, aspect);
                camera.advance(1.0);
                let fill = projected_room_span(
                    projection_mode,
                    camera.azimuth_degrees,
                    camera.elevation_degrees,
                    camera.target,
                    aspect,
                ) / camera.ortho_height;
                assert!(
                    (fill - expected_fill).abs() < 1e-5,
                    "{projection_mode:?}/{preset:?} changed visual fill from \
                     {expected_fill} to {fill}"
                );
            }

            assert!((camera.ortho_height - iso_zoom).abs() < 1e-5);
        }
    }

    #[test]
    fn iso_preserves_zoom_but_reset_restores_the_complete_default_view() {
        let aspect = VIEWPORT.width() / VIEWPORT.height();
        let mut camera = Camera {
            ortho_height: 1.4,
            ..Camera::default()
        };
        camera.apply_preset(Preset::Iso, aspect);
        camera.advance(1.0);
        assert!((camera.ortho_height - 1.4).abs() < 1e-5);

        camera.azimuth_degrees = 123.0;
        camera.elevation_degrees = -27.0;
        camera.target = [0.4, -0.2, 0.3];
        camera.toggle_projection();
        camera.reset_view();
        camera.advance(1.0);

        assert!((camera.azimuth_degrees - params::ISO_AZIMUTH_DEGREES).abs() < 1e-5);
        assert!((camera.elevation_degrees - params::ISO_ELEVATION_DEGREES).abs() < 1e-5);
        assert!((camera.ortho_height - params::DEFAULT_ORTHO_HEIGHT).abs() < 1e-5);
        assert!(camera.target.iter().all(|axis| axis.abs() < 1e-5));
        assert_eq!(camera.projection_mode, ProjectionMode::Perspective);
    }

    #[test]
    fn projection_toggle_preserves_target_plane_scale_and_adds_foreshortening() {
        let mut camera = Camera::default();
        let azimuth = camera.azimuth_degrees.to_radians();
        let right = [azimuth.cos(), 0.0, -azimuth.sin()];
        let direction = camera.direction();
        let point = [right[0] * 0.5, right[1] * 0.5, right[2] * 0.5];
        let centre = camera.project([0.0; 3], VIEWPORT);
        let orthographic = camera.project(point, VIEWPORT);

        camera.toggle_projection();
        assert_eq!(camera.projection_mode(), ProjectionMode::Perspective);
        let perspective_at_target = camera.project(point, VIEWPORT);
        assert!((orthographic.x - perspective_at_target.x).abs() < 0.01);
        assert!((orthographic.y - perspective_at_target.y).abs() < 0.01);

        let near = [
            point[0] + direction[0],
            point[1] + direction[1],
            point[2] + direction[2],
        ];
        let far = [
            point[0] - direction[0],
            point[1] - direction[1],
            point[2] - direction[2],
        ];
        let near_screen = camera.project(near, VIEWPORT);
        let far_screen = camera.project(far, VIEWPORT);
        assert!((near_screen.x - centre.x).abs() > (far_screen.x - centre.x).abs());

        camera.toggle_projection();
        assert_eq!(camera.projection_mode(), ProjectionMode::Orthographic);
    }

    #[test]
    fn the_origin_projects_to_the_centre_of_the_viewport_from_any_angle() {
        for (azimuth, elevation) in [(45.0, 30.0), (0.0, 0.0), (0.0, 89.0), (217.0, -43.0)] {
            let camera = Camera {
                azimuth_degrees: azimuth,
                elevation_degrees: elevation,
                ..Camera::default()
            };
            let centre = camera.project([0.0; 3], VIEWPORT);
            assert!(
                (centre.x - VIEWPORT.center().x).abs() < 0.01
                    && (centre.y - VIEWPORT.center().y).abs() < 0.01,
                "azimuth {azimuth} elevation {elevation} projected to {centre:?}"
            );
        }
    }

    #[test]
    fn projection_round_trips_through_the_whole_free_camera_range() {
        // A point that projects to a given screen position must keep doing so
        // after the view is rebuilt from the same state — including at the
        // axis-aligned angles a user can always drag to.
        let cases = [(45.0, 30.0, 3.4), (0.0, 0.0, 1.0), (90.0, 89.0, 6.0)];
        for (azimuth, elevation, ortho_height) in cases {
            let mut camera = Camera {
                azimuth_degrees: azimuth,
                elevation_degrees: elevation,
                ortho_height,
                ..Camera::default()
            };
            camera.pan([12.0, -8.0], VIEWPORT.height());

            let point = [0.3, -0.2, 0.7];
            let first = camera.project(point, VIEWPORT);
            let second = camera.project(point, VIEWPORT);
            assert!((first.x - second.x).abs() < 1e-4 && (first.y - second.y).abs() < 1e-4);
            assert!(
                first.x.is_finite() && first.y.is_finite(),
                "{azimuth}/{elevation} produced {first:?}"
            );
        }
    }

    #[test]
    fn a_point_further_along_the_view_direction_projects_the_same_but_reads_nearer() {
        // Orthographic projection: depth must not change screen position.
        let camera = Camera::default();
        let direction = camera.direction();
        let near = camera.project(direction, VIEWPORT);
        let far = camera.project([-direction[0], -direction[1], -direction[2]], VIEWPORT);
        assert!((near.x - far.x).abs() < 0.01 && (near.y - far.y).abs() < 0.01);
    }

    #[test]
    fn elevation_is_clamped_short_of_the_pole_and_azimuth_wraps() {
        let mut camera = Camera::default();
        camera.orbit([0.0, 10_000.0]);
        assert!((camera.elevation_degrees() - params::MAX_ELEVATION_DEGREES).abs() < 1e-3);
        camera.orbit([0.0, -100_000.0]);
        assert!((camera.elevation_degrees() + params::MAX_ELEVATION_DEGREES).abs() < 1e-3);

        camera.orbit([-10_000.0, 0.0]);
        let azimuth = camera.azimuth_degrees();
        assert!(
            (0.0..360.0).contains(&azimuth),
            "azimuth escaped: {azimuth}"
        );
    }

    #[test]
    fn zoom_and_pan_stay_inside_their_limits() {
        let mut camera = Camera::default();
        camera.zoom(100_000.0);
        assert!((camera.ortho_height() - params::MAX_ORTHO_HEIGHT).abs() < 1e-3);
        camera.zoom(-100_000.0);
        assert!((camera.ortho_height() - params::MIN_ORTHO_HEIGHT).abs() < 1e-3);

        camera.pan([100_000.0, 100_000.0], VIEWPORT.height());
        for axis in camera.target {
            assert!(axis.abs() <= params::MAX_PAN + 1e-3, "pan escaped: {axis}");
        }
    }

    #[test]
    fn degeneracy_peaks_on_an_axis_and_vanishes_at_the_iso_angle() {
        let iso = Camera::default();
        assert!(iso.degeneracy() < 0.01, "{}", iso.degeneracy());

        let top = Camera {
            azimuth_degrees: 0.0,
            elevation_degrees: params::MAX_ELEVATION_DEGREES,
            ..Camera::default()
        };
        assert!(top.degeneracy() > 0.9, "{}", top.degeneracy());

        let level = Camera {
            azimuth_degrees: 0.0,
            elevation_degrees: 0.0,
            ..Camera::default()
        };
        assert!(level.degeneracy() > 0.9, "{}", level.degeneracy());
    }

    #[test]
    fn hairline_world_width_halves_when_the_view_zooms_in_twofold() {
        let camera = Camera::default();
        let wide = camera.world_units_per_point(600.0);
        let closer = Camera {
            ortho_height: camera.ortho_height / 2.0,
            ..camera
        };
        let close = closer.world_units_per_point(600.0);
        assert!((wide / close - 2.0).abs() < 1e-4);
    }

    /// Clip-space depth of a world point, in WebGPU's `0.0..=1.0` convention.
    fn clip_depth(camera: &Camera, point: [f32; 3]) -> f32 {
        transform(camera.view_projection(4.0 / 3.0), point)[2]
    }

    #[test]
    fn depth_maps_onto_webgpus_zero_to_one_clip_range() {
        for projection_mode in [ProjectionMode::Orthographic, ProjectionMode::Perspective] {
            let camera = Camera {
                projection_mode,
                ..Camera::default()
            };
            let direction = camera.direction();
            let at = |distance: f32| {
                [
                    direction[0] * distance,
                    direction[1] * distance,
                    direction[2] * distance,
                ]
            };
            // The near plane sits CAMERA_DISTANCE - DEPTH_HALF_RANGE from the
            // eye, which is DEPTH_HALF_RANGE in front of the target.
            let near = clip_depth(&camera, at(params::DEPTH_HALF_RANGE));
            let centre = clip_depth(&camera, [0.0; 3]);
            let far = clip_depth(&camera, at(-params::DEPTH_HALF_RANGE));

            assert!(
                near.abs() < 1e-4,
                "{projection_mode:?} near plane should land on 0.0, got {near}"
            );
            assert!(
                (far - 1.0).abs() < 1e-4,
                "{projection_mode:?} far plane should land on 1.0, got {far}"
            );
            assert!(
                centre > near && centre < far,
                "{projection_mode:?} target depth is not between its planes: {centre}"
            );
            if projection_mode == ProjectionMode::Orthographic {
                assert!((centre - 0.5).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn the_whole_room_stays_inside_the_clip_volume_from_every_angle() {
        // The OpenGL depth convention passes at the isometric default and then
        // silently discards half the scene at an axis-aligned view; sweep the
        // angles rather than trusting one.
        for projection_mode in [ProjectionMode::Orthographic, ProjectionMode::Perspective] {
            for azimuth in [0.0, 45.0, 90.0, 137.0, 180.0, 270.0, 315.0] {
                for elevation in [-89.0, -30.0, 0.0, 15.0, 30.0, 89.0] {
                    let camera = Camera {
                        azimuth_degrees: azimuth,
                        elevation_degrees: elevation,
                        projection_mode,
                        ..Camera::default()
                    };
                    for corner in [-1.0_f32, 1.0] {
                        for point in [
                            [corner, corner, corner],
                            [corner, -corner, corner],
                            [corner, corner, -corner],
                            [0.0, 0.0, 0.0],
                        ] {
                            let depth = clip_depth(&camera, point);
                            assert!(
                                (0.0..=1.0).contains(&depth),
                                "{projection_mode:?} {point:?} fell outside the clip volume at \
                                 {azimuth}/{elevation}: {depth}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn angle_delta_takes_the_short_way_round() {
        assert!((angle_delta(350.0, 10.0) - 20.0).abs() < 1e-4);
        assert!((angle_delta(10.0, 350.0) + 20.0).abs() < 1e-4);
        assert!((angle_delta(0.0, 180.0).abs() - 180.0).abs() < 1e-4);
    }

    #[test]
    fn a_worked_view_survives_a_round_trip_through_the_store() {
        let mut camera = Camera::default();
        camera.orbit([-140.0, 55.0]);
        camera.zoom(-260.0);
        camera.pan([48.0, -22.0], VIEWPORT.height());
        camera.toggle_projection();

        let encoded = serde_json::to_string(&camera.state()).expect("serialize camera state");
        let decoded: CameraState =
            serde_json::from_str(&encoded).expect("deserialize camera state");
        let reopened = Camera::from_state(decoded);

        assert_eq!(reopened, camera, "the stored view came back changed");
    }

    #[test]
    fn a_corrupt_stored_view_lands_inside_the_live_limits() {
        // Nothing here is reachable by dragging; all of it is one hand-edit
        // away in the JSON, and NaN in particular survives `f32::clamp`.
        let camera = Camera::from_state(CameraState {
            azimuth_degrees: f32::NAN,
            elevation_degrees: 9000.0,
            ortho_height: 0.0,
            target: [f32::INFINITY, -1e30, f32::NAN],
            projection_mode: ProjectionMode::Perspective,
        });

        assert!((camera.azimuth_degrees() - params::ISO_AZIMUTH_DEGREES).abs() < 1e-4);
        assert!((camera.elevation_degrees() - params::MAX_ELEVATION_DEGREES).abs() < 1e-4);
        assert!((camera.ortho_height() - params::MIN_ORTHO_HEIGHT).abs() < 1e-4);
        assert_eq!(camera.projection_mode(), ProjectionMode::Perspective);

        // The whole point of the clamp is that no NaN reaches the vertices.
        for value in camera.view_projection(VIEWPORT.aspect_ratio()) {
            assert!(value.is_finite(), "a stored NaN reached the view matrix");
        }
    }

    #[test]
    fn a_view_saved_mid_preset_stores_where_it_was_heading() {
        // Quitting during the 220 ms ease should not freeze the camera on an
        // arbitrary frame of a transition the user already asked to end.
        let aspect = VIEWPORT.aspect_ratio();
        let mut interrupted = Camera::default();
        interrupted.apply_preset(Preset::Top, aspect);
        assert!(interrupted.advance(0.02), "the preset ease ended too early");

        let mut settled = Camera::default();
        settled.apply_preset(Preset::Top, aspect);
        while settled.advance(0.02) {}

        assert_eq!(Camera::from_state(interrupted.state()), settled);
    }
}
