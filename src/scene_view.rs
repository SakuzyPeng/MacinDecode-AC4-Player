//! The audio thread's view of the object scene, mirrored for the UI.
//!
//! The Windows render callback is the only place that knows where every object
//! actually is at a given instant, and it runs on the WASAPI event thread. This
//! module is the one-way channel from there to the frame that draws it.
//!
//! Three rules keep it out of the audio thread's way, and all three are load
//! bearing rather than stylistic:
//!
//! * **The writer never blocks.** [`SceneViewMirror::write_with_lfe`] takes the lock with
//!   `try_lock` and drops the update if the UI happens to hold it. A missed
//!   frame is invisible; a late WASAPI callback is a glitch.
//! * **Neither side allocates.** The object array is fixed at
//!   [`MAX_VIEW_OBJECTS`] and [`SceneViewFrame`] is `Copy`, so a write is a
//!   memcpy and a scene with more objects than the budget is truncated rather
//!   than grown.
//! * **The reader copies and leaves.** The UI takes the frame out under the lock
//!   and builds its geometry from the copy. Holding the lock across mesh
//!   assembly would make the writer's `try_lock` fail for the whole of every
//!   frame, and the mirror would silently stop updating.
//!
//! Staleness is handled by [`PlaybackKey`] rather than by clearing: a seek, a
//! new source, or a device recovery bumps the key, and a frame stamped with a
//! superseded one is simply not returned. That is the same gate the Scene FIFO
//! itself uses, so the view can never show PCM positions from a playback the
//! stream has already moved past.

use std::sync::{Mutex, PoisonError};

use crate::decoder::{ContentHeadTracking, PlaybackKey, TrackingSummary};
use crate::scene3d::params::{TRAIL_INTERVAL_MILLISECONDS, TRAIL_SAMPLES};

/// Objects the view will draw. The design budget is 20 dynamic objects plus the
/// static LFE slot; a scene beyond it is truncated and reported, never grown,
/// because growing it would move the allocation onto the audio thread.
pub const MAX_VIEW_OBJECTS: usize = 20;
/// LFE metering never consumes a dynamic-object slot or carries a position.
pub const LFE_METER_SLOT: usize = MAX_VIEW_OBJECTS;
pub const METER_SLOTS: usize = MAX_VIEW_OBJECTS + 1;

/// Loudness bins kept per object, and the audio each one nominally covers.
///
/// Forty at ten milliseconds is four hundred milliseconds — the ITU-R BS.1770
/// momentary window, so a reader that sums the whole ring is computing exactly
/// the quantity the standard defines and not an approximation of it.
///
/// Ten milliseconds is also the Windows render quantum, so on that path a bin
/// is one publication. The other two paths publish faster (`MacinRender` polls
/// the device every two milliseconds) or slower (the preview is capped at a
/// quarter second), which is why a bin closes once it *holds* a bin's worth of
/// frames rather than when a clock says so: the grid then comes out the same
/// on all three regardless of how often each one publishes.
pub const LOUDNESS_BINS: usize = 40;
/// Audio one loudness bin nominally covers, in milliseconds.
pub const LOUDNESS_BIN_MILLISECONDS: u32 = 10;

/// Frames in one loudness bin, at least one so the grid can never stall on a
/// pathological sample rate.
#[must_use]
pub fn loudness_bin_frames(sample_rate: u32) -> u32 {
    (u64::from(sample_rate) * u64::from(LOUDNESS_BIN_MILLISECONDS) / 1000)
        .try_into()
        .unwrap_or(u32::MAX)
        .max(1)
}

/// Energy measured over one publication window: K-weighted for dynamic
/// objects, unweighted for the LFE channel, which is excluded from BS.1770.
///
/// Energy rather than a finished meter reading, and that is the load-bearing
/// choice in this module. The three consumers publish at cadences that differ
/// by orders of magnitude, so any value advanced one step per publication would
/// behave differently on each; and [`SceneViewMirror::write_with_lfe`] deliberately holds
/// the previous frame when an update carries no objects, so a decaying value
/// stored here would freeze rather than fall. Ballistics therefore belong where
/// the picture is drawn, applied to what these bins accumulated.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ObjectEnergy {
    /// Sum of squared samples (K-weighted for objects), multiplied by the OAMD
    /// gain the renderer was handed — this is the level the output will
    /// produce, not the level in the object's own track. Accumulated as `f64`
    /// to stay inside the reference implementation's own precision.
    pub sum_squares: f64,
    /// Frames the sum covers. A timeline gap contributes frames carrying no
    /// energy, because a gap really does render silence; an underrun
    /// contributes neither, because a transport fault is not content.
    pub frames: u32,
    /// Largest absolute sample in the window, unweighted, for clip indication.
    pub peak: f32,
}

impl ObjectEnergy {
    /// Fold `other` into this window. A consumer that walks a publication in
    /// several spans accumulates them this way rather than publishing each.
    #[cfg_attr(
        not(feature = "decode"),
        allow(
            dead_code,
            reason = "energy is accumulated by the render callback or the scene \
                      preview, and neither exists without a decoder"
        )
    )]
    pub fn absorb(&mut self, other: Self) {
        self.sum_squares += other.sum_squares;
        self.frames = self.frames.saturating_add(other.frames);
        self.peak = self.peak.max(other.peak);
    }
}

/// One closed or filling loudness bin.
///
/// The sum stays `f64` all the way to the reader rather than narrowing for
/// storage. Forty bins for twenty objects is under thirteen kilobytes either
/// way, and keeping the width means the whole chain from the filter to the
/// displayed decibel carries the reference implementation's precision, with no
/// narrowing step to account for when the cross-check disagrees.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LoudnessBin {
    pub sum_squares: f64,
    pub frames: u32,
    pub peak: f32,
}

/// One dynamic object as the render callback submitted it to Windows.
///
/// `position` is already in the listener space `backend/source.rs` renders in —
/// Core/ADM `[x, y, z]` mapped to `[x, z, -y]` — which is the same space
/// `scene3d` draws in, so the view never re-derives it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ObjectView {
    pub element_id: u64,
    /// Whether Windows was asked to spatialize this element: its metadata is
    /// active and semantically complete. An inactive element has no position
    /// worth trusting.
    pub active: bool,
    pub position: [f32; 3],
    pub gain: f32,
    pub tracking: ContentHeadTracking,
    /// Whether an instant (`ramp_frames == 0`) metadata update landed for this
    /// element inside the quantum being published. The mirror latches it until
    /// the next breadcrumb, because quanta are far shorter than the sampling
    /// interval and the flag belongs to a sampled point, not to a quantum.
    pub jumped: bool,
    /// What this element's audio measured over the window being published.
    pub energy: ObjectEnergy,
}

/// The positionless bed channel, displayed as channel zero.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LfeView {
    pub element_id: u64,
    pub active: bool,
    pub gain: f32,
    pub energy: ObjectEnergy,
}

/// One instant of the scene, as the audio thread last saw it, plus the recent
/// history of where each object has been.
#[derive(Debug, Clone, Copy)]
pub struct SceneViewFrame {
    objects: [ObjectView; MAX_VIEW_OBJECTS],
    lfe: Option<LfeView>,
    /// Objects the render callback resolved, which may exceed the array.
    total_objects: usize,
    /// Breadcrumbs per object slot, oldest first and contiguous. Kept as a
    /// shift-down array rather than a ring: at forty points the shift is a
    /// half-kilobyte memmove once every sampling interval, and in exchange the
    /// scene can hand a plain slice straight to the mesh builder instead of
    /// stitching two halves back together every frame.
    trails: [[[f32; 3]; TRAIL_SAMPLES]; MAX_VIEW_OBJECTS],
    trail_lens: [usize; MAX_VIEW_OBJECTS],
    /// Which breadcrumbs the object arrived at rather than travelled to.
    trail_jumps: [[bool; TRAIL_SAMPLES]; MAX_VIEW_OBJECTS],
    /// How loud the object was *when* each breadcrumb was taken, as a linear
    /// level. Recorded rather than derived: `add_trail` argues that painting
    /// history with the present gain asserts something that was not true, and
    /// the same objection would apply to the present level — but a reading
    /// taken at the moment the mark was is simply what was true then.
    trail_loudness: [[f32; TRAIL_SAMPLES]; MAX_VIEW_OBJECTS],
    /// A discontinuity seen since the last breadcrumb was taken. Latched here
    /// because a quantum is roughly a quarter of the sampling interval, so the
    /// update that jumped is usually not the one being sampled.
    pending_jump: [bool; MAX_VIEW_OBJECTS],
    /// Loudness bins per object slot, oldest first and contiguous. The last
    /// entry of each slot is the bin still filling, so a reader always has the
    /// freshest audio available rather than waiting a bin for it.
    loudness: [[LoudnessBin; LOUDNESS_BINS]; METER_SLOTS],
    loudness_lens: [usize; METER_SLOTS],
    /// Presentation frame at which the next breadcrumb is due.
    next_trail_frame: i64,
    /// `None` until the first write. Distinguishes "no playback yet" from a
    /// playback that legitimately carries no objects.
    key: Option<PlaybackKey>,
    tracking: TrackingSummary,
}

impl Default for SceneViewFrame {
    fn default() -> Self {
        Self {
            objects: [ObjectView::default(); MAX_VIEW_OBJECTS],
            lfe: None,
            total_objects: 0,
            trails: [[[0.0; 3]; TRAIL_SAMPLES]; MAX_VIEW_OBJECTS],
            trail_lens: [0; MAX_VIEW_OBJECTS],
            trail_jumps: [[false; TRAIL_SAMPLES]; MAX_VIEW_OBJECTS],
            trail_loudness: [[0.0; TRAIL_SAMPLES]; MAX_VIEW_OBJECTS],
            pending_jump: [false; MAX_VIEW_OBJECTS],
            loudness: [[LoudnessBin::default(); LOUDNESS_BINS]; METER_SLOTS],
            loudness_lens: [0; METER_SLOTS],
            next_trail_frame: 0,
            key: None,
            tracking: TrackingSummary::default(),
        }
    }
}

impl SceneViewFrame {
    pub const fn lfe(&self) -> Option<LfeView> {
        self.lfe
    }

    pub const fn tracking(&self) -> TrackingSummary {
        self.tracking
    }

    /// Keep stored trails in their declared reference frame. Rendering the
    /// room uses the current forward head rotation for head-relative objects.
    pub fn in_world_space(mut self, pose: crate::head_tracking::Quaternion) -> Self {
        for slot in 0..self.total_objects.min(MAX_VIEW_OBJECTS) {
            if self.objects[slot].tracking.head_locked() {
                self.objects[slot].position = pose
                    .conjugate()
                    .rotate_listener(self.objects[slot].position);
                for point in &mut self.trails[slot][..self.trail_lens[slot]] {
                    *point = pose.conjugate().rotate_listener(*point);
                }
            }
        }
        self
    }

    /// The objects that fit in the budget.
    #[must_use]
    pub fn objects(&self) -> &[ObjectView] {
        &self.objects[..self.total_objects.min(MAX_VIEW_OBJECTS)]
    }

    /// Where the object in `slot` has been, oldest first.
    ///
    /// Empty until the first breadcrumb is due, and emptied whenever the slot
    /// changes hands — a trail belongs to an element, not to an array index.
    #[must_use]
    pub fn trail(&self, slot: usize) -> &[[f32; 3]] {
        match self.trail_lens.get(slot) {
            Some(&len) => &self.trails[slot][..len],
            None => &[],
        }
    }

    /// How loud `slot` was at each of its breadcrumbs, aligned with
    /// [`Self::trail`]. Linear level, as the meter reads it.
    #[must_use]
    pub fn trail_loudness(&self, slot: usize) -> &[f32] {
        match self.trail_lens.get(slot) {
            Some(&len) => &self.trail_loudness[slot][..len],
            None => &[],
        }
    }

    /// Which of `slot`'s breadcrumbs the object arrived at instantly, aligned
    /// with [`Self::trail`]. A set flag means the object did not travel from the
    /// previous mark — it was somewhere else and then it was here.
    #[must_use]
    pub fn trail_jumps(&self, slot: usize) -> &[bool] {
        match self.trail_lens.get(slot) {
            Some(&len) => &self.trail_jumps[slot][..len],
            None => &[],
        }
    }

    /// Objects the scene carried beyond [`MAX_VIEW_OBJECTS`]. Non-zero means the
    /// view is showing an incomplete scene and has to say so.
    #[must_use]
    pub const fn hidden_objects(&self) -> usize {
        self.total_objects.saturating_sub(MAX_VIEW_OBJECTS)
    }

    /// What `slot` measured recently, oldest bin first, the last still filling.
    ///
    /// Empty until the first publication, and emptied whenever the slot changes
    /// hands — a measurement belongs to an element, not to an array index.
    #[must_use]
    pub fn loudness_bins(&self, slot: usize) -> &[LoudnessBin] {
        match self.loudness_lens.get(slot) {
            Some(&len) => &self.loudness[slot][..len],
            None => &[],
        }
    }

    /// Mean square of `slot`'s newest `window_frames` of audio, and the frames
    /// that mean actually covers.
    ///
    /// Readers sum backwards by *frames* rather than by bins because a bin is
    /// closed by how much audio it holds, not by a clock: a consumer that
    /// publishes rarely lands a longer-than-nominal bin, and counting bins
    /// would then silently measure a different amount of audio. The returned
    /// frame count is short of `window_frames` only while the ring is still
    /// filling, which is how a caller tells "quiet" from "not yet measured".
    #[must_use]
    pub fn mean_square(&self, slot: usize, window_frames: u32) -> (f64, u32) {
        let mut sum = 0.0_f64;
        let mut frames = 0_u32;
        for bin in self.loudness_bins(slot).iter().rev() {
            if frames >= window_frames {
                break;
            }
            sum += bin.sum_squares;
            frames = frames.saturating_add(bin.frames);
        }
        if frames == 0 {
            return (0.0, 0);
        }
        (sum / f64::from(frames), frames)
    }

    /// Largest absolute sample `slot` produced over the whole ring, gain
    /// included and K-weighting deliberately *not*.
    ///
    /// This is the one reading that is not a loudness: weighting is a model of
    /// hearing, and a converter does not clip according to a model of hearing.
    /// It answers whether the renderer was handed a sample it cannot carry,
    /// which is a different question from how loud the object sounded, and the
    /// meter bank reports the two side by side for exactly that reason.
    #[must_use]
    pub fn sample_peak(&self, slot: usize) -> f32 {
        self.loudness_bins(slot)
            .iter()
            .fold(0.0_f32, |peak, bin| peak.max(bin.peak))
    }
}

/// Breadcrumb spacing in presentation frames, at least one so the cadence can
/// never stall on a pathological sample rate.
pub fn sample_interval_frames(sample_rate: u32) -> i64 {
    let frames = u64::from(sample_rate) * u64::from(TRAIL_INTERVAL_MILLISECONDS) / 1000;
    i64::try_from(frames).unwrap_or(i64::MAX).max(1)
}

/// Fold one publication's energy into a slot's newest bin, opening a new one
/// first when the current bin already holds its share of audio.
///
/// A publication longer than a bin lands whole in one bin rather than being
/// spread across several: the consumer hands over one number for the window, so
/// splitting it would have to invent a distribution. The mean stays exact
/// either way, because every bin carries the frame count its sum covers; what
/// coarsens is only the time resolution, and only on a path that publishes
/// more slowly than the grid — which is the silent preview, where nothing is
/// being heard anyway.
fn push_energy(
    bins: &mut [LoudnessBin; LOUDNESS_BINS],
    len: &mut usize,
    energy: ObjectEnergy,
    bin_frames: u32,
) {
    let open = match len.checked_sub(1) {
        Some(index) if bins[index].frames < bin_frames => index,
        _ => {
            if *len < LOUDNESS_BINS {
                *len += 1;
            } else {
                bins.copy_within(1.., 0);
            }
            let index = *len - 1;
            bins[index] = LoudnessBin::default();
            index
        }
    };
    let bin = &mut bins[open];
    bin.sum_squares += energy.sum_squares;
    bin.frames = bin.frames.saturating_add(energy.frames);
    bin.peak = bin.peak.max(energy.peak);
}

#[derive(Debug, Clone, Copy)]
struct Breadcrumb {
    point: [f32; 3],
    jumped: bool,
    loudness: f32,
}

fn push_trail(
    trail: &mut [[f32; 3]; TRAIL_SAMPLES],
    jumps: &mut [bool; TRAIL_SAMPLES],
    loudness: &mut [f32; TRAIL_SAMPLES],
    len: &mut usize,
    mark: Breadcrumb,
) {
    if let Some(slot) = trail.get_mut(*len) {
        *slot = mark.point;
        jumps[*len] = mark.jumped;
        loudness[*len] = mark.loudness;
        *len = len.saturating_add(1);
        return;
    }
    trail.copy_within(1.., 0);
    jumps.copy_within(1.., 0);
    loudness.copy_within(1.., 0);
    trail[TRAIL_SAMPLES - 1] = mark.point;
    jumps[TRAIL_SAMPLES - 1] = mark.jumped;
    loudness[TRAIL_SAMPLES - 1] = mark.loudness;
}

/// The shared handle. `SpatialOutputController` owns it, the render source
/// writes it, the UI reads it.
#[derive(Debug, Default)]
pub struct SceneViewMirror {
    frame: Mutex<SceneViewFrame>,
}

impl SceneViewMirror {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish the objects one render quantum resolved. Audio thread only.
    ///
    /// **An update carrying neither objects nor LFE is discarded.** A
    /// quantum resolves no element state whenever it produced only a forward
    /// timeline gap or ran dry, which happens routinely; storing those as an
    /// empty scene would make every object blink out and back roughly once a
    /// buffer. Holding the last known positions is both steadier and more
    /// truthful — there genuinely is no newer position to show.
    ///
    /// Stale positions cannot leak across a seek or a source change this way,
    /// because a held frame keeps the key it was written under and `read`
    /// rejects it.
    #[cfg_attr(
        not(feature = "decode"),
        allow(
            dead_code,
            reason = "object positions come from the render callback or the \
                      scene preview, and neither exists without a decoder"
        )
    )]
    pub fn write_with_lfe<I>(
        &self,
        key: PlaybackKey,
        objects: I,
        lfe: Option<LfeView>,
        timeline_frame: i64,
        sample_rate: u32,
    ) where
        I: IntoIterator<Item = ObjectView>,
    {
        // Staged off-lock so the critical section stays short.
        let mut staged = [ObjectView::default(); MAX_VIEW_OBJECTS];
        let mut total = 0usize;
        let mut tracking = TrackingSummary::default();
        for object in objects {
            tracking.observe(object.element_id, object.tracking);
            if let Some(slot) = staged.get_mut(total) {
                *slot = object;
            }
            total = total.saturating_add(1);
        }
        if total == 0 && lfe.is_none() {
            return;
        }

        let Ok(mut guard) = self.frame.try_lock() else {
            return;
        };
        // Reborrowed once so the field borrows below are disjoint; through the
        // guard's Deref they would all be borrows of the whole frame.
        let frame = &mut *guard;

        // A seek, a new source or a device recovery starts a different history.
        // Carrying breadcrumbs across one would draw a line the object never
        // travelled, straight from where it used to be to where it now is.
        if frame.key != Some(key) {
            frame.trail_lens = [0; MAX_VIEW_OBJECTS];
            frame.pending_jump = [false; MAX_VIEW_OBJECTS];
            frame.loudness_lens = [0; METER_SLOTS];
            frame.next_trail_frame = timeline_frame;
        }
        // A trail belongs to an element, not to an array index. If the scene's
        // element set changed, a slot can now hold a different object and its
        // history is not that object's.
        for (slot, previous) in frame.objects.iter().enumerate() {
            if previous.element_id != staged[slot].element_id
                || previous.tracking.head_locked() != staged[slot].tracking.head_locked()
            {
                frame.trail_lens[slot] = 0;
                frame.pending_jump[slot] = false;
                frame.loudness_lens[slot] = 0;
            }
        }

        if frame.lfe.map(|view| view.element_id) != lfe.map(|view| view.element_id) {
            frame.loudness_lens[LFE_METER_SLOT] = 0;
        }
        frame.lfe = lfe;
        frame.objects = staged;
        frame.total_objects = total;
        frame.key = Some(key);
        frame.tracking = tracking;

        // Latched rather than consumed here: the discontinuity belongs to a
        // sampled point, and the quantum that carries it is usually not the one
        // a breadcrumb falls on.
        let bin_frames = loudness_bin_frames(sample_rate);
        if let Some(lfe) = lfe {
            push_energy(
                &mut frame.loudness[LFE_METER_SLOT],
                &mut frame.loudness_lens[LFE_METER_SLOT],
                lfe.energy,
                bin_frames,
            );
        }
        for slot in 0..total.min(MAX_VIEW_OBJECTS) {
            frame.pending_jump[slot] |= frame.objects[slot].jumped;
            push_energy(
                &mut frame.loudness[slot],
                &mut frame.loudness_lens[slot],
                frame.objects[slot].energy,
                bin_frames,
            );
        }

        if timeline_frame >= frame.next_trail_frame {
            frame.next_trail_frame =
                timeline_frame.saturating_add(sample_interval_frames(sample_rate));
            for slot in 0..total.min(MAX_VIEW_OBJECTS) {
                let point = frame.objects[slot].position;
                let jumped = std::mem::take(&mut frame.pending_jump[slot]);
                // Read before the borrow below: this is what the object
                // measured around the instant the mark is being taken.
                let (mean_square, measured) = frame.mean_square(slot, bin_frames);
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "a breadcrumb's level is a display value, well inside f32"
                )]
                let loudness = if measured == 0 {
                    0.0
                } else {
                    mean_square.sqrt() as f32
                };
                push_trail(
                    &mut frame.trails[slot],
                    &mut frame.trail_jumps[slot],
                    &mut frame.trail_loudness[slot],
                    &mut frame.trail_lens[slot],
                    Breadcrumb {
                        point,
                        jumped,
                        loudness,
                    },
                );
            }
        }
    }

    /// Take a copy of the current frame if it belongs to `key`. UI thread.
    ///
    /// Returns `None` for a frame from a superseded playback, and for a mirror
    /// nothing has written yet — on a platform without the Windows output that
    /// is every call, which is what leaves the stage an empty room.
    #[must_use]
    pub fn read(&self, key: PlaybackKey) -> Option<SceneViewFrame> {
        let frame = *self.frame.lock().unwrap_or_else(PoisonError::into_inner);
        (frame.key == Some(key)).then_some(frame)
    }

    #[cfg(test)]
    pub fn write<I>(&self, key: PlaybackKey, objects: I, timeline_frame: i64, sample_rate: u32)
    where
        I: IntoIterator<Item = ObjectView>,
    {
        self.write_with_lfe(key, objects, None, timeline_frame, sample_rate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lfe_meter_is_independent_of_dynamic_object_capacity_and_epochs() {
        let mirror = SceneViewMirror::new();
        let key = PlaybackKey::new(1, 0);
        let lfe = LfeView {
            element_id: 99,
            active: true,
            gain: 0.5,
            energy: ObjectEnergy {
                sum_squares: 30.0,
                frames: 480,
                peak: 0.25,
            },
        };
        mirror.write_with_lfe(key, std::iter::empty(), Some(lfe), 480, 48_000);
        let frame = mirror.read(key).unwrap();
        assert!(frame.objects().is_empty());
        assert_eq!(frame.lfe(), Some(lfe));
        assert!((frame.mean_square(LFE_METER_SLOT, 480).0 - 0.0625).abs() < 1e-9);
        mirror.write_with_lfe(
            key,
            (0..=MAX_VIEW_OBJECTS).map(|id| object(id as u64 + 1, 0.0)),
            Some(lfe),
            960,
            48_000,
        );
        let frame = mirror.read(key).unwrap();
        assert_eq!(frame.objects().len(), MAX_VIEW_OBJECTS);
        assert_eq!(frame.hidden_objects(), 1);
        assert_eq!(frame.lfe(), Some(lfe));
        let next = PlaybackKey::new(1, 1);
        let silent = LfeView {
            energy: ObjectEnergy {
                frames: 480,
                ..Default::default()
            },
            ..lfe
        };
        mirror.write_with_lfe(next, std::iter::empty(), Some(silent), 480, 48_000);
        assert!(mirror.read(key).is_none());
        let frame = mirror.read(next).unwrap();
        assert_eq!(frame.mean_square(LFE_METER_SLOT, 480), (0.0, 480));
        assert!(frame.sample_peak(LFE_METER_SLOT).abs() < f32::EPSILON);
        mirror.write_with_lfe(next, [object(1, 0.0)], None, 960, 48_000);
        assert!(mirror.read(next).unwrap().lfe().is_none());
    }

    #[test]
    fn reference_frames_rotate_only_head_locked_objects_and_reset_their_trails() {
        let mirror = SceneViewMirror::new();
        let key = PlaybackKey::new(1, 1);
        let head = ObjectView {
            position: [0.0, 0.0, -1.0],
            tracking: ContentHeadTracking::HeadRelative,
            ..object(7, 0.0)
        };
        let world = ObjectView {
            element_id: 9,
            tracking: ContentHeadTracking::SceneRelative,
            ..head
        };
        mirror.write(key, [head, world], 0, 48_000);
        let raw = mirror.read(key).unwrap();
        let displayed = raw.in_world_space(crate::head_tracking::Quaternion::from_euler([
            90.0, 0.0, 0.0,
        ]));
        assert!((displayed.objects()[0].position[0] + 1.0).abs() < 0.0001);
        assert_eq!(
            displayed.objects()[1].position.map(f32::to_bits),
            world.position.map(f32::to_bits)
        );
        assert_eq!(
            displayed.trail(0)[0].map(f32::to_bits),
            displayed.objects()[0].position.map(f32::to_bits)
        );
        assert_eq!(
            raw.objects()[0].position.map(f32::to_bits),
            head.position.map(f32::to_bits)
        );
        mirror.write(
            key,
            [
                ObjectView {
                    tracking: ContentHeadTracking::SceneRelative,
                    ..head
                },
                world,
            ],
            1,
            48_000,
        );
        let switched = mirror.read(key).unwrap();
        assert!(switched.trail(0).is_empty());
        assert_eq!(switched.trail(1).len(), 1);
        assert_eq!(switched.tracking().scene_relative, 2);
        assert_eq!(switched.tracking().head_relative, 0);
    }

    #[test]
    fn diagnostics_include_objects_beyond_the_visual_budget() {
        let mirror = SceneViewMirror::new();
        let key = PlaybackKey::new(1, 1);
        let issue = crate::decoder::TrackingIssue::ConflictingGroups;
        mirror.write(
            key,
            (0..=MAX_VIEW_OBJECTS).map(|index| ObjectView {
                tracking: if index == MAX_VIEW_OBJECTS {
                    ContentHeadTracking::Unsupported(issue)
                } else {
                    ContentHeadTracking::Unspecified
                },
                ..object(u64::try_from(index).unwrap(), 0.0)
            }),
            0,
            48_000,
        );
        let frame = mirror.read(key).unwrap();
        assert_eq!(frame.hidden_objects(), 1);
        assert_eq!(frame.tracking().unspecified, MAX_VIEW_OBJECTS);
        assert_eq!(frame.tracking().unsupported, 1);
        assert_eq!(
            frame.tracking().first_issue,
            Some((u64::try_from(MAX_VIEW_OBJECTS).unwrap(), issue))
        );
    }
    #[test]
    fn energy_bins_close_on_a_bin_of_audio_and_the_mean_covers_the_window_asked_for() {
        let mirror = SceneViewMirror::new();
        let key = PlaybackKey::new(1, 1);
        let bin = loudness_bin_frames(48_000);
        // Three publications of exactly one bin each: the ring should hold
        // three bins rather than one fat one or three partial ones.
        for step in 0..3 {
            mirror.write(
                key,
                [energetic(1.0, bin)],
                i64::from(step) * i64::from(bin),
                48_000,
            );
        }
        let frame = mirror.read(key).unwrap();
        assert_eq!(frame.loudness_bins(0).len(), 3);

        // A window of one bin sees one bin's frames; a window larger than the
        // ring sees everything the ring holds and says so.
        let (mean, frames) = frame.mean_square(0, bin);
        assert_eq!(frames, bin);
        assert!((mean - 1.0).abs() < 1e-12, "mean square was {mean}");
        let (_, all) = frame.mean_square(0, bin * 10);
        assert_eq!(all, bin * 3);
    }

    #[test]
    fn the_sample_peak_is_the_loudest_unweighted_sample_left_in_the_ring() {
        let mirror = SceneViewMirror::new();
        let key = PlaybackKey::new(1, 1);
        let bin = loudness_bin_frames(48_000);
        for (step, peak) in [0.2_f32, 0.9, 0.4].into_iter().enumerate() {
            let clipping = ObjectView {
                energy: ObjectEnergy {
                    sum_squares: 0.0,
                    frames: bin,
                    peak,
                },
                ..object(7, 0.0)
            };
            let at = i64::try_from(step).unwrap() * i64::from(bin);
            mirror.write(key, [clipping], at, 48_000);
        }
        let frame = mirror.read(key).unwrap();
        // Quieter bins following it do not erase it: a clip is something that
        // happened, and the reading has to survive long enough to be seen.
        assert!((frame.sample_peak(0) - 0.9).abs() < f32::EPSILON);
        // A slot that never measured anything has no peak rather than a quiet
        // one, which is the same distinction `mean_square` draws with frames.
        assert!(frame.sample_peak(5).abs() < f32::EPSILON);
    }

    #[test]
    fn a_publication_shorter_than_a_bin_keeps_filling_the_same_bin() {
        let mirror = SceneViewMirror::new();
        let key = PlaybackKey::new(1, 1);
        let bin = loudness_bin_frames(48_000);
        // MacinRender publishes far more often than the grid; those updates
        // have to accumulate rather than each claiming a bin of their own.
        for step in 0..4 {
            mirror.write(key, [energetic(1.0, bin / 4)], i64::from(step), 48_000);
        }
        let frame = mirror.read(key).unwrap();
        assert_eq!(frame.loudness_bins(0).len(), 1);
        assert_eq!(frame.loudness_bins(0)[0].frames, (bin / 4) * 4);
    }

    #[test]
    fn a_superseded_playback_and_a_reused_slot_both_start_the_measurement_over() {
        let mirror = SceneViewMirror::new();
        let first = PlaybackKey::new(1, 1);
        let bin = loudness_bin_frames(48_000);
        mirror.write(first, [energetic(1.0, bin)], 0, 48_000);
        assert_eq!(mirror.read(first).unwrap().loudness_bins(0).len(), 1);

        // A seek: the measurement belongs to the playback that produced it.
        let second = PlaybackKey::new(1, 2);
        mirror.write(second, [energetic(0.0, bin)], 0, 48_000);
        let frame = mirror.read(second).unwrap();
        assert_eq!(frame.loudness_bins(0).len(), 1);
        assert_eq!(
            frame.loudness_bins(0)[0].sum_squares.to_bits(),
            0.0_f64.to_bits()
        );

        // The slot changing hands: a measurement belongs to an element, not to
        // an array index.
        mirror.write(second, [energetic(1.0, bin)], i64::from(bin), 48_000);
        assert_eq!(mirror.read(second).unwrap().loudness_bins(0).len(), 2);
        let moved = ObjectView {
            element_id: 99,
            ..energetic(1.0, bin)
        };
        mirror.write(second, [moved], i64::from(bin) * 2, 48_000);
        assert_eq!(mirror.read(second).unwrap().loudness_bins(0).len(), 1);
    }

    #[test]
    fn the_ring_is_bounded_and_drops_its_oldest_measurement() {
        let mirror = SceneViewMirror::new();
        let key = PlaybackKey::new(1, 1);
        let bin = loudness_bin_frames(48_000);
        for step in 0..(LOUDNESS_BINS + 5) {
            let step = i64::try_from(step).unwrap();
            mirror.write(key, [energetic(1.0, bin)], step * i64::from(bin), 48_000);
        }
        let frame = mirror.read(key).unwrap();
        assert_eq!(frame.loudness_bins(0).len(), LOUDNESS_BINS);
    }

    #[test]
    fn a_breadcrumb_records_the_level_that_was_true_when_it_was_taken() {
        let mirror = SceneViewMirror::new();
        let key = PlaybackKey::new(1, 1);
        let bin = loudness_bin_frames(48_000);
        let interval = sample_interval_frames(48_000);

        // Loud while the first mark is taken, silent by the second. The trail
        // has to keep both readings rather than repainting the first with the
        // second — that is the whole reason the reading is stored per mark.
        mirror.write(key, [energetic(0.25, bin)], 0, 48_000);
        mirror.write(key, [energetic(0.0, bin)], interval, 48_000);
        let frame = mirror.read(key).unwrap();

        assert_eq!(frame.trail(0).len(), 2);
        let levels = frame.trail_loudness(0);
        assert!(
            (levels[0] - 0.5).abs() < 1e-6,
            "first breadcrumb recorded {}",
            levels[0]
        );
        assert!(
            levels[1] < levels[0],
            "second breadcrumb recorded {} against the first's {}",
            levels[1],
            levels[0]
        );
    }

    fn energetic(mean_square: f64, frames: u32) -> ObjectView {
        ObjectView {
            energy: ObjectEnergy {
                sum_squares: mean_square * f64::from(frames),
                frames,
                peak: 0.0,
            },
            ..object(7, 0.0)
        }
    }

    fn object(element_id: u64, x: f32) -> ObjectView {
        ObjectView {
            element_id,
            active: true,
            position: [x, 0.0, 0.0],
            gain: 1.0,
            tracking: ContentHeadTracking::Unspecified,
            jumped: false,
            energy: ObjectEnergy::default(),
        }
    }

    /// An object whose quantum carried an instant metadata update.
    fn jumping(element_id: u64, x: f32) -> ObjectView {
        ObjectView {
            jumped: true,
            ..object(element_id, x)
        }
    }

    fn budget() -> u64 {
        u64::try_from(MAX_VIEW_OBJECTS).expect("the object budget fits a u64")
    }

    const RATE: u32 = 48_000;

    /// Publish one quantum's worth of objects at a presentation position.
    fn write_at(mirror: &SceneViewMirror, key: PlaybackKey, frame: i64, objects: &[ObjectView]) {
        mirror.write(key, objects.iter().copied(), frame, RATE);
    }

    #[test]
    fn an_unwritten_mirror_reads_empty_so_the_stage_draws_an_empty_room() {
        // This is the permanent state off Windows, where nothing writes it.
        let mirror = SceneViewMirror::new();
        assert!(mirror.read(PlaybackKey::new(0, 0)).is_none());
    }

    #[test]
    fn a_frame_from_a_superseded_playback_is_not_returned() {
        let mirror = SceneViewMirror::new();
        write_at(&mirror, PlaybackKey::new(3, 1), 0, &[object(1, 0.5)]);

        assert!(mirror.read(PlaybackKey::new(3, 1)).is_some());
        assert!(
            mirror.read(PlaybackKey::new(3, 2)).is_none(),
            "a seek must invalidate the positions it superseded"
        );
        assert!(
            mirror.read(PlaybackKey::new(4, 1)).is_none(),
            "a new source must invalidate the previous one's positions"
        );
    }

    #[test]
    fn a_quantum_that_resolved_no_objects_holds_the_last_known_positions() {
        // A forward timeline gap or a dry FIFO resolves no element state at
        // all, and both happen routinely. Storing them as an empty scene would
        // blink every object out and back roughly once a buffer.
        let mirror = SceneViewMirror::new();
        let key = PlaybackKey::new(1, 0);
        write_at(&mirror, key, 0, &[object(7, 0.25)]);
        write_at(&mirror, key, 1, &[]);

        let frame = mirror.read(key).expect("the previous frame is held");
        assert_eq!(frame.objects().len(), 1);
        assert_eq!(frame.objects()[0].element_id, 7);
        assert!((frame.objects()[0].position[0] - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn holding_an_empty_quantum_still_cannot_leak_across_a_seek() {
        // The held frame keeps the key it was written under, so the gate in
        // `read` catches it without the writer needing a separate clear path.
        let mirror = SceneViewMirror::new();
        write_at(&mirror, PlaybackKey::new(1, 0), 0, &[object(7, 0.25)]);
        write_at(&mirror, PlaybackKey::new(1, 1), 0, &[]);

        assert!(mirror.read(PlaybackKey::new(1, 1)).is_none());
    }

    #[test]
    fn breadcrumbs_are_taken_on_the_interval_not_on_every_quantum() {
        // Fixed spacing in time is the whole point: it makes the gap between
        // marks a speed reading. Sampling per quantum would make it a frame-rate
        // reading instead.
        let mirror = SceneViewMirror::new();
        let key = PlaybackKey::new(1, 0);
        let interval = sample_interval_frames(RATE);

        write_at(&mirror, key, 0, &[object(1, 0.0)]);
        assert_eq!(mirror.read(key).expect("frame").trail(0).len(), 1);

        write_at(&mirror, key, interval / 2, &[object(1, 0.1)]);
        assert_eq!(
            mirror.read(key).expect("frame").trail(0).len(),
            1,
            "a quantum inside the interval must not add a mark"
        );

        write_at(&mirror, key, interval, &[object(1, 0.2)]);
        let frame = mirror.read(key).expect("frame");
        assert_eq!(frame.trail(0).len(), 2);
        assert!((frame.trail(0)[1][0] - 0.2).abs() < f32::EPSILON);
    }

    #[test]
    fn a_seek_discards_the_trail_rather_than_drawing_a_path_never_travelled() {
        let mirror = SceneViewMirror::new();
        let interval = sample_interval_frames(RATE);
        let before = PlaybackKey::new(1, 0);
        write_at(&mirror, before, 0, &[object(1, -0.9)]);
        write_at(&mirror, before, interval, &[object(1, -0.8)]);
        assert_eq!(mirror.read(before).expect("frame").trail(0).len(), 2);

        let after = PlaybackKey::new(1, 1);
        write_at(&mirror, after, 500_000, &[object(1, 0.9)]);
        let frame = mirror.read(after).expect("frame");
        assert_eq!(
            frame.trail(0).len(),
            1,
            "breadcrumbs survived a seek and would join two unrelated positions"
        );
        assert!((frame.trail(0)[0][0] - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn a_slot_that_changes_element_does_not_inherit_the_previous_trail() {
        // Slots are array indices; trails belong to elements. A scene whose
        // element set changes can hand a slot to a different object.
        let mirror = SceneViewMirror::new();
        let key = PlaybackKey::new(1, 0);
        let interval = sample_interval_frames(RATE);
        write_at(&mirror, key, 0, &[object(11, -0.5)]);
        write_at(&mirror, key, interval, &[object(11, -0.4)]);
        assert_eq!(mirror.read(key).expect("frame").trail(0).len(), 2);

        write_at(&mirror, key, interval * 2, &[object(22, 0.6)]);
        let frame = mirror.read(key).expect("frame");
        assert_eq!(frame.trail(0).len(), 1);
        assert!((frame.trail(0)[0][0] - 0.6).abs() < f32::EPSILON);
    }

    #[test]
    fn a_full_trail_drops_its_oldest_mark_and_stays_oldest_first() {
        let mirror = SceneViewMirror::new();
        let key = PlaybackKey::new(1, 0);
        let interval = sample_interval_frames(RATE);
        let steps = TRAIL_SAMPLES + 5;
        for step in 0..steps {
            let position = f32::from(u16::try_from(step).expect("step fits a u16"));
            write_at(
                &mirror,
                key,
                interval * i64::try_from(step).expect("step fits an i64"),
                &[object(1, position)],
            );
        }

        let frame = mirror.read(key).expect("frame");
        let trail = frame.trail(0);
        assert_eq!(trail.len(), TRAIL_SAMPLES);
        let oldest = f32::from(u16::try_from(steps - TRAIL_SAMPLES).expect("fits"));
        let newest = f32::from(u16::try_from(steps - 1).expect("fits"));
        assert!(
            (trail[0][0] - oldest).abs() < f32::EPSILON,
            "{:?}",
            trail[0]
        );
        assert!(
            (trail[TRAIL_SAMPLES - 1][0] - newest).abs() < f32::EPSILON,
            "the newest mark is not at the end"
        );
    }

    #[test]
    fn a_jump_seen_between_breadcrumbs_lands_on_the_next_one() {
        // Quanta are roughly a quarter of the sampling interval, so the update
        // that jumped is usually not the one a breadcrumb falls on. Dropping it
        // on the floor would lose most jumps; attaching it to every subsequent
        // mark would claim the object kept teleporting.
        let mirror = SceneViewMirror::new();
        let key = PlaybackKey::new(1, 0);
        let interval = sample_interval_frames(RATE);

        write_at(&mirror, key, 0, &[object(1, 0.0)]);
        write_at(&mirror, key, interval / 2, &[jumping(1, 0.5)]);
        write_at(&mirror, key, interval, &[object(1, 0.9)]);
        write_at(&mirror, key, interval * 2, &[object(1, 0.9)]);

        let frame = mirror.read(key).expect("frame");
        assert_eq!(frame.trail_jumps(0), [false, true, false]);
    }

    #[test]
    fn a_seek_discards_a_latched_jump_along_with_the_trail() {
        let mirror = SceneViewMirror::new();
        let interval = sample_interval_frames(RATE);
        let before = PlaybackKey::new(1, 0);
        write_at(&mirror, before, 0, &[object(1, 0.0)]);
        write_at(&mirror, before, interval / 2, &[jumping(1, 0.5)]);

        let after = PlaybackKey::new(1, 1);
        write_at(&mirror, after, 900_000, &[object(1, -0.5)]);
        let frame = mirror.read(after).expect("frame");
        assert_eq!(
            frame.trail_jumps(0),
            [false],
            "a jump from the playback before the seek was attributed to this one"
        );
    }

    #[test]
    fn a_scene_past_the_budget_is_truncated_and_reports_what_it_hid() {
        let mirror = SceneViewMirror::new();
        let key = PlaybackKey::new(1, 0);
        let objects: Vec<ObjectView> = (0..budget() + 3).map(|id| object(id, 0.0)).collect();
        write_at(&mirror, key, 0, &objects);

        let frame = mirror.read(key).expect("frame");
        assert_eq!(frame.objects().len(), MAX_VIEW_OBJECTS);
        assert_eq!(frame.hidden_objects(), 3);
        assert_eq!(
            frame.objects()[MAX_VIEW_OBJECTS - 1].element_id,
            budget() - 1,
            "truncation kept the wrong end of the scene"
        );
    }
}
