//! Visual constants for the object scene view.
//!
//! These are locked in the AC-4 Scene Calibrator (the web mockup that prototyped
//! this renderer); tune there and paste the result here rather than nudging
//! values by hand. Constants land in this file as the features that consume them
//! land, so the set here is smaller than the mockup exposes.
//!
//! Two constraints are easy to violate and hard to spot afterwards:
//!
//! * Actor sizes are world units with the room half-extent as `1.0`, and are
//!   deliberately independent of [`ROOM_BLOCKS`]. Tying both to a single "block"
//!   unit makes the grid resolution silently resize the listener and the objects
//!   together, so their ratio can never be corrected — only the whole scene
//!   scales. The grid is a ruler; the listener and objects are actors.
//! * Three-tone shading lerps toward [`crate::theme::INK`], so any base colour
//!   already near INK loses all separation between faces and renders as one flat
//!   dark mass. Keep bases in the mid range.

/// Divisions along each floor axis. Purely the ruler's resolution — it must not
/// feed any actor's size.
///
/// Readability comes from the three-tier weighting in
/// [`crate::scene3d::scene`], not from this number: at a single weight, 8 and 16
/// divisions were indistinguishable, which means density carries no information.
pub const ROOM_BLOCKS: u32 = 16;

/// Hairline width in egui points, matched to the 1px strokes the rest of the UI
/// uses. Converted to world units against the current orthographic height so it
/// stays this wide at any zoom (see [`Camera::world_units_per_point`]).
///
/// [`Camera::world_units_per_point`]: crate::scene3d::camera::Camera::world_units_per_point
pub const HAIRLINE_POINTS: f32 = 1.0;

/// How far the floor grid is pushed from `BORDER` toward `MUTED`.
pub const FLOOR_GRID_CONTRAST: f32 = 0.35;

/// Face tones, applied by lerping the base colour toward `INK` by `1.0 - tone`.
/// Top face keeps the base; the two side families step down from there.
pub const TONE_TOP: f32 = 1.00;
/// Tone for faces whose dominant normal is on the X axis.
pub const TONE_LEFT: f32 = 0.88;
/// Tone for faces whose dominant normal is on the Z axis.
pub const TONE_RIGHT: f32 = 0.76;

/// Maximum lerp toward `STAGE` applied to distant geometry.
pub const AIR_PERSPECTIVE: f32 = 0.18;

/// Extra air perspective applied as the view approaches an axis. At an
/// axis-aligned view an AABB shows a single face, so three-tone shading collapses
/// and the projection carries no depth at all; this is what replaces both.
pub const DEGENERATE_VIEW_BOOST: f32 = 0.90;

/// World-space span over which air perspective ramps from none to full.
pub const AIR_PERSPECTIVE_SPAN: f32 = 3.5;

/// Edge length of a dynamic object's cube, in world units.
pub const OBJECT_EDGE: f32 = 0.05;

/// Default orthographic height, framing the room with a little air around it.
pub const DEFAULT_ORTHO_HEIGHT: f32 = 3.4;
/// Zoomed all the way in: a single object fills a good part of the viewport.
pub const MIN_ORTHO_HEIGHT: f32 = 0.35;
/// Zoomed all the way out.
pub const MAX_ORTHO_HEIGHT: f32 = 6.0;

/// Default camera azimuth in degrees.
pub const ISO_AZIMUTH_DEGREES: f32 = 45.0;
/// Default camera elevation in degrees.
pub const ISO_ELEVATION_DEGREES: f32 = 30.0;

/// How close a released drag has to land before the view settles onto a
/// canonical angle. This never blocks an angle — it only makes the clean
/// readings easy to hit.
pub const SNAP_TOLERANCE_DEGREES: f32 = 6.0;

/// Easing time for a snap or a preset.
pub const SNAP_DAMPING_MILLISECONDS: u64 = 220;

/// Elevation limit. Deliberately short of 90 degrees: looking exactly down the
/// Y axis degenerates the view matrix's up vector. This is a numerical guard,
/// not a design restriction — every angle inside it is reachable.
pub const MAX_ELEVATION_DEGREES: f32 = 89.0;

/// How far the view target may be panned away from the room's centre.
pub const MAX_PAN: f32 = 2.0;

/// Distance from the view target to the eye. Only sets where the depth range
/// sits; an orthographic projection's framing comes from the ortho height.
pub const CAMERA_DISTANCE: f32 = 8.0;

/// Half-depth of the clip range around the view target. Kept tight on purpose:
/// orthographic depth precision is uniform, so a narrow range leaves the decal
/// bias plenty of headroom against z-fighting on the floor plane.
pub const DEPTH_HALF_RANGE: f32 = 6.0;
