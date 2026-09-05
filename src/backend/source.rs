use std::collections::BTreeMap;
use std::sync::Arc;

use macindecode_windows_spatial_audio::{
    DynamicObjectRender, LfeObjectRender, RenderQuantum, SpatialSource,
};

use crate::decoder::{DecodedSceneBlock, PlaybackKey, SceneQueueReader, SceneSignature};
use crate::scene_view::{MAX_VIEW_OBJECTS, ObjectView, SceneViewMirror};

use super::state::{
    block_offset_at, element_state_at, has_instant_update, lfe_render_state, listener_render_state,
    validate_block,
};

pub(super) struct SceneRenderSource {
    pose: Arc<crate::head_tracking::PoseMirror>,
    last_pose: crate::head_tracking::Quaternion,
    reader: SceneQueueReader,
    /// Where the UI reads this stream's object positions from. Written at the
    /// end of every quantum; see [`SceneViewMirror`] for why that never blocks.
    mirror: Arc<SceneViewMirror>,
    /// Stamped onto every mirrored frame so a superseded playback's positions
    /// cannot reach the view. Taken from `reader`, which is already gated by it.
    key: PlaybackKey,
    sample_rate: u32,
    dynamic_object_count: u32,
    has_lfe: bool,
    scene_signature: SceneSignature,
    timeline_frame: i64,
    current: Option<BlockCursor>,
}

impl SceneRenderSource {
    pub(super) fn new(
        reader: SceneQueueReader,
        mirror: Arc<SceneViewMirror>,
        sample_rate: u32,
        dynamic_object_count: u32,
        has_lfe: bool,
        scene_signature: SceneSignature,
        start_frame: u64,
    ) -> Self {
        Self {
            pose: Arc::new(crate::head_tracking::PoseMirror::default()),
            last_pose: crate::head_tracking::Quaternion::default(),
            key: reader.playback_key(),
            reader,
            mirror,
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

    pub(super) fn with_pose(mut self, pose: Arc<crate::head_tracking::PoseMirror>) -> Self {
        self.pose = pose;
        self
    }

    fn render_quantum(&mut self, frame_count: u32) -> Result<RenderQuantum, String> {
        let requested = usize::try_from(frame_count)
            .map_err(|_| "Windows Spatial Audio frame count exceeds usize".to_owned())?;
        let mut objects = BTreeMap::<u64, DynamicObjectRender>::new();
        // Per slot, not per element: the mirror's slots are the signature's
        // sorted element IDs, and so is `objects`' BTreeMap order.
        let mut jumped = [false; MAX_VIEW_OBJECTS];
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
            copy_object_pcm(
                cursor,
                written,
                take,
                requested,
                self.scene_signature.object_element_ids(),
                &mut objects,
                &mut jumped,
            )?;
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
        // Mirror exactly what is about to be submitted rather than resolving the
        // OAMD state a second time. A parallel derivation would drift from the
        // audio under ramps, and these are already in the listener space the
        // scene view draws in.
        //
        // The timeline position doubles as the trail's clock. It is the only
        // monotonic presentation-time source here, and it jumps exactly when a
        // seek does — which is also when the trail has to be discarded, so the
        // two stay consistent for free.
        self.mirror.write(
            self.key,
            objects
                .values()
                .enumerate()
                .map(|(slot, render)| ObjectView {
                    element_id: render.element_id,
                    active: render.active,
                    position: render.position,
                    gain: render.gain,
                    jumped: jumped.get(slot).copied().unwrap_or(false),
                }),
            self.timeline_frame,
            self.sample_rate,
        );
        if let Some(pose) = self.pose.try_pose() {
            self.last_pose = pose;
        }
        // The scene mirror stays in world coordinates; only the system submission
        // is rotated into head space. Rotating both the room and avatar would double it.
        for object in objects.values_mut() {
            object.position = self.last_pose.rotate_listener(object.position);
        }
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
            let Some(offset_frames) = block_offset_at(&block, self.timeline_frame)? else {
                continue;
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

fn copy_object_pcm(
    cursor: &BlockCursor,
    destination_offset: usize,
    take: usize,
    requested: usize,
    element_ids: &[u64],
    renders: &mut BTreeMap<u64, DynamicObjectRender>,
    jumped: &mut [bool; MAX_VIEW_OBJECTS],
) -> Result<(), String> {
    let take_frames =
        u32::try_from(take).map_err(|_| "Scene object slice exceeds the u32 range".to_owned())?;
    let jump_end = cursor.offset_frames.saturating_add(take_frames);
    let source_start = usize::try_from(cursor.offset_frames)
        .map_err(|_| "Scene object offset exceeds usize".to_owned())?;
    let source_end = source_start
        .checked_add(take)
        .ok_or_else(|| "Scene object slice overflow".to_owned())?;
    let destination_end = destination_offset
        .checked_add(take)
        .ok_or_else(|| "Spatial Audio object slice overflow".to_owned())?;

    for object in cursor.block.objects() {
        // An instant update anywhere in this quantum belongs to the next
        // breadcrumb, not to this quantum: the mirror latches it, because a
        // quantum is roughly a quarter of the trail's sampling interval.
        if let Ok(slot) = element_ids.binary_search(&object.element_id())
            && let Some(flag) = jumped.get_mut(slot)
            && has_instant_update(
                &cursor.block,
                object.element_id(),
                cursor.offset_frames,
                jump_end,
            )
        {
            *flag = true;
        }
        let state = element_state_at(
            &cursor.block,
            object.element_id(),
            object.initial_state(),
            cursor.offset_frames,
        );
        let render = renders.entry(object.element_id()).or_insert_with(|| {
            let (active, position, gain) = listener_render_state(state);
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

#[cfg(test)]
mod tests {
    use crate::decoder::{
        SceneLfePcm, SceneMetadataUpdate, SceneObjectPcm, SpatialObjectState, SpatialPosition,
    };

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
