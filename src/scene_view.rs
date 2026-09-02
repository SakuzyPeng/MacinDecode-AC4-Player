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
}

/// One instant of the scene, as the audio thread last saw it.
#[derive(Debug, Clone, Copy, Default)]
pub struct SceneViewFrame {
    objects: [ObjectView; MAX_VIEW_OBJECTS],
    /// Objects the render callback resolved, which may exceed the array.
    total_objects: usize,
    /// `None` until the first write. Distinguishes "no playback yet" from a
    /// playback that legitimately carries no objects.
    key: Option<PlaybackKey>,
}

impl SceneViewFrame {
    /// The objects that fit in the budget.
    #[must_use]
    pub fn objects(&self) -> &[ObjectView] {
        &self.objects[..self.total_objects.min(MAX_VIEW_OBJECTS)]
    }

    /// Objects the scene carried beyond [`MAX_VIEW_OBJECTS`]. Non-zero means the
    /// view is showing an incomplete scene and has to say so.
    #[must_use]
    pub const fn hidden_objects(&self) -> usize {
        self.total_objects.saturating_sub(MAX_VIEW_OBJECTS)
    }
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
        not(target_os = "windows"),
        allow(
            dead_code,
            reason = "only the Windows render callback produces object positions"
        )
    )]
    pub fn write<I>(&self, key: PlaybackKey, objects: I)
    where
        I: IntoIterator<Item = ObjectView>,
    {
        // Staged off-lock so the critical section is one memcpy.
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

        let Ok(mut frame) = self.frame.try_lock() else {
            return;
        };
        frame.objects = staged;
        frame.total_objects = total;
        frame.key = Some(key);
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
        }
    }

    fn budget() -> u64 {
        u64::try_from(MAX_VIEW_OBJECTS).expect("the object budget fits a u64")
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
        mirror.write(PlaybackKey::new(3, 1), [object(1, 0.5)]);

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
        mirror.write(key, [object(7, 0.25)]);
        mirror.write(key, std::iter::empty());

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
        mirror.write(PlaybackKey::new(1, 0), [object(7, 0.25)]);
        mirror.write(PlaybackKey::new(1, 1), std::iter::empty());

        assert!(mirror.read(PlaybackKey::new(1, 1)).is_none());
    }

    #[test]
    fn a_scene_past_the_budget_is_truncated_and_reports_what_it_hid() {
        let mirror = SceneViewMirror::new();
        let key = PlaybackKey::new(1, 0);
        let objects: Vec<ObjectView> = (0..budget() + 3).map(|id| object(id, 0.0)).collect();
        mirror.write(key, objects);

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
