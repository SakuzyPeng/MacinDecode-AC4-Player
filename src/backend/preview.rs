//! The scene view's clock on a build with no spatial output.
//!
//! The mirror is normally written from the Windows render callback: the view is
//! literally the object set that was just handed to the renderer, which is why
//! it cannot drift from what is audible. Without a renderer there is no
//! callback, so nothing drains the Scene FIFO — it fills to its two-second
//! bound, decoding stops, and the stage stays an empty room no matter how much
//! of the file has been read.
//!
//! This is the substitute. It walks the same FIFO at wall-clock rate and
//! resolves object state through the same [`element_state_at`] the audio path
//! uses, so the positions it publishes are the ones that would have been
//! submitted. What it does not do is touch PCM: there is nowhere to send it.
//!
//! Two invariants keep it from ever competing with real playback:
//!
//! - It is constructed only where no renderer owns the FIFO, so exactly one
//!   consumer pops a given queue.
//! - Every write carries the [`PlaybackKey`] the reader was bound to, so a
//!   superseded playback's positions are rejected on read like any other.

use std::sync::Arc;

use crate::decoder::{DecodedSceneBlock, PlaybackKey, SceneQueueReader};
use crate::scene_view::{MAX_VIEW_OBJECTS, ObjectView, SceneViewMirror};

use super::state::{element_state_at, has_instant_update, listener_render_state};

/// Where the walk has reached inside one popped block.
struct BlockCursor {
    block: DecodedSceneBlock,
    offset_frames: u32,
}

pub(super) struct ScenePreview {
    reader: SceneQueueReader,
    mirror: Arc<SceneViewMirror>,
    key: PlaybackKey,
    /// The signature's element IDs, ascending. Mirror slots are positions in
    /// this list, matching how the render path orders its `BTreeMap`.
    element_ids: Vec<u64>,
    sample_rate: u32,
    timeline_frame: i64,
    current: Option<BlockCursor>,
    ended: bool,
    /// Sub-frame remainder carried between ticks. At 48 kHz a 16 ms frame is
    /// 768 samples exactly, but nothing guarantees the frame time divides
    /// evenly, and dropping the remainder every tick would run the preview
    /// measurably slow.
    carry_frames: f64,
}

impl ScenePreview {
    pub(super) fn new(
        reader: SceneQueueReader,
        mirror: Arc<SceneViewMirror>,
        element_ids: Vec<u64>,
        sample_rate: u32,
        start_frame: u64,
    ) -> Self {
        Self {
            key: reader.playback_key(),
            reader,
            mirror,
            element_ids,
            sample_rate,
            timeline_frame: i64::try_from(start_frame).unwrap_or(i64::MAX),
            current: None,
            ended: false,
            carry_frames: 0.0,
        }
    }

    pub(super) const fn playhead_frames(&self) -> u64 {
        if self.timeline_frame < 0 {
            0
        } else {
            self.timeline_frame.cast_unsigned()
        }
    }

    pub(super) const fn has_ended(&self) -> bool {
        self.ended
    }

    /// Move the preview forward by one UI frame and publish what it lands on.
    ///
    /// Returns whether anything was published, so the caller can leave the
    /// snapshot alone on a tick that produced nothing.
    pub(super) fn advance(&mut self, delta_seconds: f32) -> bool {
        if self.ended {
            return false;
        }
        let Some(mut remaining) = self.frames_for(delta_seconds) else {
            return false;
        };

        let mut jumped = [false; MAX_VIEW_OBJECTS];
        while remaining > 0 {
            let exhausted = self
                .current
                .as_ref()
                .is_some_and(|cursor| cursor.offset_frames >= cursor.block.duration_frames());
            if self.current.is_none() || exhausted {
                let Some(block) = self.reader.try_pop() else {
                    // Either the decoder has not caught up or the file is done.
                    // Both mean hold position rather than invent one -- and hold
                    // the spent block too, so `publish` still has a state to
                    // resolve instead of going silent for the whole stall.
                    self.ended = self.reader.is_end_of_stream();
                    break;
                };
                self.current = Some(BlockCursor {
                    block,
                    offset_frames: 0,
                });
            }
            let Some(cursor) = self.current.as_mut() else {
                break;
            };

            // A block may start after the timeline: MP4 edit lists leave real
            // gaps. Cross them in silence rather than snapping the clock.
            let block_position = cursor
                .block
                .start_frame()
                .saturating_add(i64::from(cursor.offset_frames));
            if block_position > self.timeline_frame {
                let gap = (block_position - self.timeline_frame).min(remaining);
                self.timeline_frame = self.timeline_frame.saturating_add(gap);
                remaining -= gap;
                continue;
            }

            let available = i64::from(
                cursor
                    .block
                    .duration_frames()
                    .saturating_sub(cursor.offset_frames),
            );
            let take = available.min(remaining);
            if take <= 0 {
                break;
            }
            let take_frames = u32::try_from(take).unwrap_or(u32::MAX);
            let span_end = cursor.offset_frames.saturating_add(take_frames);
            for (slot, element_id) in self.element_ids.iter().enumerate() {
                if let Some(flag) = jumped.get_mut(slot)
                    && has_instant_update(
                        &cursor.block,
                        *element_id,
                        cursor.offset_frames,
                        span_end,
                    )
                {
                    *flag = true;
                }
            }
            cursor.offset_frames = span_end;
            self.timeline_frame = self.timeline_frame.saturating_add(take);
            remaining -= take;
        }

        self.publish(&jumped);
        true
    }

    /// Whole frames to advance this tick, folding in the carried remainder.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "`whole` is a non-negative integral f64 bounded by a quarter \
                  second of frames, so it is far inside the i64 range"
    )]
    fn frames_for(&mut self, delta_seconds: f32) -> Option<i64> {
        if !delta_seconds.is_finite() || delta_seconds <= 0.0 {
            return None;
        }
        // A stalled UI thread must not make the preview lurch forward through
        // seconds of the file at once; it is a view, so dropping time is the
        // right failure.
        let step = f64::from(delta_seconds).min(0.25) * f64::from(self.sample_rate);
        let total = step + self.carry_frames;
        let whole = total.floor();
        self.carry_frames = total - whole;
        let frames = whole as i64;
        (frames > 0).then_some(frames)
    }

    /// Resolve every element where the walk stopped and hand it to the mirror.
    fn publish(&self, jumped: &[bool; MAX_VIEW_OBJECTS]) {
        let Some(cursor) = self.current.as_ref() else {
            return;
        };
        // Clamp to the last frame the walk actually reached: a block consumed
        // exactly to its end is kept until the next tick precisely so its final
        // state stays resolvable here.
        let offset = cursor
            .offset_frames
            .min(cursor.block.duration_frames().saturating_sub(1));

        let mut views = [None; MAX_VIEW_OBJECTS];
        let mut count = 0;
        for (slot, element_id) in self.element_ids.iter().enumerate() {
            let Some(object) = cursor
                .block
                .objects()
                .iter()
                .find(|object| object.element_id() == *element_id)
            else {
                continue;
            };
            let state =
                element_state_at(&cursor.block, *element_id, object.initial_state(), offset);
            let (active, position, gain) = listener_render_state(state);
            if let Some(entry) = views.get_mut(count) {
                *entry = Some(ObjectView {
                    element_id: *element_id,
                    active,
                    position,
                    gain,
                    jumped: jumped.get(slot).copied().unwrap_or(false),
                });
                count += 1;
            }
        }

        self.mirror.write(
            self.key,
            views.into_iter().take(count).flatten(),
            self.timeline_frame,
            self.sample_rate,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::{
        DecodedSceneBlock, PlaybackKey, SceneMetadataUpdate, SceneObjectPcm, SharedSceneQueue,
        SpatialObjectState, SpatialPosition, scene_queue_pair,
    };

    const RATE: u32 = 48_000;
    const BLOCK: u32 = 2_048;

    fn state(x: f32, y: f32, z: f32) -> SpatialObjectState {
        SpatialObjectState::new(true, Some(SpatialPosition::new(x, y, z)), Some(1.0), true)
    }

    /// One block whose single object ramps along x across the whole block.
    fn sweeping_block(start_frame: i64) -> DecodedSceneBlock {
        DecodedSceneBlock::new(
            RATE,
            start_frame,
            BLOCK,
            1,
            0,
            None,
            true,
            vec![SceneObjectPcm::new(
                7,
                Some(state(-1.0, 0.0, 0.0)),
                vec![0.0; BLOCK as usize],
            )],
            None,
            vec![SceneMetadataUpdate::new(
                7,
                0,
                BLOCK,
                u32::MAX,
                state(1.0, 0.0, 0.0),
            )],
        )
    }

    /// Consecutive still blocks. The FIFO is bounded at two seconds, so a test
    /// that queues more than 46 of these gets `Full` rather than a long file.
    fn queued_blocks(count: i64) -> Vec<DecodedSceneBlock> {
        (0..count)
            .map(|index| still_block(index * i64::from(BLOCK)))
            .collect()
    }

    fn still_block(start_frame: i64) -> DecodedSceneBlock {
        DecodedSceneBlock::new(
            RATE,
            start_frame,
            BLOCK,
            1,
            0,
            None,
            true,
            vec![SceneObjectPcm::new(
                7,
                Some(state(0.0, 0.0, 0.0)),
                vec![0.0; BLOCK as usize],
            )],
            None,
            Vec::new(),
        )
    }

    fn preview_over(
        key: PlaybackKey,
        blocks: Vec<DecodedSceneBlock>,
    ) -> (SharedSceneQueue, Arc<SceneViewMirror>, ScenePreview) {
        let (queue, reader) = scene_queue_pair(key);
        for block in blocks {
            queue.try_push(key, block).expect("queue the block");
        }
        let mirror = Arc::new(SceneViewMirror::new());
        let preview = ScenePreview::new(reader, Arc::clone(&mirror), vec![7], RATE, 0);
        (queue, mirror, preview)
    }

    fn published_x(mirror: &SceneViewMirror, key: PlaybackKey) -> f32 {
        let frame = mirror.read(key).expect("the preview published a frame");
        frame.objects().first().expect("one object").position[0]
    }

    #[test]
    fn the_clock_advances_by_wall_time_times_the_sample_rate() {
        let key = PlaybackKey::new(1, 0);
        let blocks = queued_blocks(8);
        let (_queue, _mirror, mut preview) = preview_over(key, blocks);

        preview.advance(0.1);
        assert_eq!(preview.playhead_frames(), 4_800);
        preview.advance(0.1);
        assert_eq!(preview.playhead_frames(), 9_600);
    }

    #[test]
    fn a_stalled_frame_drops_time_rather_than_lurching_through_the_file() {
        // A view that skipped a second of the file on a slow frame would show a
        // position the listener never passed through.
        let key = PlaybackKey::new(1, 0);
        let (_queue, _mirror, mut preview) = preview_over(key, queued_blocks(16));

        // `frames_for` caps one tick at a quarter of a second.
        preview.advance(5.0);
        assert_eq!(preview.playhead_frames(), 12_000);
    }

    #[test]
    fn sub_frame_remainders_are_carried_instead_of_dropped() {
        // 1/60 s at 48 kHz is 800 frames exactly, but 1/61 is not. Dropping the
        // remainder every tick would run the preview measurably slow.
        let key = PlaybackKey::new(1, 0);
        let (_queue, _mirror, mut preview) = preview_over(key, queued_blocks(24));

        for _ in 0..61 {
            preview.advance(1.0 / 61.0);
        }
        let drift = i64::from(RATE) - preview.playhead_frames().cast_signed();
        assert!(
            drift.abs() <= 1,
            "carried remainder drifted by {drift} frames"
        );
    }

    #[test]
    fn a_ramping_object_is_published_where_the_ramp_puts_it() {
        // The one thing a static bed cannot demonstrate: the published position
        // has to track the OAMD ramp, not the block's starting state.
        let key = PlaybackKey::new(1, 0);
        let (_queue, mirror, mut preview) = preview_over(key, vec![sweeping_block(0)]);

        // A quarter of the block in, a -1..1 ramp is halfway to the midpoint.
        preview.advance(f32::from(512_u16) / 48_000.0);
        let quarter = published_x(&mirror, key);
        assert!(
            (quarter + 0.5).abs() < 0.01,
            "quarter-way through the ramp published x={quarter}"
        );

        preview.advance(f32::from(512_u16) / 48_000.0);
        let half = published_x(&mirror, key);
        assert!(
            (half).abs() < 0.01,
            "halfway through the ramp published x={half}"
        );
        assert!(half > quarter, "the object did not move along the ramp");
    }

    #[test]
    fn an_underrun_holds_the_last_position_rather_than_inventing_one() {
        let key = PlaybackKey::new(1, 0);
        let (_queue, mirror, mut preview) = preview_over(key, vec![sweeping_block(0)]);

        preview.advance(1.0);
        let stalled = preview.playhead_frames();
        assert_eq!(stalled, u64::from(BLOCK), "walked past the queued block");
        let held = published_x(&mirror, key);

        preview.advance(1.0);
        assert_eq!(
            preview.playhead_frames(),
            stalled,
            "clock ran past the data"
        );
        assert!(
            (published_x(&mirror, key) - held).abs() < f32::EPSILON,
            "an underrun moved the object"
        );
        assert!(
            !preview.has_ended(),
            "an underrun is not the end of the file"
        );
    }

    #[test]
    fn the_end_is_reported_only_once_the_queue_has_drained() {
        let key = PlaybackKey::new(1, 0);
        let (queue, _mirror, mut preview) = preview_over(key, vec![still_block(0)]);
        queue.mark_end_of_stream(key);

        assert!(!preview.has_ended(), "ended before the block was walked");
        preview.advance(1.0);
        assert!(
            preview.has_ended(),
            "the drained queue was not reported as ended"
        );
        assert_eq!(preview.playhead_frames(), u64::from(BLOCK));
    }

    #[test]
    fn a_forward_gap_is_crossed_without_snapping_the_clock() {
        // An MP4 edit list can leave the first block starting after zero. The
        // clock has to walk the gap, not teleport over it.
        let key = PlaybackKey::new(1, 0);
        let (_queue, _mirror, mut preview) = preview_over(key, vec![still_block(24_000)]);

        preview.advance(0.1);
        assert_eq!(preview.playhead_frames(), 4_800, "the gap was skipped");
        preview.advance(0.25);
        preview.advance(0.25);
        assert_eq!(
            preview.playhead_frames(),
            u64::from(24_000 + BLOCK),
            "the block after the gap was not walked"
        );
    }

    #[test]
    fn a_superseded_playback_cannot_publish_into_the_live_view() {
        let key = PlaybackKey::new(1, 0);
        let (_queue, mirror, mut preview) = preview_over(key, vec![still_block(0)]);
        preview.advance(0.01);

        assert!(
            mirror.read(key).is_some(),
            "the bound key reads its own frame"
        );
        assert!(
            mirror.read(PlaybackKey::new(1, 1)).is_none(),
            "a later epoch accepted a stale frame"
        );
    }
}
