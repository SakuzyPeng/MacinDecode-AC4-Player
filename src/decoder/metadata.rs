//! Player-owned metadata timing. Each decoded frame becomes a self-contained
//! snapshot with continuation events before it enters the shared Scene FIFO.
#[cfg(macinrender_output)]
use super::SceneLfePcm;
#[cfg(any(macinrender_output, test))]
use super::SceneObjectPcm;
use super::{
    DecodedSceneBlock, FIELD_ACTIVE, FIELD_GAIN, FIELD_HEAD_TRACKING, FIELD_POSITION,
    SceneMetadataUpdate, SpatialObjectState, SpatialPosition,
};

#[cfg(any(feature = "decode", test))]
#[derive(Clone, Copy)]
struct EndState {
    current: SpatialObjectState,
    target: SpatialObjectState,
    remaining: [u32; 2],
}

#[cfg(any(feature = "decode", test))]
#[derive(Default)]
pub(super) struct SceneContinuity {
    end: Option<i64>,
    generation: u32,
    sample_rate: u32,
    presentation: (u32, Option<u32>),
    states: std::collections::BTreeMap<u64, EndState>,
}

#[cfg(any(feature = "decode", test))]
impl SceneContinuity {
    pub(super) fn clear(&mut self) {
        self.end = None;
        self.states.clear();
    }

    pub(super) fn normalize(&mut self, block: &mut DecodedSceneBlock) {
        if self.end != Some(block.start_frame)
            || self.generation != block.configuration_generation
            || self.sample_rate != block.sample_rate
            || self.presentation != (block.presentation_index, block.presentation_id)
        {
            self.states.clear();
        }
        let mut continuations = Vec::new();
        for object in &mut block.objects {
            self.resume(
                object.element_id,
                &mut object.initial_state,
                &mut continuations,
            );
        }
        if let Some(lfe) = &mut block.lfe {
            self.resume(lfe.element_id, &mut lfe.initial_state, &mut continuations);
        }
        // Existing offset-zero controls follow the inherited ramps, so an
        // explicit update overrides only its own field at the same sample.
        continuations.append(&mut block.metadata_updates);
        block.metadata_updates = continuations;

        let mut next = std::collections::BTreeMap::new();
        for (id, initial) in block
            .objects
            .iter()
            .map(|object| (object.element_id, object.initial_state))
            .chain(
                block
                    .lfe
                    .iter()
                    .map(|lfe| (lfe.element_id, lfe.initial_state)),
            )
        {
            let timeline = timeline_at(&block.metadata_updates, id, initial, block.duration_frames);
            if let (Some(current), Some(target)) =
                (timeline.at(block.duration_frames), timeline.state)
            {
                let remaining = [timeline.position, timeline.gain].map(|ramp| {
                    ramp.map_or(0, |ramp| {
                        ramp.start_frame
                            .saturating_add(ramp.duration_frames)
                            .saturating_sub(block.duration_frames)
                    })
                });
                next.insert(
                    id,
                    EndState {
                        current,
                        target,
                        remaining,
                    },
                );
            }
        }
        self.states = next;
        self.end = block
            .start_frame
            .checked_add(i64::from(block.duration_frames));
        self.generation = block.configuration_generation;
        self.sample_rate = block.sample_rate;
        self.presentation = (block.presentation_index, block.presentation_id);
    }

    fn resume(
        &self,
        id: u64,
        initial: &mut Option<SpatialObjectState>,
        updates: &mut Vec<SceneMetadataUpdate>,
    ) {
        let (Some(raw), Some(previous)) = (*initial, self.states.get(&id)) else {
            return;
        };
        *initial = Some(
            SpatialObjectState::new(
                raw.metadata_active(),
                previous.current.position(),
                previous.current.linear_gain(),
                raw.semantic_complete(),
            )
            .with_tracking(raw.tracking()),
        );
        let target = SpatialObjectState::new(
            raw.metadata_active(),
            previous.target.position(),
            previous.target.linear_gain(),
            raw.semantic_complete(),
        )
        .with_tracking(raw.tracking());
        for (field, remaining) in [FIELD_POSITION, FIELD_GAIN]
            .into_iter()
            .zip(previous.remaining)
        {
            if remaining != 0 {
                updates.push(SceneMetadataUpdate::new(id, 0, remaining, field, target));
            }
        }
    }
}

/// The element's state `offset_frames` into `block`, following every metadata
/// update up to that point and interpolating whichever ramp is still running.
pub(crate) fn element_state_at(
    block: &DecodedSceneBlock,
    element_id: u64,
    initial_state: Option<SpatialObjectState>,
    offset_frames: u32,
) -> Option<SpatialObjectState> {
    state_at_updates(
        block.metadata_updates(),
        element_id,
        initial_state,
        offset_frames,
    )
}

pub(crate) fn state_at_updates(
    updates: &[SceneMetadataUpdate],
    element_id: u64,
    initial_state: Option<SpatialObjectState>,
    offset_frames: u32,
) -> Option<SpatialObjectState> {
    timeline_at(updates, element_id, initial_state, offset_frames).at(offset_frames)
}

/// Continuous fields own their deadlines; discrete and ignored-field events
/// cannot overwrite the coordinates or gain target of a running ramp.
struct StateTimeline {
    state: Option<SpatialObjectState>,
    position: Option<MetadataRamp>,
    gain: Option<MetadataRamp>,
}

impl StateTimeline {
    fn at(&self, offset: u32) -> Option<SpatialObjectState> {
        let state = self.state?;
        let position = self.position.map(|ramp| ramp.state_at(offset));
        let gain = self.gain.map(|ramp| ramp.state_at(offset));
        Some(
            SpatialObjectState::new(
                state.metadata_active(),
                position.map_or(state.position(), SpatialObjectState::position),
                gain.map_or(state.linear_gain(), SpatialObjectState::linear_gain),
                state.semantic_complete()
                    && position.is_none_or(SpatialObjectState::semantic_complete)
                    && gain.is_none_or(SpatialObjectState::semantic_complete),
            )
            .with_tracking(state.tracking()),
        )
    }

    fn apply(&mut self, update: SceneMetadataUpdate) {
        let target = update.state();
        let Some(previous) = self.at(update.offset_frames()) else {
            self.state = Some(target);
            return;
        };
        let base = self.state.unwrap_or(previous);
        let fields = update.changed_fields();
        let make_ramp = || {
            (update.ramp_frames() != 0).then_some(MetadataRamp {
                start_frame: update.offset_frames(),
                duration_frames: update.ramp_frames(),
                from: previous,
                to: target,
            })
        };
        if fields & FIELD_POSITION != 0 {
            self.position = make_ramp();
        }
        if fields & FIELD_GAIN != 0 {
            self.gain = make_ramp();
        }
        self.state = Some(
            SpatialObjectState::new(
                if fields & FIELD_ACTIVE != 0 {
                    target.metadata_active()
                } else {
                    base.metadata_active()
                },
                if fields & FIELD_POSITION != 0 {
                    target.position()
                } else {
                    base.position()
                },
                if fields & FIELD_GAIN != 0 {
                    target.linear_gain()
                } else {
                    base.linear_gain()
                },
                target.semantic_complete(),
            )
            .with_tracking(if fields & FIELD_HEAD_TRACKING != 0 {
                target.tracking()
            } else {
                base.tracking()
            }),
        );
    }
}

fn timeline_at(
    updates: &[SceneMetadataUpdate],
    element_id: u64,
    initial_state: Option<SpatialObjectState>,
    offset: u32,
) -> StateTimeline {
    let mut timeline = StateTimeline {
        state: initial_state,
        position: None,
        gain: None,
    };
    for update in updates
        .iter()
        .copied()
        .filter(|update| update.element_id() == element_id)
    {
        if update.offset_frames() > offset {
            break;
        }
        timeline.apply(update);
    }
    timeline
}

/// Resume each running field after a clipped prefix. A later tracking-only
/// event must not hide an earlier position/gain target.
#[cfg(macinrender_output)]
pub(crate) fn remaining_ramps(
    block: &DecodedSceneBlock,
    id: u64,
    offset: u32,
) -> [Option<SceneMetadataUpdate>; 2] {
    let initial = block
        .objects()
        .iter()
        .find(|object| object.element_id() == id)
        .and_then(SceneObjectPcm::initial_state)
        .or_else(|| {
            block
                .lfe()
                .filter(|lfe| lfe.element_id() == id)
                .and_then(SceneLfePcm::initial_state)
        });
    let timeline = timeline_at(block.metadata_updates(), id, initial, offset);
    [
        (FIELD_POSITION, timeline.position),
        (FIELD_GAIN, timeline.gain),
    ]
    .map(|(field, ramp)| {
        let ramp = ramp?;
        let remaining = ramp
            .start_frame
            .saturating_add(ramp.duration_frames)
            .saturating_sub(offset);
        (remaining != 0 && ramp.start_frame < offset).then(|| {
            SceneMetadataUpdate::new(id, 0, remaining, field, timeline.state.unwrap_or(ramp.to))
        })
    })
}

#[derive(Clone, Copy)]
pub(crate) struct MetadataRamp {
    pub(crate) start_frame: u32,
    pub(crate) duration_frames: u32,
    pub(crate) from: SpatialObjectState,
    pub(crate) to: SpatialObjectState,
}

impl MetadataRamp {
    #[allow(
        clippy::cast_precision_loss,
        reason = "metadata ramp offsets become a normalized interpolation fraction"
    )]
    pub(crate) fn state_at(self, frame: u32) -> SpatialObjectState {
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

pub(crate) fn interpolate_state(
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
    .with_tracking(to.tracking())
}

fn lerp(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::ContentHeadTracking;

    fn state(x: f32, gain: f32, tracking: ContentHeadTracking) -> SpatialObjectState {
        SpatialObjectState::new(
            true,
            Some(SpatialPosition::new(x, 1.0, 0.0)),
            Some(gain),
            true,
        )
        .with_tracking(tracking)
    }
    fn block(
        start: i64,
        initial: Option<SpatialObjectState>,
        updates: Vec<SceneMetadataUpdate>,
    ) -> DecodedSceneBlock {
        DecodedSceneBlock::new(
            48_000,
            start,
            40,
            1,
            0,
            None,
            true,
            vec![SceneObjectPcm::new(7, initial, vec![0.01; 40])],
            None,
            updates,
        )
    }

    #[test]
    fn cross_frame_head_control_preserves_independent_position_and_gain_deadlines() {
        let mut continuity = SceneContinuity::default();
        let from = state(-1.0, 0.0, ContentHeadTracking::SceneRelative);
        let target = state(1.0, 1.0, ContentHeadTracking::SceneRelative);
        let mut first = block(
            0,
            Some(from),
            vec![
                SceneMetadataUpdate::new(7, 0, 100, FIELD_POSITION, target),
                SceneMetadataUpdate::new(7, 0, 200, FIELD_GAIN, target),
            ],
        );
        continuity.normalize(&mut first);
        let tracked = target.with_tracking(ContentHeadTracking::HeadRelative);
        let mut second = block(
            40,
            Some(tracked),
            vec![SceneMetadataUpdate::new(
                7,
                0,
                0,
                FIELD_HEAD_TRACKING,
                tracked,
            )],
        );
        continuity.normalize(&mut second);
        let actual = element_state_at(&second, 7, second.objects[0].initial_state, 10).unwrap();
        assert!(actual.position().unwrap().x().abs() < 0.000_001);
        assert!((actual.linear_gain().unwrap() - 0.25).abs() < 0.000_001);
        assert_eq!(actual.tracking(), ContentHeadTracking::HeadRelative);
        let mut third = block(80, Some(tracked), Vec::new());
        continuity.normalize(&mut third);
        let at_position_end =
            element_state_at(&third, 7, third.objects[0].initial_state, 20).unwrap();
        assert!((at_position_end.position().unwrap().x() - 1.0).abs() < 0.000_001);
        assert!((at_position_end.linear_gain().unwrap() - 0.5).abs() < 0.000_001);
        assert_eq!(third.metadata_updates[0].ramp_frames(), 20);
        assert_eq!(third.metadata_updates[1].ramp_frames(), 120);
    }

    #[test]
    fn reset_generation_gaps_and_missing_state_do_not_inherit_prior_ramps() {
        let target = state(1.0, 1.0, ContentHeadTracking::HeadRelative);
        for reset in 0..4 {
            let mut continuity = SceneContinuity::default();
            let mut first = block(
                0,
                Some(state(-1.0, 0.0, ContentHeadTracking::SceneRelative)),
                vec![SceneMetadataUpdate::new(
                    7,
                    0,
                    100,
                    FIELD_POSITION | FIELD_GAIN,
                    target,
                )],
            );
            continuity.normalize(&mut first);
            let mut next = block(40, Some(target), Vec::new());
            match reset {
                0 => continuity.clear(),
                1 => next.configuration_generation += 1,
                2 => next.start_frame += 1,
                _ => next.objects[0].initial_state = None,
            }
            continuity.normalize(&mut next);
            assert!(next.metadata_updates.is_empty());
            assert_eq!(
                next.objects[0].initial_state,
                if reset == 3 { None } else { Some(target) }
            );
        }
    }
}
