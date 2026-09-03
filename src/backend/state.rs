//! Resolving an element's OAMD state at a point inside a Scene block.
//!
//! This is pure arithmetic over `crate::decoder`'s cross-platform types, and it
//! is deliberately **not** gated on Windows even though only the Windows render
//! callback calls it today. Two reasons, both practical:
//!
//! * Its tests run everywhere. Sitting inside `backend::source` they compiled
//!   only on Windows, so on every other platform the ramp arithmetic — the part
//!   most likely to be wrong and least likely to be noticed — went unexercised.
//! * The scene view's future path samples the FIFO through the very same
//!   functions the audio uses. That is a correctness requirement rather than a
//!   convenience: a second, parallel derivation would drift from the audio
//!   across every ramp, and the drift would be invisible until someone compared
//!   the picture with what they heard.

use crate::decoder::{DecodedSceneBlock, SpatialObjectState, SpatialPosition};
use crate::scene_view::FuturePath;

/// The element's state `offset_frames` into `block`, following every metadata
/// update up to that point and interpolating whichever ramp is still running.
pub(super) fn element_state_at(
    block: &DecodedSceneBlock,
    element_id: u64,
    initial_state: Option<SpatialObjectState>,
    offset_frames: u32,
) -> Option<SpatialObjectState> {
    let mut state = initial_state;
    let mut ramp: Option<MetadataRamp> = None;
    for update in block
        .metadata_updates()
        .iter()
        .copied()
        .filter(|update| update.element_id() == element_id)
    {
        if update.offset_frames() > offset_frames {
            break;
        }
        let from = ramp
            .map(|active| active.state_at(update.offset_frames()))
            .or(state);
        if update.ramp_frames() == 0 {
            state = Some(update.state());
            ramp = None;
        } else if let Some(from) = from {
            state = Some(update.state());
            ramp = Some(MetadataRamp {
                start_frame: update.offset_frames(),
                duration_frames: update.ramp_frames(),
                from,
                to: update.state(),
            });
        } else {
            // With no state before the first complete update there is no valid ramp origin.
            // Establish the first known state at its update boundary instead of muting the
            // remainder of the Scene block.
            state = Some(update.state());
            ramp = None;
        }
    }
    ramp.map(|active| active.state_at(offset_frames)).or(state)
}

#[derive(Clone, Copy)]
struct MetadataRamp {
    start_frame: u32,
    duration_frames: u32,
    from: SpatialObjectState,
    to: SpatialObjectState,
}

impl MetadataRamp {
    #[allow(
        clippy::cast_precision_loss,
        reason = "metadata ramp offsets become a normalized interpolation fraction"
    )]
    fn state_at(self, frame: u32) -> SpatialObjectState {
        let elapsed = frame
            .saturating_sub(self.start_frame)
            .min(self.duration_frames);
        let amount = if self.duration_frames == 0 {
            1.0
        } else {
            elapsed as f32 / self.duration_frames as f32
        };
        interpolate_state(self.from, self.to, amount)
    }
}

fn interpolate_state(
    from: SpatialObjectState,
    to: SpatialObjectState,
    amount: f32,
) -> SpatialObjectState {
    let amount = amount.clamp(0.0, 1.0);
    if amount <= 0.0 {
        return from;
    }
    if amount >= 1.0 {
        return to;
    }
    let position = match (from.position(), to.position()) {
        (Some(from), Some(to)) => Some(SpatialPosition::new(
            lerp(from.x(), to.x(), amount),
            lerp(from.y(), to.y(), amount),
            lerp(from.z(), to.z(), amount),
        )),
        (_, target) => target,
    };
    let linear_gain = match (from.linear_gain(), to.linear_gain()) {
        (Some(from), Some(to)) => Some(lerp(from, to, amount)),
        (_, target) => target,
    };
    SpatialObjectState::new(
        to.metadata_active(),
        position,
        linear_gain,
        from.semantic_complete() && to.semantic_complete(),
    )
}

fn lerp(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount
}

/// Flatten a resolved state into what Windows Spatial Audio is handed for a
/// dynamic object: Core/ADM `[x, y, z]` becomes the listener coordinates
/// `[x, z, -y]`, clamped to the unit cube.
pub(super) fn windows_render_state(state: Option<SpatialObjectState>) -> (bool, [f32; 3], f32) {
    let Some(state) = state else {
        return (false, [0.0; 3], 0.0);
    };
    let Some(position) = state.position() else {
        return (false, [0.0; 3], 0.0);
    };
    let windows_position = [
        position.x().clamp(-1.0, 1.0),
        position.z().clamp(-1.0, 1.0),
        (-position.y()).clamp(-1.0, 1.0),
    ];
    let gain = state.linear_gain().unwrap_or(1.0);
    (
        state.metadata_active() && state.semantic_complete(),
        windows_position,
        gain,
    )
}

/// The same for the LFE bed, which carries activation and gain but no position.
pub(super) fn lfe_render_state(state: Option<SpatialObjectState>) -> (bool, f32) {
    let Some(state) = state else {
        return (false, 0.0);
    };
    (
        state.metadata_active() && state.semantic_complete(),
        state.linear_gain().unwrap_or(1.0),
    )
}

/// Sample every grid point of `path` that falls inside `block`.
///
/// `element_ids` is the Scene signature's object list, which is sorted, so a
/// slot is a binary search away and matches the mirror's slot order exactly —
/// both are element ID ascending. Nothing is allocated: this runs on the audio
/// thread, and for the queued blocks it runs while the FIFO lock is held.
///
/// An object with no position at some moment ends its own path there; the rest
/// carry on. Positions come from the same `element_state_at` the audio uses, so
/// the drawn path is the one that will actually be heard.
pub(super) fn sample_future_path(
    path: &mut FuturePath,
    block: &DecodedSceneBlock,
    element_ids: &[u64],
    interval_frames: i64,
) {
    if interval_frames <= 0 {
        return;
    }
    let start = block.start_frame();
    let Some(end) = start.checked_add(i64::from(block.duration_frames())) else {
        return;
    };
    // A forward timeline gap carries no element state at all. Re-anchoring the
    // grid to the next block that does is the alternative to ending every
    // object's path at the first gap; one dash comes out short, which is a far
    // smaller lie than the path disappearing.
    path.skip_to(start);

    while path.next_frame() < end {
        let Ok(offset) = u32::try_from(path.next_frame() - start) else {
            return;
        };
        for object in block.objects() {
            let Ok(slot) = element_ids.binary_search(&object.element_id()) else {
                continue;
            };
            let state =
                element_state_at(block, object.element_id(), object.initial_state(), offset);
            let (active, position, _) = windows_render_state(state);
            path.push(slot, active.then_some(position));
        }
        path.advance(interval_frames);
    }
}

#[cfg(test)]
mod tests {
    use crate::decoder::{SceneMetadataUpdate, SceneObjectPcm};
    use crate::scene_view::sample_interval_frames;
    use crate::scene3d::params::FUTURE_SAMPLES;

    use super::*;

    fn complete_state(gain: f32) -> SpatialObjectState {
        SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(0.25, -0.5, 0.75)),
            Some(gain),
            true,
        )
    }

    const RATE: u32 = 48_000;

    fn positioned(x: f32) -> SpatialObjectState {
        SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(x, 0.0, 0.0)),
            Some(1.0),
            true,
        )
    }

    /// A Scene block carrying element state but no PCM: the sampler never reads
    /// samples, only metadata.
    fn block(
        start_frame: i64,
        duration_frames: u32,
        objects: Vec<SceneObjectPcm>,
        updates: Vec<SceneMetadataUpdate>,
    ) -> DecodedSceneBlock {
        DecodedSceneBlock::new(
            RATE,
            start_frame,
            duration_frames,
            1,
            0,
            None,
            true,
            objects,
            None,
            updates,
        )
    }

    fn interval() -> i64 {
        sample_interval_frames(RATE)
    }

    fn xs(path: &FuturePath, slot: usize) -> Vec<f32> {
        path.path(slot).iter().map(|point| point[0]).collect()
    }

    #[test]
    fn maps_core_adm_axes_to_windows_listener_coordinates() {
        let state = SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(0.5, 1.0, 0.25)),
            Some(0.75),
            true,
        );
        let (active, position, gain) = windows_render_state(Some(state));
        assert!(active);
        for (actual, expected) in position.into_iter().zip([0.5, 0.25, -1.0]) {
            assert!((actual - expected).abs() < f32::EPSILON);
        }
        assert!((gain - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn interpolates_position_and_gain_over_metadata_ramps() {
        let from = SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(-1.0, 0.0, 0.0)),
            Some(0.25),
            true,
        );
        let to = SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(1.0, 0.0, 1.0)),
            Some(0.75),
            true,
        );
        let middle = interpolate_state(from, to, 0.5);
        assert_eq!(middle.position(), Some(SpatialPosition::new(0.0, 0.0, 0.5)));
        assert_eq!(middle.linear_gain(), Some(0.5));
    }

    #[test]
    fn later_update_establishes_state_when_initial_state_is_missing() {
        let target = complete_state(0.6);
        let block = DecodedSceneBlock::new(
            48_000,
            0,
            16,
            1,
            0,
            None,
            true,
            vec![SceneObjectPcm::new(42, None, vec![0.0; 16])],
            None,
            vec![SceneMetadataUpdate::new(42, 4, 8, u32::MAX, target)],
        );

        assert_eq!(element_state_at(&block, 42, None, 3), None);
        assert_eq!(element_state_at(&block, 42, None, 4), Some(target));
        assert_eq!(element_state_at(&block, 42, None, 12), Some(target));
    }

    #[test]
    fn ramp_endpoint_uses_the_exact_complete_target_state() {
        let incomplete = SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(-1.0, 0.0, 0.0)),
            Some(0.25),
            false,
        );
        let complete = complete_state(0.9);

        assert_eq!(interpolate_state(incomplete, complete, 1.0), complete);
        assert_eq!(
            MetadataRamp {
                start_frame: 10,
                duration_frames: 5,
                from: incomplete,
                to: complete,
            }
            .state_at(15),
            complete
        );
    }

    #[test]
    fn lfe_render_state_follows_oamd_activation_and_gain() {
        assert_eq!(lfe_render_state(None), (false, 0.0));
        assert_eq!(lfe_render_state(Some(complete_state(0.4))), (true, 0.4));

        let inactive = SpatialObjectState::new(false, None, Some(0.8), true);
        assert_eq!(lfe_render_state(Some(inactive)), (false, 0.8));
    }

    #[test]
    fn the_path_is_sampled_on_the_interval_and_follows_the_ramp_the_audio_will_hear() {
        // The whole point of routing this through `element_state_at`: the line
        // drawn ahead of an object is the trajectory that will actually be
        // rendered, not a parallel guess that drifts across every ramp.
        let step = interval();
        let span = u32::try_from(step * 5).expect("span fits a u32");
        let block = block(
            0,
            span,
            vec![SceneObjectPcm::new(7, Some(positioned(-1.0)), Vec::new())],
            vec![SceneMetadataUpdate::new(
                7,
                0,
                span,
                u32::MAX,
                positioned(1.0),
            )],
        );

        let mut path = FuturePath::new();
        path.restart(0, 1);
        sample_future_path(&mut path, &block, &[7], step);

        // Five grid points fall inside the block, and the ramp is linear across
        // it, so each is two fifths of the way along from the one before.
        let sampled = xs(&path, 0);
        assert_eq!(sampled.len(), 5);
        for (index, actual) in sampled.iter().enumerate() {
            #[allow(
                clippy::cast_precision_loss,
                reason = "five sample indices convert exactly"
            )]
            let expected = -1.0 + 0.4 * index as f32;
            assert!(
                (actual - expected).abs() < 1e-5,
                "sample {index} is {actual}, expected {expected}"
            );
        }
    }

    #[test]
    fn an_object_that_loses_its_position_ends_only_its_own_path() {
        let step = interval();
        let span = u32::try_from(step * 5).expect("span fits a u32");
        let silent = SpatialObjectState::new(
            false,
            Some(SpatialPosition::new(0.5, 0.0, 0.0)),
            Some(1.0),
            true,
        );
        let block = block(
            0,
            span,
            vec![
                SceneObjectPcm::new(3, Some(positioned(-0.5)), Vec::new()),
                SceneObjectPcm::new(9, Some(positioned(0.5)), Vec::new()),
            ],
            vec![SceneMetadataUpdate::new(
                9,
                u32::try_from(step * 2).expect("offset fits a u32"),
                0,
                u32::MAX,
                silent,
            )],
        );

        let mut path = FuturePath::new();
        path.restart(0, 2);
        sample_future_path(&mut path, &block, &[3, 9], step);

        assert_eq!(path.path(0).len(), 5, "a live object stopped early");
        assert_eq!(
            path.path(1).len(),
            2,
            "the path continued past the point the object stops being placed"
        );
    }

    #[test]
    fn a_path_closed_once_is_not_reopened_by_a_later_block() {
        // Without this the line would leap the silent stretch and draw a segment
        // the object never travels.
        let step = interval();
        let span = u32::try_from(step).expect("span fits a u32");
        let ids = [4];
        let mut path = FuturePath::new();
        path.restart(0, 1);

        let absent = block(
            0,
            span,
            vec![SceneObjectPcm::new(4, None, Vec::new())],
            vec![],
        );
        sample_future_path(&mut path, &absent, &ids, step);
        assert!(path.path(0).is_empty());

        let back = block(
            step,
            span,
            vec![SceneObjectPcm::new(4, Some(positioned(0.25)), Vec::new())],
            vec![],
        );
        sample_future_path(&mut path, &back, &ids, step);
        assert!(path.path(0).is_empty(), "a closed path accepted new points");
        assert!(path.is_complete());
    }

    #[test]
    fn contiguous_blocks_continue_one_path_without_repeating_a_sample() {
        let step = interval();
        let span = u32::try_from(step * 2).expect("span fits a u32");
        let ids = [7];
        let first = block(
            0,
            span,
            vec![SceneObjectPcm::new(7, Some(positioned(-1.0)), Vec::new())],
            vec![],
        );
        let second = block(
            step * 2,
            span,
            vec![SceneObjectPcm::new(7, Some(positioned(1.0)), Vec::new())],
            vec![],
        );

        let mut path = FuturePath::new();
        path.restart(0, 1);
        sample_future_path(&mut path, &first, &ids, step);
        sample_future_path(&mut path, &second, &ids, step);

        assert_eq!(xs(&path, 0), vec![-1.0, -1.0, 1.0, 1.0]);
    }

    #[test]
    fn a_forward_gap_re_anchors_the_grid_instead_of_ending_the_path() {
        // A gap carries no element state at all. Ending every path at the first
        // one would make the line vanish over a pre-roll; re-anchoring costs one
        // short dash.
        let step = interval();
        let span = u32::try_from(step).expect("span fits a u32");
        let ids = [7];
        let before = block(
            0,
            span,
            vec![SceneObjectPcm::new(7, Some(positioned(-1.0)), Vec::new())],
            vec![],
        );
        let after = block(
            step * 4,
            span,
            vec![SceneObjectPcm::new(7, Some(positioned(1.0)), Vec::new())],
            vec![],
        );

        let mut path = FuturePath::new();
        path.restart(0, 1);
        sample_future_path(&mut path, &before, &ids, step);
        sample_future_path(&mut path, &after, &ids, step);

        assert_eq!(xs(&path, 0), vec![-1.0, 1.0]);
    }

    #[test]
    fn the_path_stops_at_its_capacity_and_reports_itself_complete() {
        let step = interval();
        let samples = i64::try_from(FUTURE_SAMPLES).expect("the budget fits an i64");
        let span = u32::try_from(step * (samples + 10)).expect("span fits a u32");
        let block = block(
            0,
            span,
            vec![SceneObjectPcm::new(7, Some(positioned(0.0)), Vec::new())],
            vec![],
        );

        let mut path = FuturePath::new();
        path.restart(0, 1);
        sample_future_path(&mut path, &block, &[7], step);

        assert_eq!(path.path(0).len(), FUTURE_SAMPLES);
        assert!(path.is_complete());
    }

    #[test]
    fn slots_follow_the_signature_order_the_mirror_uses() {
        // The mirror fills its slots from a BTreeMap keyed by element ID, and
        // the signature's list is sorted, so the two agree only if the sampler
        // resolves slots by that list rather than by the block's own order.
        let step = interval();
        let span = u32::try_from(step).expect("span fits a u32");
        let block = block(
            0,
            span,
            vec![
                SceneObjectPcm::new(9, Some(positioned(0.9)), Vec::new()),
                SceneObjectPcm::new(3, Some(positioned(0.3)), Vec::new()),
            ],
            vec![],
        );

        let mut path = FuturePath::new();
        path.restart(0, 2);
        sample_future_path(&mut path, &block, &[3, 9], step);

        assert_eq!(xs(&path, 0), vec![0.3]);
        assert_eq!(xs(&path, 1), vec![0.9]);
    }
}
