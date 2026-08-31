use std::collections::BTreeMap;

use macindecode_windows_spatial_audio::{
    DynamicObjectRender, LfeObjectRender, RenderQuantum, SpatialSource,
};

use crate::decoder::{
    DecodedSceneBlock, SceneLfePcm, SceneObjectPcm, SceneQueueReader, SceneSignature,
    SpatialObjectState, SpatialPosition,
};

pub(super) struct SceneRenderSource {
    reader: SceneQueueReader,
    sample_rate: u32,
    dynamic_object_count: u32,
    has_lfe: bool,
    scene_signature: SceneSignature,
    timeline_frame: i64,
    current: Option<BlockCursor>,
}

impl SceneRenderSource {
    pub(super) const fn new(
        reader: SceneQueueReader,
        sample_rate: u32,
        dynamic_object_count: u32,
        has_lfe: bool,
        scene_signature: SceneSignature,
        start_frame: u64,
    ) -> Self {
        Self {
            reader,
            sample_rate,
            dynamic_object_count,
            has_lfe,
            scene_signature,
            timeline_frame: if start_frame > i64::MAX as u64 {
                i64::MAX
            } else {
                start_frame.cast_signed()
            },
            current: None,
        }
    }

    fn render_quantum(&mut self, frame_count: u32) -> Result<RenderQuantum, String> {
        let requested = usize::try_from(frame_count)
            .map_err(|_| "Windows Spatial Audio frame count exceeds usize".to_owned())?;
        let mut objects = BTreeMap::<u64, DynamicObjectRender>::new();
        let mut lfe = self.has_lfe.then(|| LfeQuantumAccumulator::new(requested));
        let mut written = 0usize;
        let mut underrun = false;

        while written < requested {
            if self.current.is_none() && !self.load_next_block()? {
                underrun = !self.reader.is_end_of_stream();
                break;
            }

            let Some(cursor) = self.current.as_mut() else {
                continue;
            };
            let block_position = cursor
                .block
                .start_frame()
                .checked_add(i64::from(cursor.offset_frames))
                .ok_or_else(|| "Scene block position overflow".to_owned())?;
            if block_position > self.timeline_frame {
                let available = usize::try_from(block_position - self.timeline_frame)
                    .unwrap_or(usize::MAX)
                    .min(requested - written);
                self.timeline_frame = self
                    .timeline_frame
                    .checked_add(i64::try_from(available).unwrap_or(i64::MAX))
                    .ok_or_else(|| "Scene gap position overflow".to_owned())?;
                written += available;
                continue;
            }

            let remaining_block = usize::try_from(
                cursor
                    .block
                    .duration_frames()
                    .saturating_sub(cursor.offset_frames),
            )
            .map_err(|_| "Scene block duration exceeds usize".to_owned())?;
            let take = remaining_block.min(requested - written);
            if take == 0 {
                self.current = None;
                continue;
            }
            copy_object_pcm(cursor, written, take, requested, &mut objects)?;
            copy_lfe_pcm(cursor, written, take, lfe.as_mut())?;
            let take_u32 =
                u32::try_from(take).map_err(|_| "Render quantum exceeds u32".to_owned())?;
            cursor.offset_frames = cursor.offset_frames.saturating_add(take_u32);
            self.timeline_frame = self
                .timeline_frame
                .checked_add(i64::from(take_u32))
                .ok_or_else(|| "Scene render position overflow".to_owned())?;
            written += take;
            if cursor.offset_frames == cursor.block.duration_frames() {
                self.current = None;
            }
        }

        let end_of_stream = self.current.is_none() && self.reader.is_end_of_stream();
        Ok(RenderQuantum {
            objects: objects.into_values().collect(),
            lfe: lfe.map(LfeQuantumAccumulator::finish),
            frames_written: u32::try_from(written).unwrap_or(u32::MAX),
            end_of_stream,
            underrun,
        })
    }

    fn load_next_block(&mut self) -> Result<bool, String> {
        loop {
            let Some(block) = self.reader.try_pop() else {
                return Ok(false);
            };
            validate_block(
                &block,
                self.sample_rate,
                self.dynamic_object_count,
                self.has_lfe,
                &self.scene_signature,
            )?;
            let block_end = block
                .start_frame()
                .checked_add(i64::from(block.duration_frames()))
                .ok_or_else(|| "Scene block end position overflow".to_owned())?;
            if block_end <= self.timeline_frame {
                continue;
            }
            let offset_frames = if block.start_frame() < self.timeline_frame {
                u32::try_from(self.timeline_frame - block.start_frame())
                    .map_err(|_| "Scene overlap exceeds a block".to_owned())?
            } else {
                0
            };
            self.current = Some(BlockCursor {
                block,
                offset_frames,
            });
            return Ok(true);
        }
    }
}

impl SpatialSource for SceneRenderSource {
    fn render(&mut self, frame_count: u32) -> Result<RenderQuantum, String> {
        self.render_quantum(frame_count)
    }
}

struct BlockCursor {
    block: DecodedSceneBlock,
    offset_frames: u32,
}

fn validate_block(
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

fn copy_object_pcm(
    cursor: &BlockCursor,
    destination_offset: usize,
    take: usize,
    requested: usize,
    renders: &mut BTreeMap<u64, DynamicObjectRender>,
) -> Result<(), String> {
    let source_start = usize::try_from(cursor.offset_frames)
        .map_err(|_| "Scene object offset exceeds usize".to_owned())?;
    let source_end = source_start
        .checked_add(take)
        .ok_or_else(|| "Scene object slice overflow".to_owned())?;
    let destination_end = destination_offset
        .checked_add(take)
        .ok_or_else(|| "Spatial Audio object slice overflow".to_owned())?;

    for object in cursor.block.objects() {
        let state = element_state_at(
            &cursor.block,
            object.element_id(),
            object.initial_state(),
            cursor.offset_frames,
        );
        let render = renders.entry(object.element_id()).or_insert_with(|| {
            let (active, position, gain) = windows_render_state(state);
            DynamicObjectRender {
                element_id: object.element_id(),
                active,
                position,
                gain,
                samples: vec![0.0; requested],
            }
        });
        render.samples[destination_offset..destination_end]
            .copy_from_slice(&object.samples()[source_start..source_end]);
    }
    Ok(())
}

fn copy_lfe_pcm(
    cursor: &BlockCursor,
    destination_offset: usize,
    take: usize,
    destination: Option<&mut LfeQuantumAccumulator>,
) -> Result<(), String> {
    let (Some(source), Some(destination)) = (cursor.block.lfe(), destination) else {
        return Ok(());
    };
    if !destination.state_initialized {
        let state = element_state_at(
            &cursor.block,
            source.element_id(),
            source.initial_state(),
            cursor.offset_frames,
        );
        (destination.render.active, destination.render.gain) = lfe_render_state(state);
        destination.state_initialized = true;
    }
    let source_start = usize::try_from(cursor.offset_frames)
        .map_err(|_| "Scene LFE offset exceeds usize".to_owned())?;
    let source_end = source_start
        .checked_add(take)
        .ok_or_else(|| "Scene LFE slice overflow".to_owned())?;
    let destination_end = destination_offset
        .checked_add(take)
        .ok_or_else(|| "Spatial Audio LFE slice overflow".to_owned())?;
    destination.render.samples[destination_offset..destination_end]
        .copy_from_slice(&source.samples()[source_start..source_end]);
    Ok(())
}

struct LfeQuantumAccumulator {
    render: LfeObjectRender,
    state_initialized: bool,
}

impl LfeQuantumAccumulator {
    fn new(frame_count: usize) -> Self {
        Self {
            render: LfeObjectRender {
                active: false,
                gain: 0.0,
                samples: vec![0.0; frame_count],
            },
            state_initialized: false,
        }
    }

    fn finish(self) -> LfeObjectRender {
        self.render
    }
}

fn element_state_at(
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

fn windows_render_state(state: Option<SpatialObjectState>) -> (bool, [f32; 3], f32) {
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

fn lfe_render_state(state: Option<SpatialObjectState>) -> (bool, f32) {
    let Some(state) = state else {
        return (false, 0.0);
    };
    (
        state.metadata_active() && state.semantic_complete(),
        state.linear_gain().unwrap_or(1.0),
    )
}

#[cfg(test)]
mod tests {
    use crate::decoder::{SceneLfePcm, SceneMetadataUpdate, SceneObjectPcm};

    use super::*;

    fn complete_state(gain: f32) -> SpatialObjectState {
        SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(0.25, -0.5, 0.75)),
            Some(gain),
            true,
        )
    }

    fn configured_block(generation: u32) -> DecodedSceneBlock {
        DecodedSceneBlock::new(
            48_000,
            0,
            4,
            generation,
            2,
            Some(7),
            true,
            vec![SceneObjectPcm::new(
                42,
                Some(complete_state(1.0)),
                vec![0.0; 4],
            )],
            None,
            Vec::new(),
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
    fn lfe_quantum_uses_ramped_state_and_pcm_at_its_start() {
        let target = complete_state(0.8);
        let cursor = BlockCursor {
            block: DecodedSceneBlock::new(
                48_000,
                0,
                6,
                1,
                0,
                None,
                true,
                Vec::new(),
                Some(SceneLfePcm::new(
                    99,
                    Some(complete_state(0.2)),
                    vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0],
                )),
                vec![SceneMetadataUpdate::new(99, 2, 2, u32::MAX, target)],
            ),
            offset_frames: 4,
        };
        let mut accumulator = LfeQuantumAccumulator::new(2);

        copy_lfe_pcm(&cursor, 0, 2, Some(&mut accumulator)).expect("copy LFE quantum");
        let render = accumulator.finish();
        assert!(render.active);
        assert!((render.gain - 0.8).abs() < f32::EPSILON);
        assert_eq!(render.samples, vec![4.0, 5.0]);
    }

    #[test]
    fn rejects_configuration_generation_changes_after_activation() {
        let initial = configured_block(3);
        let signature = SceneSignature::from_block(&initial);
        validate_block(&initial, 48_000, 1, false, &signature)
            .expect("first block establishes the active Scene configuration");

        let error = validate_block(&configured_block(4), 48_000, 1, false, &signature)
            .expect_err("a generation change must not reuse the active object IDs");
        assert!(error.contains("generation changed from 3 to 4"), "{error}");
        assert!(error.contains("must be reconfigured"), "{error}");
    }
}
