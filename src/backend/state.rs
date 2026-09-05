//! Resolving an element's OAMD state at a point inside a Scene block.
//!
//! This is pure arithmetic over `crate::decoder`'s cross-platform types, and it
//! is shared by the Windows render callback and the scene preview. Validation,
//! timeline trimming and OAMD resolution must agree between those consumers;
//! their arithmetic and its tests run on every platform.

use crate::decoder::{
    DecodedSceneBlock, SceneLfePcm, SceneObjectPcm, SceneSignature, SpatialObjectState,
    SpatialPosition,
};

/// Check the same Scene contract before either consumer accepts a block.
/// Error prefixes are also the app's automatic-reconfiguration contract.
pub(super) fn validate_block(
    block: &DecodedSceneBlock,
    sample_rate: u32,
    dynamic_object_count: u32,
    stream_has_lfe: bool,
    expected_signature: &SceneSignature,
) -> Result<(), String> {
    if block.sample_rate() != sample_rate {
        return Err(format!(
            "Scene sample rate changed from {sample_rate} to {} Hz",
            block.sample_rate()
        ));
    }
    let expected = usize::try_from(block.duration_frames())
        .map_err(|_| "Scene block duration exceeds usize".to_owned())?;
    let actual_dynamic_objects = u32::try_from(block.objects().len())
        .map_err(|_| "Scene object count exceeds the Windows API range".to_owned())?;
    if actual_dynamic_objects != dynamic_object_count {
        return Err(format!(
            "Scene dynamic-object count changed from {dynamic_object_count} to {actual_dynamic_objects} after Spatial Audio activation"
        ));
    }
    if block.lfe().is_some() != stream_has_lfe {
        return Err("Scene LFE layout changed after Spatial Audio activation".to_owned());
    }
    if expected_signature.configuration_generation() != block.configuration_generation() {
        return Err(format!(
            "Scene configuration generation changed from {} to {}; the Spatial Audio stream must be reconfigured",
            expected_signature.configuration_generation(),
            block.configuration_generation()
        ));
    }
    if expected_signature.presentation_index() != block.presentation_index()
        || expected_signature.presentation_id() != block.presentation_id()
    {
        return Err("Selected Scene presentation changed during Spatial Audio playback".to_owned());
    }
    let mut actual_object_ids = block
        .objects()
        .iter()
        .map(SceneObjectPcm::element_id)
        .collect::<Vec<_>>();
    actual_object_ids.sort_unstable();
    if actual_object_ids != expected_signature.object_element_ids() {
        return Err("Scene dynamic-object element IDs changed during playback".to_owned());
    }
    if block.lfe().map(SceneLfePcm::element_id) != expected_signature.lfe_element_id() {
        return Err("Scene LFE element ID changed during playback".to_owned());
    }
    for object in block.objects() {
        if object.samples().len() != expected {
            return Err(format!(
                "Scene object {} PCM length does not match its block",
                object.element_id()
            ));
        }
    }
    if let Some(component) = block.lfe()
        && component.samples().len() != expected
    {
        return Err("Scene LFE PCM length does not match its block".to_owned());
    }
    Ok(())
}

/// Offset of the first frame on the current presentation timeline.
/// `None` skips an entirely expired block, including MP4 pre-zero preroll.
pub(super) fn block_offset_at(
    block: &DecodedSceneBlock,
    timeline_frame: i64,
) -> Result<Option<u32>, String> {
    let block_end = block
        .start_frame()
        .checked_add(i64::from(block.duration_frames()))
        .ok_or_else(|| "Scene block end position overflow".to_owned())?;
    if block_end <= timeline_frame {
        return Ok(None);
    }
    let offset = if block.start_frame() < timeline_frame {
        u32::try_from(timeline_frame - block.start_frame())
            .map_err(|_| "Scene overlap exceeds a block".to_owned())?
    } else {
        0
    };
    Ok(Some(offset))
}

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

/// Flatten a resolved state into listener coordinates: Core/ADM `[x, y, z]`
/// becomes `[x, z, -y]`, clamped to the unit cube.
///
/// This is what Windows Spatial Audio is handed for a dynamic object, and also
/// what the scene view draws in — one conversion, so a picture of a scene and
/// the scene itself cannot disagree about where anything is.
pub(super) fn listener_render_state(state: Option<SpatialObjectState>) -> (bool, [f32; 3], f32) {
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
///
/// Unlike its neighbours this one has a single consumer. The scene view draws
/// the LFE slot from the decoder's `has_lfe`, not from the mirror — the mirror
/// carries no LFE state at all — so only the render callback, submitting bed
/// gain to Windows Spatial Audio, ever asks for it.
#[cfg_attr(
    not(spatial_output),
    allow(dead_code, reason = "only the render callback submits LFE bed gain")
)]
pub(super) fn lfe_render_state(state: Option<SpatialObjectState>) -> (bool, f32) {
    let Some(state) = state else {
        return (false, 0.0);
    };
    (
        state.metadata_active() && state.semantic_complete(),
        state.linear_gain().unwrap_or(1.0),
    )
}

/// Whether an instant metadata update for `element_id` lands in
/// `[from_offset, to_offset)` of `block`.
///
/// `ramp_frames == 0` is OAMD stating outright that nothing is interpolated:
/// the element is at one position and then at another, with no moment in
/// between at which it was anywhere else. That is a fact about the bitstream,
/// not a guess from how far the object moved — whether the jump is *worth
/// drawing a marker for* is a separate, perceptual question, and it is decided
/// in `scene3d`.
pub(super) fn has_instant_update(
    block: &DecodedSceneBlock,
    element_id: u64,
    from_offset: u32,
    to_offset: u32,
) -> bool {
    for update in block.metadata_updates() {
        // Updates are in ascending offset order, the same assumption
        // `element_state_at` makes.
        if update.offset_frames() >= to_offset {
            break;
        }
        if update.element_id() == element_id
            && update.ramp_frames() == 0
            && update.offset_frames() >= from_offset
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use crate::decoder::{SceneMetadataUpdate, SceneObjectPcm};

    use super::*;

    fn complete_state(gain: f32) -> SpatialObjectState {
        SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(0.25, -0.5, 0.75)),
            Some(gain),
            true,
        )
    }

    #[test]
    fn maps_core_adm_axes_to_windows_listener_coordinates() {
        let state = SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(0.5, 1.0, 0.25)),
            Some(0.75),
            true,
        );
        let (active, position, gain) = listener_render_state(Some(state));
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
    fn instant_update_detection_is_half_open_element_specific_and_ignores_ramps() {
        let state = complete_state(1.0);
        let block = DecodedSceneBlock::new(
            48_000,
            0,
            16,
            1,
            0,
            None,
            true,
            Vec::new(),
            None,
            vec![
                SceneMetadataUpdate::new(7, 2, 0, u32::MAX, state),
                SceneMetadataUpdate::new(9, 4, 0, u32::MAX, state),
                SceneMetadataUpdate::new(7, 6, 3, u32::MAX, state),
                SceneMetadataUpdate::new(7, 8, 0, u32::MAX, state),
            ],
        );

        assert!(has_instant_update(&block, 7, 2, 3));
        assert!(!has_instant_update(&block, 7, 0, 2));
        assert!(!has_instant_update(&block, 7, 3, 8));
        assert!(has_instant_update(&block, 7, 3, 9));
        assert!(!has_instant_update(&block, 8, 0, 16));
    }
}
