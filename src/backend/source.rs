use std::collections::BTreeMap;

use macindecode_windows_spatial_audio::{DynamicObjectRender, RenderQuantum, SpatialSource};

use crate::decoder::{
    DecodedSceneBlock, SceneObjectPcm, SceneQueueReader, SpatialObjectState, SpatialPosition,
};

pub(super) struct SceneRenderSource {
    reader: SceneQueueReader,
    sample_rate: u32,
    has_lfe: bool,
    timeline_frame: i64,
    current: Option<BlockCursor>,
}

impl SceneRenderSource {
    pub(super) const fn new(reader: SceneQueueReader, sample_rate: u32, has_lfe: bool) -> Self {
        Self {
            reader,
            sample_rate,
            has_lfe,
            timeline_frame: 0,
            current: None,
        }
    }

    fn render_quantum(&mut self, frame_count: u32) -> Result<RenderQuantum, String> {
        let requested = usize::try_from(frame_count)
            .map_err(|_| "Windows Spatial Audio frame count exceeds usize".to_owned())?;
        let mut objects = BTreeMap::<u64, DynamicObjectRender>::new();
        let mut lfe = self.has_lfe.then(|| vec![0.0; requested]);
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
            lfe,
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
            validate_block(&block, self.sample_rate, self.has_lfe)?;
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
    stream_has_lfe: bool,
) -> Result<(), String> {
    if block.sample_rate() != sample_rate {
        return Err(format!(
            "Scene sample rate changed from {sample_rate} to {} Hz",
            block.sample_rate()
        ));
    }
    let expected = usize::try_from(block.duration_frames())
        .map_err(|_| "Scene block duration exceeds usize".to_owned())?;
    for object in block.objects() {
        if object.samples().len() != expected {
            return Err(format!(
                "Scene object {} PCM length does not match its block",
                object.element_id()
            ));
        }
    }
    if let Some(component) = block.lfe()
        && (!stream_has_lfe || component.samples().len() != expected)
    {
        return Err("Scene LFE layout changed after Spatial Audio activation".to_owned());
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
        let state = object_state_at(&cursor.block, object, cursor.offset_frames);
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
    destination: Option<&mut Vec<f32>>,
) -> Result<(), String> {
    let (Some(source), Some(destination)) = (cursor.block.lfe(), destination) else {
        return Ok(());
    };
    let source_start = usize::try_from(cursor.offset_frames)
        .map_err(|_| "Scene LFE offset exceeds usize".to_owned())?;
    let source_end = source_start
        .checked_add(take)
        .ok_or_else(|| "Scene LFE slice overflow".to_owned())?;
    let destination_end = destination_offset
        .checked_add(take)
        .ok_or_else(|| "Spatial Audio LFE slice overflow".to_owned())?;
    destination[destination_offset..destination_end]
        .copy_from_slice(&source.samples()[source_start..source_end]);
    Ok(())
}

fn object_state_at(
    block: &DecodedSceneBlock,
    object: &SceneObjectPcm,
    offset_frames: u32,
) -> Option<SpatialObjectState> {
    let mut state = object.initial_state()?;
    let mut ramp = None;
    for update in block
        .metadata_updates()
        .iter()
        .copied()
        .filter(|update| update.element_id() == object.element_id())
    {
        if update.offset_frames() > offset_frames {
            break;
        }
        let from = ramp.map_or(state, |active: MetadataRamp| {
            active.state_at(update.offset_frames())
        });
        if update.ramp_frames() == 0 {
            state = update.state();
            ramp = None;
        } else {
            state = update.state();
            ramp = Some(MetadataRamp {
                start_frame: update.offset_frames(),
                duration_frames: update.ramp_frames(),
                from,
                to: update.state(),
            });
        }
    }
    ramp.map_or(Some(state), |active| Some(active.state_at(offset_frames)))
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
