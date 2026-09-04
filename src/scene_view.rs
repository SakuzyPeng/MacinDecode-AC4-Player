//! The audio thread's view of the object scene, mirrored for the UI.
//!
//! The Windows render callback is the only place that knows where every object
//! actually is at a given instant, and it runs on the WASAPI event thread. This
//! module is the one-way channel from there to the frame that draws it.
//!
//! Three rules keep it out of the audio thread's way, and all three are load
//! bearing rather than stylistic:
//!
//! * **The writer never blocks.** [`SceneViewMirror::write`] takes the lock with
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

use crate::decoder::PlaybackKey;
use crate::scene3d::params::{TRAIL_INTERVAL_MILLISECONDS, TRAIL_SAMPLES};

/// Objects the view will draw. The design budget is 20 dynamic objects plus the
/// static LFE slot; a scene beyond it is truncated and reported, never grown,
/// because growing it would move the allocation onto the audio thread.
pub const MAX_VIEW_OBJECTS: usize = 20;

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
    /// Whether an instant (`ramp_frames == 0`) metadata update landed for this
    /// element inside the quantum being published. The mirror latches it until
    /// the next breadcrumb, because quanta are far shorter than the sampling
    /// interval and the flag belongs to a sampled point, not to a quantum.
    pub jumped: bool,
}

/// One instant of the scene, as the audio thread last saw it, plus the recent
/// history of where each object has been.
#[derive(Debug, Clone, Copy)]
pub struct SceneViewFrame {
    objects: [ObjectView; MAX_VIEW_OBJECTS],
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
    /// A discontinuity seen since the last breadcrumb was taken. Latched here
    /// because a quantum is roughly a quarter of the sampling interval, so the
    /// update that jumped is usually not the one being sampled.
    pending_jump: [bool; MAX_VIEW_OBJECTS],
    /// Presentation frame at which the next breadcrumb is due.
    next_trail_frame: i64,
    /// `None` until the first write. Distinguishes "no playback yet" from a
    /// playback that legitimately carries no objects.
    key: Option<PlaybackKey>,
}

impl Default for SceneViewFrame {
    fn default() -> Self {
        Self {
            objects: [ObjectView::default(); MAX_VIEW_OBJECTS],
            total_objects: 0,
            trails: [[[0.0; 3]; TRAIL_SAMPLES]; MAX_VIEW_OBJECTS],
            trail_lens: [0; MAX_VIEW_OBJECTS],
            trail_jumps: [[false; TRAIL_SAMPLES]; MAX_VIEW_OBJECTS],
            pending_jump: [false; MAX_VIEW_OBJECTS],
            next_trail_frame: 0,
            key: None,
        }
    }
}

impl SceneViewFrame {
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
}

/// Breadcrumb spacing in presentation frames, at least one so the cadence can
/// never stall on a pathological sample rate.
pub fn sample_interval_frames(sample_rate: u32) -> i64 {
    let frames = u64::from(sample_rate) * u64::from(TRAIL_INTERVAL_MILLISECONDS) / 1000;
    i64::try_from(frames).unwrap_or(i64::MAX).max(1)
}

fn push_trail(
    trail: &mut [[f32; 3]; TRAIL_SAMPLES],
    jumps: &mut [bool; TRAIL_SAMPLES],
    len: &mut usize,
    point: [f32; 3],
    jumped: bool,
) {
    if let Some(slot) = trail.get_mut(*len) {
        *slot = point;
        jumps[*len] = jumped;
        *len = len.saturating_add(1);
        return;
    }
    trail.copy_within(1.., 0);
    jumps.copy_within(1.., 0);
    trail[TRAIL_SAMPLES - 1] = point;
    jumps[TRAIL_SAMPLES - 1] = jumped;
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
    /// **An update carrying no objects is discarded rather than stored.** A
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
    pub fn write<I>(&self, key: PlaybackKey, objects: I, timeline_frame: i64, sample_rate: u32)
    where
        I: IntoIterator<Item = ObjectView>,
    {
        // Staged off-lock so the critical section stays short.
        let mut staged = [ObjectView::default(); MAX_VIEW_OBJECTS];
        let mut total = 0usize;
        for object in objects {
            if let Some(slot) = staged.get_mut(total) {
                *slot = object;
            }
            total = total.saturating_add(1);
        }
        if total == 0 {
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
            frame.next_trail_frame = timeline_frame;
        }
        // A trail belongs to an element, not to an array index. If the scene's
        // element set changed, a slot can now hold a different object and its
        // history is not that object's.
        for (slot, previous) in frame.objects.iter().enumerate() {
            if previous.element_id != staged[slot].element_id {
                frame.trail_lens[slot] = 0;
                frame.pending_jump[slot] = false;
            }
        }

        frame.objects = staged;
        frame.total_objects = total;
        frame.key = Some(key);

        // Latched rather than consumed here: the discontinuity belongs to a
        // sampled point, and the quantum that carries it is usually not the one
        // a breadcrumb falls on.
        for slot in 0..total.min(MAX_VIEW_OBJECTS) {
            frame.pending_jump[slot] |= frame.objects[slot].jumped;
        }

        if timeline_frame >= frame.next_trail_frame {
            frame.next_trail_frame =
                timeline_frame.saturating_add(sample_interval_frames(sample_rate));
            for slot in 0..total.min(MAX_VIEW_OBJECTS) {
                let point = frame.objects[slot].position;
                let jumped = std::mem::take(&mut frame.pending_jump[slot]);
                push_trail(
                    &mut frame.trails[slot],
                    &mut frame.trail_jumps[slot],
                    &mut frame.trail_lens[slot],
                    point,
                    jumped,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(element_id: u64, x: f32) -> ObjectView {
        ObjectView {
            element_id,
            active: true,
            position: [x, 0.0, 0.0],
            gain: 1.0,
            jumped: false,
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
