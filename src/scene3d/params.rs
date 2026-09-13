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

/// Fraction of an element face available to its scene number. The label scales
/// down as the number gains digits and also fits the shallow LFE cabinet faces.
pub const OBJECT_LABEL_FACE_FILL: f32 = 0.72;

/// Distance the label sits above the cube face, in world units. It is large
/// enough to win the strict depth test without reading as detached lettering.
pub const OBJECT_LABEL_SURFACE_OFFSET: f32 = 0.0015;

/// Stroke width of the face label in screen points. Like every other annotation
/// it stays legible while zooming rather than scaling into a heavy world line.
pub const OBJECT_LABEL_STROKE_POINTS: f32 = 1.15;

/// Linear gain below which a positioned object reads as present but silent.
/// -36 dB, the floor the mockup settled on for gain-driven appearance.
pub const OBJECT_SILENT_GAIN: f32 = 0.015_848_932;

/// How far a silent object is pushed toward `STAGE`. Fading toward the ground
/// is the same mechanism air perspective uses, so a silent object recedes the
/// way a distant one does instead of introducing a second visual language.
pub const OBJECT_SILENT_FADE: f32 = 0.55;

/// Footprint edge at the silence floor, as a multiple of [`OBJECT_EDGE`]. The
/// footprint carries gain by growing, which is why it is not a fixed size.
///
/// Gain rides on the floor rather than on anything wrapped around the cube. An
/// outline concentric with a solid box is the universal signature of a debug
/// collision volume, and reads as one however faintly it is drawn — the problem
/// is the shape, not the weight. The footprint is already there, is coplanar
/// with the floor, and so can never cross a face or hide the scene number; a
/// pool that widens under a louder object is the reading, and the floor grid is
/// already the ruler it is measured against.
///
/// The scale is read in decibels, not in linear gain: a linear map crushes the
/// whole lower thirty decibels against the floor, where most of the interesting
/// range lives.
///
/// **The floor is [`OBJECT_SILENT_GAIN`], deliberately not a second constant.**
/// The mockup's table listed a `-36 dB` gain floor, which is the same number
/// this already is. Sharing it means the footprint bottoming out and the cube
/// fading to its silent colour happen at exactly the same gain by construction
/// — one threshold, two channels, and no way for them to drift apart later.
///
/// The minimum stays clearly visible: the footprint is also the depth cue that
/// places an airborne object on the grid at a grazing view, so a silent object
/// may not lose it.
///
/// Below `1.0` on purpose, and the one view where that costs anything has its own
/// answer. An *orthographic* straight-down view projects a cube and the footprint
/// directly beneath it onto the same place, so anything narrower than the cube is
/// hidden — a property of the projection, not of this encoding. Perspective
/// separates them by parallax, by more the further the object sits from the view
/// axis, and the toolbar already toggles between the two. So the quiet end can
/// stay small and the floor stays calm.
pub const FOOTPRINT_MIN_SCALE: f32 = 0.45;
/// Footprint edge at unity gain and above. Gains past unity clamp here rather
/// than growing without bound across the neighbouring objects' floor.
pub const FOOTPRINT_MAX_SCALE: f32 = 1.60;

/// Hairline width of the gain ring, in screen points.
///
/// With measured loudness available the footprint carries two readings instead
/// of one: the ring is still gain — the width the metadata *asks* for — and the
/// filled core is the level the object actually delivers, read on the very same
/// decibel scale. The core can therefore never exceed the ring, and the gap
/// between them is the whole point: a wide ring around an empty core is an
/// object that was positioned and gained but has nothing in it, which is
/// exactly the mistake a gain-only footprint cannot show.
///
/// Splitting the floor mark rather than adding a mark keeps the scene's object
/// count of visual elements unchanged, and keeps the reading where
/// [`FOOTPRINT_MIN_SCALE`] argues it belongs: coplanar with the floor, where it
/// can cross no face and hide no scene number.
pub const FOOTPRINT_RING_POINTS: f32 = 0.9;

/// Audio the fast meter averages before the ballistics see it, in milliseconds.
///
/// Short enough that a transient is not averaged away, long enough that the
/// reading is a level rather than a sample. The attack and release below are
/// what actually set the meter's feel; this only decides what it is chasing.
pub const METER_WINDOW_MILLISECONDS: u32 = 30;
/// Meter attack time constant, in milliseconds. Fast, so a hit reads as a hit.
pub const METER_ATTACK_MILLISECONDS: f32 = 10.0;
/// Meter release time constant, in milliseconds.
///
/// Slow enough to read, and deliberately not the 400 ms of the momentary
/// window: that window is a *measurement* the off-stage readout reports, while
/// this is the feel of a meter riding a moving object. Loading both onto one
/// number would make the picture lag the sound by a window length.
pub const METER_RELEASE_MILLISECONDS: f32 = 300.0;

/// How far above a cube's top face the loudness nameplate is anchored, in world
/// units. Clear of the face label without floating free of the object.
pub const NAMEPLATE_OFFSET: f32 = 0.055;

/// Characters the nameplate's readout reserves: sign, two integer digits, the
/// point, one decimal — `-00.0`, the widest value the scale can produce.
///
/// The plate is sized from this rather than from the value it currently shows,
/// and the two cells inside it are fixed: the sign owns the first, the digits
/// are right-aligned against the last. A plate that grew with its own value
/// would make the level strip beneath it mean a different number of pixels on
/// every object, and right-aligning the whole string instead would pin the
/// decimal point but leave the sign hopping a cell whenever the level crossed
/// -10 dB. Neither is a scale.
pub const NAMEPLATE_CELLS: usize = 5;
/// Readout size in screen points. Fixed, so the plate does not grow with zoom.
pub const NAMEPLATE_TEXT_POINTS: f32 = 11.0;
/// Horizontal padding inside the plate, in screen points.
pub const NAMEPLATE_PAD_POINTS: f32 = 6.0;
/// Height of the level strip along the plate's bottom edge, in screen points.
pub const NAMEPLATE_STRIP_POINTS: f32 = 3.0;
/// Opacity of a nameplate whose object has fallen below the silence floor.
/// It recedes rather than disappearing, as the cube itself does.
pub const NAMEPLATE_SILENT_ALPHA: f32 = 0.32;

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

/// Breadcrumb size at the silence floor and at unity, as multiples of
/// [`TRAIL_MARK_SCALE`], when measured loudness is shown.
///
/// This is the one place the trail carries something other than time, and it is
/// allowed to because of *which* loudness it carries. `add_trail` argues that
/// tinting past marks with the present gain asserts something that was never
/// true; the mirror records a reading taken at the moment each mark was, so
/// this says only what was true then. The range stays narrow — the gap between
/// marks is still speed, and a size swing large enough to compete with it would
/// cost the reading the trail already has.
pub const TRAIL_LOUD_MIN_SCALE: f32 = 0.55;
/// Breadcrumb size at unity gain, as a multiple of [`TRAIL_MARK_SCALE`].
pub const TRAIL_LOUD_MAX_SCALE: f32 = 1.45;

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
