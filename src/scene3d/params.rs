//! Visual constants for the object scene view.
//!
//! These are locked in the AC-4 Scene Calibrator (the web mockup that prototyped
//! this renderer); tune there and paste the result here rather than nudging
//! values by hand. Constants land in this file as the features that consume them
//! land, so the set here is smaller than the mockup exposes.
//!
//! Two constraints are easy to violate and hard to spot afterwards:
//!
//! * Actor sizes are world units and deliberately independent of
//!   [`ROOM_BLOCKS`]. Tying both to a single "block" unit makes the grid
//!   resolution silently resize the listener and the objects together, so their
//!   ratio can never be corrected — only the whole scene scales. The grid is a
//!   ruler; the listener and objects are actors.
//! * Three-tone shading lerps toward [`crate::theme::INK`], so any base colour
//!   already near INK loses all separation between faces and renders as one flat
//!   dark mass. Keep bases in the mid range.

/// Major blocks along each floor axis. Four matches the coarse ruler used by
/// the Logic spatial view; it remains independent of every actor's size.
///
/// Readability comes from graduated weighting in [`crate::scene3d::scene`]: the
/// centre axes remain strongest, these block boundaries sit in the middle, and
/// the subdivisions below form the fine ruler.
pub const ROOM_BLOCKS: u32 = 4;

/// Fine cells inside each major room block. Four restores the earlier 16×16
/// ruler without losing the visually dominant 4×4 structure.
pub const GRID_SUBDIVISIONS_PER_BLOCK: u32 = 4;

/// Total fine divisions drawn along each floor axis.
pub const ROOM_GRID_DIVISIONS: u32 = ROOM_BLOCKS * GRID_SUBDIVISIONS_PER_BLOCK;

/// Room width in world units. Logic's top view is square, so width and depth
/// match while the front view establishes a deliberately lower ceiling.
pub const ROOM_WIDTH: f32 = 2.0;
/// Room height in world units. A 3:5 height-to-width ratio gives the low
/// rectangular volume visible in Logic's front and side references.
pub const ROOM_HEIGHT: f32 = 1.2;
/// Room depth in world units.
pub const ROOM_DEPTH: f32 = 2.0;

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

/// Edge length of a dynamic object's cube, in world units. Logic's marker is
/// roughly 28% of its listener's head-and-shoulder envelope in the supplied
/// reference views.
pub const OBJECT_EDGE: f32 = 0.11;

/// Linear gain below which a positioned object reads as present but silent.
/// -36 dB, the floor the mockup settled on for gain-driven appearance.
pub const OBJECT_SILENT_GAIN: f32 = 0.015_848_932;

/// How far a silent object is pushed toward `STAGE`. Fading toward the ground
/// is the same mechanism air perspective uses, so a silent object recedes the
/// way a distant one does instead of introducing a second visual language.
pub const OBJECT_SILENT_FADE: f32 = 0.55;

/// Trail breadcrumbs kept per object, and how far apart in time they are taken.
/// Forty at forty milliseconds is 1.6 seconds of history.
///
/// The trail is stroboscopic on purpose: discrete marks at a fixed time
/// interval, so **the gap between marks is speed**. A continuous ribbon would
/// throw that away and add a width channel carrying nothing. It is also honest
/// about what the data is — `backend::source::element_state_at` shows the real
/// trajectory is a piecewise-linear polyline, so an OAMD ramp comes out as
/// evenly spaced marks and a `ramp_frames == 0` jump as one long gap.
pub const TRAIL_SAMPLES: usize = 40;
/// Sampling interval for the trail, in milliseconds.
pub const TRAIL_INTERVAL_MILLISECONDS: u32 = 40;

/// How far the oldest breadcrumb is pushed toward `STAGE`. Younger marks
/// interpolate up from here, which is what makes the trail read directionally
/// without needing an arrowhead.
pub const TRAIL_FADE: f32 = 0.85;

/// Weight of the trail's floor projection relative to its airborne marks. The
/// projection is not decoration: at a grazing or axis-aligned view it is the
/// only thing placing the path on the grid.
pub const FLOOR_TRAIL_WEIGHT: f32 = 0.45;

/// Breadcrumb edge as a fraction of [`OBJECT_EDGE`]. Small enough that a dense
/// trail does not read as a second row of objects.
pub const TRAIL_MARK_SCALE: f32 = 0.30;

/// Samples of the path ahead kept per object. At [`TRAIL_INTERVAL_MILLISECONDS`]
/// apart this is 1.92 seconds, just under the Scene FIFO's two-second capacity,
/// so **the length of the dashed path is the buffer depth**: when the decoder
/// falls behind, the line ahead of each object visibly shortens.
///
/// The sampling interval is deliberately the trail's, not a third number. Past
/// and future are then the same clock read in two directions, and a recompute
/// can never lag by more than one sample of the path's own resolution.
pub const FUTURE_SAMPLES: usize = 48;

/// Fraction of each future segment actually drawn, leaving the rest as the gap.
/// Because the samples are evenly spaced in time, **the dash length is speed**,
/// the same reading the breadcrumb spacing gives — one convention, two tenses.
pub const FUTURE_DASH_DUTY: f32 = 0.55;

/// How far the farthest future segment is pushed toward `STAGE`. The mirror of
/// [`TRAIL_FADE`]: history fades backwards, the path ahead fades forwards.
pub const FUTURE_FADE: f32 = 0.85;

/// How far two consecutive samples have to be apart, in normalized units,
/// before an instant metadata update is worth annotating as a jump.
///
/// Two different questions live here and must not be conflated. Whether the
/// update was a discontinuity is a **fact**, decided in `backend::state` by
/// `ramp_frames == 0` — no heuristic. Whether it is worth drawing a marker for
/// is a **perceptual** judgement, and it belongs here: a stream that sends
/// instant updates for every small correction would otherwise turn the whole
/// path into a chain of hollow marks, which is worse than the problem. A jump
/// of two hundredths of a room is not one anybody loses track of.
pub const JUMP_MIN_DISTANCE: f32 = 0.30;

/// Jump marker edge, relative to a breadcrumb's. Slightly larger, and hollow
/// where a breadcrumb is solid: the marker says "appeared here", not "was
/// sampled here".
pub const JUMP_MARK_SCALE: f32 = 1.6;

/// The jump arrow, in screen points. It is an annotation rather than an object,
/// so its size is fixed on screen — growing with zoom would read as broken, and
/// growing with the jump distance would read as a path.
///
/// Large enough to survive being read next to the endpoint marker, which *is*
/// world-sized: zoom in far enough and the marker grows while the arrow does
/// not, so a shaft that merely clears the box at one zoom disappears into it at
/// another.
pub const JUMP_ARROW_POINTS: f32 = 26.0;
/// Barb length of the jump arrow's head, in screen points.
pub const JUMP_ARROW_HEAD_POINTS: f32 = 9.0;
/// Half-angle between the jump arrow's barbs and its shaft.
pub const JUMP_ARROW_HEAD_DEGREES: f32 = 32.0;

/// Outer shoulder width of the listener, in world units. This is the scale
/// anchor: three head-and-shoulder envelopes span the room height. The complete
/// canonical Minecraft figure is twice this height, so a standing body occupies
/// two thirds of the low room without shrinking its upper body.
pub const FIGURE_SHOULDER_WIDTH: f32 = ROOM_HEIGHT / 3.0;

/// Floor chosen so the standing figure's head centre remains at the acoustic
/// origin: 12 leg + 12 torso + 4 half-head model units below it.
pub const ROOM_FLOOR_Y: f32 = -FIGURE_SHOULDER_WIDTH * 28.0 / 16.0;
/// The low ceiling completes the rectangular room above the asymmetric floor.
pub const ROOM_CEILING_Y: f32 = ROOM_FLOOR_Y + ROOM_HEIGHT;

/// The LFE cabinet, in world units. It is deliberately non-cubic: the shape
/// alone says "not one of the dynamic objects".
pub const LFE_SLAB_WIDTH: f32 = 0.50;
/// Height of the LFE cabinet.
pub const LFE_SLAB_HEIGHT: f32 = 0.22;
/// How far the cabinet is sunk into the front wall, `0.0` flush to `1.0` fully
/// buried.
pub const LFE_WALL_INSET: f32 = 0.50;

/// Default orthographic height, framing the room with a little air around it.
pub const DEFAULT_ORTHO_HEIGHT: f32 = 2.7;
/// Zoomed all the way in: a single object fills a good part of the viewport.
pub const MIN_ORTHO_HEIGHT: f32 = 0.35;
/// Zoomed all the way out.
pub const MAX_ORTHO_HEIGHT: f32 = 6.0;

/// Default camera azimuth in degrees, from the listener's back-left as in
/// Logic's Angle view.
pub const ISO_AZIMUTH_DEGREES: f32 = 325.0;
/// Default camera elevation in degrees; lower than a geometric isometric view.
pub const ISO_ELEVATION_DEGREES: f32 = 20.0;

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
