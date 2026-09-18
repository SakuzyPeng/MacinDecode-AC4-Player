//! Placing the canon in space: which object sounds which note, and where.
//!
//! The whole arrangement is built once, up front, into a list of [`Placement`]s
//! sorted by onset. [`super::DemoProgram::block_at`] then answers a block by
//! range-querying that list, which is what keeps it a pure function of its
//! start frame: nothing a block does can change what a later block renders.
//!
//! Sixteen object slots exist for the entire programme and never change their
//! element IDs, so [`crate::decoder::SceneSignature`] is constant from the
//! first block to the last and no phase boundary can force a stream rebuild.
//! Six of them carry the music -- three violins, the ground, two continuo --
//! and the other ten are a pool the phases spend on whatever they demonstrate.
//! A phase that gives every ringing note an object of its own also reclaims the
//! three violin slots, whose voices have handed their notes to the pool.
//!
//! Positions are ADM `[x, y, z]`: `x` to the right, `y` forward, `z` up. That
//! is what the `MacinRender` producer submits unchanged; the Windows path and
//! scene view both reach it through `backend::state::listener_render_state`.

use super::score;
use super::voice::{self, Timbre};
use crate::decoder::{
    ContentHeadTracking, DecodedSceneBlock, FIELD_ACTIVE, FIELD_GAIN, FIELD_HEAD_TRACKING,
    FIELD_POSITION, SceneLfePcm, SceneMetadataUpdate, SceneObjectPcm, SpatialObjectState,
    SpatialPosition,
};

/// Slots that exist for the whole programme.
///
/// Taken from the score rather than restated here: the generator proved the
/// phase map fits this many objects, and two copies of the number are two
/// chances for the proof to stop describing the code.
pub(crate) const SLOTS: usize = score::MAX_DYNAMIC_OBJECTS as usize;
/// Slots the music itself holds: violins one to three, ground, continuo pair.
pub(crate) const SPINE: usize = score::SPINE_SLOTS as usize;
const VIOLINS: usize = score::CANON_VOICE_SLOTS as usize;
/// Element IDs are `1..=SLOTS` for objects and this for the bed, all constant.
const LFE_ELEMENT_ID: u64 = 0x100;

/// Frames per Scene block. Nothing depends on the value; it is a plausible
/// AC-4 frame at 48 kHz and keeps the two-second FIFO a round number of blocks.
pub(crate) const BLOCK_FRAMES: u32 = 2048;

/// Peak amplitude one object may reach, -18 dBFS.
///
/// Eight voices sounding at once then sum to -9 dBFS in the worst case, which
/// leaves the renderer headroom it does not have to claw back. The programme's
/// real peak is asserted in this module's tests rather than assumed.
const OBJECT_PEAK: f32 = 0.125;
/// How far out the canon voices circle, and how far a fixed part sits forward.
const ORBIT_RADIUS: f32 = 0.8;
/// One revolution every four ground statements, so the canon's eight-beat delay
/// reads as a steady third of a turn between neighbouring voices.
const ORBIT_STATEMENTS: f32 = 4.0;

/// Semitones above D for each degree of D major.
const SCALE: [u8; 7] = [0, 2, 4, 5, 7, 9, 11];
const TONIC_CLASS: u8 = 2;

/// How an object's position is derived while a note sounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Motion {
    /// Held still: a ground that anchors, or a keyboard key that is struck and
    /// then left exactly where it was struck.
    Fixed([f32; 3]),
    /// Circling at a fixed bearing offset, at the height its pitch asks for.
    Orbit { turns: f32, elevation: f32 },
}

impl Motion {
    pub(crate) fn at(self, seconds: f32) -> [f32; 3] {
        match self {
            Self::Fixed(position) => position,
            Self::Orbit { turns, elevation } => {
                let angle = (turns + seconds / seconds_per_statement() / ORBIT_STATEMENTS)
                    * std::f32::consts::TAU;
                [
                    ORBIT_RADIUS * angle.sin(),
                    ORBIT_RADIUS * angle.cos(),
                    elevation,
                ]
            }
        }
    }
}

/// One note, already assigned to a slot and a place.
#[derive(Debug, Clone)]
pub(crate) struct Placement {
    pub(crate) slot: usize,
    pub(crate) onset: i64,
    /// When the slot is free again: the note's onset plus its timbre's tail.
    pub(crate) release: i64,
    pub(crate) pitch: u8,
    pub(crate) timbre: usize,
    /// Amplitude the synthesiser renders at. Zero is a deliberate case: an
    /// object carrying full metadata gain and no audio is exactly what the
    /// scene's split footprint exists to expose.
    pub(crate) level: f32,
    /// Linear gain the metadata declares, which is not the same thing.
    pub(crate) gain: f32,
    pub(crate) ramp_frames: u32,
    pub(crate) metadata_active: bool,
    pub(crate) motion: Motion,
    pub(crate) tracking: ContentHeadTracking,
}

impl Placement {
    fn new(slot: usize, onset: i64, pitch: u8, timbre: usize, release: i64) -> Self {
        Self {
            slot,
            onset,
            release,
            pitch,
            timbre,
            level: OBJECT_PEAK,
            gain: 1.0,
            ramp_frames: 0,
            metadata_active: true,
            motion: Motion::Fixed([0.0, ORBIT_RADIUS, 0.0]),
            tracking: ContentHeadTracking::SceneRelative,
        }
    }
}

/// Which timbre a slot speaks with. Indices into [`timbres`].
pub(crate) mod timbre {
    pub(crate) const PLUCKED: usize = 0;
    pub(crate) const VIBRAPHONE: usize = 1;
    pub(crate) const GLOCKENSPIEL: usize = 2;
    pub(crate) const GROUND: usize = 3;
    pub(crate) const CONTINUO: usize = 4;
    pub(crate) const HARPSICHORD: usize = 5;
    pub(crate) const TIMPANI: usize = 6;
}

/// The seven voices the demo speaks with.
///
/// Struck and plucked throughout: their transients localise, where a sustained
/// timbre would smear across the very thing the scene is trying to show. The
/// modal figures are the textbook ones for each body -- a marimba-like bar's
/// 1 : 3.9 : 9.2, a bell's Risset set -- rather than anything tuned by ear.
pub(crate) fn timbres() -> Vec<Timbre> {
    vec![
        Timbre::plucked(0.002, 1.4, 12),
        Timbre::modal(
            0.003,
            &[(1.0, 1.0, 3.0), (4.0, 0.18, 1.1), (10.7, 0.06, 0.5)],
        ),
        Timbre::modal(
            0.002,
            &[
                (1.0, 1.0, 1.2),
                (2.7, 0.5, 0.5),
                (5.4, 0.2, 0.25),
                (8.9, 0.08, 0.12),
            ],
        ),
        Timbre::plucked(0.003, 1.8, 10),
        Timbre::plucked(0.002, 0.9, 8),
        // The keyboard phase's timbre is pinned by the object budget rather
        // than by taste: a longer decay needs more pool slots than exist.
        Timbre::plucked(0.002, score::KEYBOARD_TAU_SECONDS, 10),
        Timbre::modal(
            0.004,
            &[
                (1.0, 1.0, 0.28),
                (1.59, 0.35, 0.18),
                (2.14, 0.18, 0.12),
                (2.30, 0.10, 0.09),
            ],
        ),
    ]
}

/// Seconds to whole frames, rounding toward zero.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "durations here are decay times of a few seconds at an audio rate, \
              so the product is millions of frames rather than billions"
)]
pub(crate) fn frames_of(seconds: f32, sample_rate: u32) -> i64 {
    (f64::from(seconds) * f64::from(sample_rate)) as i64
}

/// The bearing a canon voice starts from, as a fraction of a turn.
fn turn_of(voice: usize) -> f32 {
    f32::from(u8::try_from(voice).unwrap_or(0)) / f32::from(u8::try_from(VIOLINS).unwrap_or(3))
}

/// Ticks to frames, exactly and by integer arithmetic.
///
/// 72 bpm at 48 kHz is 833.33 frames to the tick, so this cannot be a whole
/// number. Rounding once here, the same way every time, keeps the map from
/// score time to frames a fixed function rather than an accumulating drift.
pub(crate) fn frame_of(tick: u64, sample_rate: u32) -> i64 {
    let numerator = tick * u64::from(sample_rate) * 60;
    let denominator = u64::from(score::TEMPO_BPM) * u64::from(score::TICKS_PER_BEAT);
    i64::try_from(numerator / denominator).unwrap_or(i64::MAX)
}

/// The pitch `degrees` scale steps above `pitch` in D major.
fn diatonic_above(pitch: u8, degrees: usize) -> u8 {
    let class = (pitch + 12 - TONIC_CLASS) % 12;
    let degree = SCALE
        .iter()
        .position(|&step| step == class % 12)
        .unwrap_or(0);
    let target = degree + degrees;
    let octaves = u8::try_from(target / 7).unwrap_or(0);
    pitch - class + SCALE[target % 7] + 12 * octaves
}

/// Height from pitch: the correspondence a listener already expects.
///
/// Legible on the stage, and only coarsely audible -- elevation cues come from
/// the pinna, so how well this reads at all is exactly what changes when a
/// different SOFA profile is loaded. That is the point of putting it here.
fn pitch_elevation(pitch: u8) -> f32 {
    let (low, high) = score::PITCH_RANGE;
    let span = f32::from(high - low).max(1.0);
    (f32::from(pitch - low) / span)
        .mul_add(1.4, -0.7)
        .clamp(-0.7, 0.7)
}

/// Where a key sits on a keyboard laid out in front of the listener.
///
/// The black keys really are set back, so they take the depth axis. Without
/// that the mapping is a scale rather than a keyboard.
fn keyboard_position(pitch: u8) -> [f32; 3] {
    let (low, high) = score::PITCH_RANGE;
    let span = f32::from(high - low).max(1.0);
    let across = (f32::from(pitch - low) / span).mul_add(1.8, -0.9);
    // `y` is forward, so a black key takes the *larger* value: on a keyboard
    // they sit further from the player, not nearer.
    let black = matches!(pitch % 12, 1 | 3 | 6 | 8 | 10);
    [
        across,
        if black { 0.85 } else { 0.6 },
        pitch_elevation(pitch) * 0.5,
    ]
}

pub(crate) struct Arrangement {
    pub(crate) placements: Vec<Placement>,
    pub(crate) duration_frames: u64,
    /// Times a pool slot had to be taken back before its note had faded.
    ///
    /// The phase map is sized so this stays zero, and a test asserts it. It is
    /// counted rather than refused because the cap has to hold by construction:
    /// if a tempo or a decay changes, the demo truncates one tail and the test
    /// goes red, instead of overrunning the sixteen objects it promised.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "the counter exists so the cap holds by construction; the test \
                      asserting it stays zero is the only thing that needs to read it"
        )
    )]
    pub(crate) stolen: usize,
}

/// Build the whole programme: every note, on its slot, in its place.
pub(crate) fn arrange(sample_rate: u32, timbres: &[Timbre]) -> Arrangement {
    let mut placements = Vec::new();
    // How long a slot is held before it may be handed on. This is the -20 dB
    // guard the generator sizes the pool with, not the -36 dB floor the
    // synthesiser stops evaluating at: the first is where truncating a tail
    // stops being audible, the second is where the scene stops drawing it.
    // Sizing the pool with one and holding slots by the other is how a pool
    // that the arithmetic says fits runs out anyway.
    let hold_of = |timbre: usize, onset: i64| -> i64 {
        onset
            + frames_of(
                timbres[timbre].longest_tau() * score::STEAL_GUARD_TAUS,
                sample_rate,
            )
    };
    let keyboard_phase = score::PHASES
        .iter()
        .find(|phase| phase.per_note())
        .copied()
        .expect("the programme has a per-note phase");

    // The three violins: one line, three entries, three bearings.
    for voice in 0..VIOLINS {
        let entry = u64::from(score::CANON_ENTRIES[voice]);
        let line = score::CANON_LINE.iter().map(|note| (entry, note));
        let tail = score::CANON_TAILS[voice]
            .iter()
            .map(|note| (entry + u64::from(score::CANON_LINE_TICKS), note));
        let timbre = [timbre::PLUCKED, timbre::VIBRAPHONE, timbre::GLOCKENSPIEL][voice];
        for (base, note) in line.chain(tail) {
            let tick = base + u64::from(note.onset());
            // The keyboard phase empties these slots into the pool.
            if within(tick, keyboard_phase) {
                continue;
            }
            let onset = frame_of(tick, sample_rate);
            let mut placement =
                Placement::new(voice, onset, note.pitch(), timbre, hold_of(timbre, onset));
            placement.motion = Motion::Orbit {
                turns: turn_of(voice),
                elevation: pitch_elevation(note.pitch()),
            };
            placements.push(placement);
        }
    }

    ground_and_continuo(&mut placements, sample_rate, &hold_of);

    let mut stolen = 0;
    for phase in score::PHASES {
        let pool = SPINE..SPINE + usize::try_from(phase.pool_slots()).unwrap_or(0);
        match phase.name() {
            "reference" => reference(&mut placements, sample_rate, &hold_of, *phase),
            "keyboard" => stolen += keyboard(&mut placements, sample_rate, &hold_of, *phase),
            "gain-ring" => gain_ring(&mut placements, sample_rate, &hold_of, *phase, pool),
            _ => {}
        }
    }

    // An object carrying a line is one instrument playing it, so each note is
    // damped as the next arrives rather than left to ring over it. On a pool
    // slot the allocator already waits out the full tail, so this clamps
    // nothing there -- which is exactly the difference between the two.
    placements.sort_by_key(|placement| (placement.slot, placement.onset));
    let damp = frames_of(voice::DAMP_SECONDS, sample_rate);
    for index in 1..placements.len() {
        if placements[index].slot == placements[index - 1].slot {
            let limit = placements[index].onset + damp;
            placements[index - 1].release = placements[index - 1].release.min(limit);
        }
    }

    placements.sort_by_key(|placement| (placement.onset, placement.slot));
    // The programme runs until the last note has finished ringing, not until
    // the last bar line. Ending on the bar line cuts the final cadence off
    // mid-decay, which is audible as a click and is not how the piece ends.
    let notated = frame_of(
        u64::from(score::BARS) * u64::from(score::TICKS_PER_BEAT * score::BEATS_PER_BAR),
        sample_rate,
    );
    let ringing = placements
        .iter()
        .map(|placement| placement.release)
        .max()
        .unwrap_or(notated);
    let duration_frames = u64::try_from(notated.max(ringing)).unwrap_or(0);
    Arrangement {
        placements,
        duration_frames,
        stolen,
    }
}

/// The ground bass, its figured-bass realisation, and the closing bar.
///
/// The realisation is the plain one: the diatonic third and fifth above each
/// ground note, an octave up. Pachelbel wrote a figured bass for a player to
/// fill in, so something has to fill it in; taking the textbook stack keeps
/// that from becoming a place where the demo quietly composes.
fn ground_and_continuo(
    placements: &mut Vec<Placement>,
    sample_rate: u32,
    hold_of: &impl Fn(usize, i64) -> i64,
) {
    let statement_ticks = u64::from(score::TICKS_PER_STATEMENT);
    // The ground and a plain triadic realisation of its figured bass, then the
    // closing bar. The coda is one note and easy to forget, and forgetting it
    // leaves the piece ending on violin tails over no bass at all.
    let closing = u64::from(score::GROUND_REPEATS) * statement_ticks;
    let ground_notes = (0..u64::from(score::GROUND_REPEATS))
        .flat_map(|statement| {
            score::GROUND
                .iter()
                .map(move |note| statement * statement_ticks + u64::from(note.onset()))
                .zip(score::GROUND)
        })
        .chain(
            score::GROUND_CODA
                .iter()
                .map(|note| (closing + u64::from(note.onset()), note)),
        );
    for (tick, note) in ground_notes {
        let onset = frame_of(tick, sample_rate);
        let mut ground = Placement::new(
            3,
            onset,
            note.pitch(),
            timbre::GROUND,
            hold_of(timbre::GROUND, onset),
        );
        ground.motion = Motion::Fixed([0.0, ORBIT_RADIUS, -0.35]);
        placements.push(ground);
        for (index, degrees) in [2usize, 4].into_iter().enumerate() {
            let mut chord = Placement::new(
                SPINE - 2 + index,
                onset,
                diatonic_above(note.pitch(), degrees) + 12,
                timbre::CONTINUO,
                hold_of(timbre::CONTINUO, onset),
            );
            chord.level = OBJECT_PEAK * 0.55;
            chord.motion = Motion::Fixed([if index == 0 { -0.55 } else { 0.55 }, 0.5, -0.15]);
            placements.push(chord);
        }
    }
}

fn within(tick: u64, phase: super::Phase) -> bool {
    let (first, last) = phase.span();
    (u64::from(first)..u64::from(last)).contains(&tick)
}

/// Violin one, doubled an octave up, twice: once head locked, once not.
///
/// Same material and same timbre, so the reference frame is the only thing
/// that differs -- which is the one comparison this player exists to make and
/// the one readers reliably get backwards.
fn reference(
    placements: &mut Vec<Placement>,
    sample_rate: u32,
    hold_of: &impl Fn(usize, i64) -> i64,
    phase: super::Phase,
) {
    let entry = u64::from(score::CANON_ENTRIES[0]);
    for note in score::CANON_LINE {
        let tick = entry + u64::from(note.onset());
        if !within(tick, phase) {
            continue;
        }
        let onset = frame_of(tick, sample_rate);
        for (index, tracking) in [
            ContentHeadTracking::HeadRelative,
            ContentHeadTracking::SceneRelative,
        ]
        .into_iter()
        .enumerate()
        {
            let mut twin = Placement::new(
                SPINE + index,
                onset,
                note.pitch() + 12,
                timbre::PLUCKED,
                hold_of(timbre::PLUCKED, onset),
            );
            twin.level = OBJECT_PEAK * 0.6;
            twin.tracking = tracking;
            let side = if index == 0 { -0.7 } else { 0.7 };
            twin.motion = Motion::Fixed([side, 0.55, 0.2]);
            placements.push(twin);
        }
    }
}

/// Every ringing note gets an object of its own, placed by its pitch.
///
/// The pool is the ten spare slots plus the three the violins vacated. Slots
/// are handed out to whichever has been free longest, and a slot is only free
/// once its previous note has passed the silence floor -- so a tail is never
/// dragged to a new position, which is the failure this phase would otherwise
/// put on display.
fn keyboard(
    placements: &mut Vec<Placement>,
    sample_rate: u32,
    hold_of: &impl Fn(usize, i64) -> i64,
    phase: super::Phase,
) -> usize {
    let mut notes = Vec::new();
    for voice in 0..VIOLINS {
        let entry = u64::from(score::CANON_ENTRIES[voice]);
        for note in score::CANON_LINE {
            let tick = entry + u64::from(note.onset());
            if within(tick, phase) {
                notes.push((frame_of(tick, sample_rate), note.pitch()));
            }
        }
    }
    notes.sort_unstable();

    let pool: Vec<usize> = (0..VIOLINS).chain(SPINE..SLOTS).collect();
    let mut free_at = vec![i64::MIN; pool.len()];
    let mut stolen = 0;
    for (onset, pitch) in notes {
        // Longest free wins; if none is free the oldest is taken back, which
        // costs one truncated tail rather than a seventeenth object.
        let index = free_at
            .iter()
            .enumerate()
            .min_by_key(|&(_, &free)| free)
            .map(|(index, _)| index)
            .expect("the pool holds slots");
        if free_at[index] > onset {
            stolen += 1;
        }
        let release = hold_of(timbre::HARPSICHORD, onset);
        free_at[index] = release;
        let mut key = Placement::new(pool[index], onset, pitch, timbre::HARPSICHORD, release);
        key.level = OBJECT_PEAK * 0.8;
        key.motion = Motion::Fixed(keyboard_position(pitch));
        placements.push(key);
    }
    stolen
}

/// Three things the metadata can say that the audio cannot.
///
/// One object holds full gain and sounds nothing, which is what keeps the gain
/// ring on the floor while the cube recedes. One switches `metadata_active` off
/// and on. Two ramp their gain, so the ramp interpolation every consumer shares
/// has something to interpolate.
fn gain_ring(
    placements: &mut Vec<Placement>,
    sample_rate: u32,
    hold_of: &impl Fn(usize, i64) -> i64,
    phase: super::Phase,
    pool: std::ops::Range<usize>,
) {
    let slots: Vec<usize> = pool.collect();
    if slots.len() < 4 {
        return;
    }
    let entry = u64::from(score::CANON_ENTRIES[1]);
    let mut index = 0usize;
    for note in score::CANON_LINE {
        let tick = entry + u64::from(note.onset());
        if !within(tick, phase) {
            continue;
        }
        let onset = frame_of(tick, sample_rate);
        let release = hold_of(timbre::VIBRAPHONE, onset);

        // Full gain, no audio: persistently silent by content, not by volume.
        let mut mute = Placement::new(slots[0], onset, note.pitch(), timbre::VIBRAPHONE, release);
        mute.level = 0.0;
        mute.motion = Motion::Fixed([-0.4, 0.75, 0.45]);
        placements.push(mute);

        // Every fourth note is declared inactive rather than merely quiet.
        let mut toggled =
            Placement::new(slots[1], onset, note.pitch(), timbre::GLOCKENSPIEL, release);
        toggled.level = OBJECT_PEAK * 0.5;
        toggled.metadata_active = !index.is_multiple_of(4);
        toggled.motion = Motion::Fixed([0.4, 0.75, 0.45]);
        placements.push(toggled);

        // A pair whose gain ramps rather than steps.
        for (offset, side) in [(0usize, -0.8f32), (1, 0.8)] {
            let mut ramped = Placement::new(
                slots[2 + offset],
                onset,
                note.pitch() - 12,
                timbre::CONTINUO,
                hold_of(timbre::CONTINUO, onset),
            );
            ramped.level = OBJECT_PEAK * 0.5;
            ramped.gain = if (index + offset).is_multiple_of(2) {
                1.0
            } else {
                0.25
            };
            ramped.ramp_frames = sample_rate / 4;
            ramped.motion = Motion::Fixed([side, 0.2, -0.3]);
            placements.push(ramped);
        }
        index += 1;
    }
}

/// The bed: one timpani stroke on the first beat of every bar.
pub(crate) fn lfe_onsets(sample_rate: u32) -> Vec<(i64, u8)> {
    let bar = u64::from(score::TICKS_PER_BEAT * score::BEATS_PER_BAR);
    (0..u64::from(score::BARS))
        .map(|index| {
            // Beat one of each bar is the ground's first or fifth note.
            let half = usize::try_from(index % 2).unwrap_or(0) * 4;
            let pitch = score::GROUND[half].pitch().saturating_sub(12);
            (frame_of(index * bar, sample_rate), pitch)
        })
        .collect()
}

/// The element ID a slot carries for the whole programme.
pub(crate) const fn element_id(slot: usize) -> u64 {
    slot as u64 + 1
}

/// Resolve a slot's declared state at one instant.
pub(crate) fn state_of(placement: &Placement, seconds: f32) -> SpatialObjectState {
    let [x, y, z] = placement.motion.at(seconds);
    SpatialObjectState::new(
        placement.metadata_active,
        Some(SpatialPosition::new(x, y, z)),
        Some(placement.gain),
        true,
    )
    .with_tracking(placement.tracking)
}

/// An idle slot: present, silent, and saying so.
pub(crate) fn idle_state() -> SpatialObjectState {
    SpatialObjectState::new(
        false,
        Some(SpatialPosition::new(0.0, 0.0, 0.0)),
        Some(0.0),
        true,
    )
    .with_tracking(ContentHeadTracking::SceneRelative)
}

pub(crate) fn update(
    element: u64,
    offset_frames: u32,
    ramp_frames: u32,
    state: SpatialObjectState,
) -> SceneMetadataUpdate {
    SceneMetadataUpdate::new(
        element,
        offset_frames,
        ramp_frames,
        FIELD_ACTIVE | FIELD_GAIN | FIELD_POSITION | FIELD_HEAD_TRACKING,
        state,
    )
}

pub(crate) fn object(slot: usize, state: SpatialObjectState, samples: Vec<f32>) -> SceneObjectPcm {
    SceneObjectPcm::new(element_id(slot), Some(state), samples)
}

pub(crate) fn lfe(state: SpatialObjectState, samples: Vec<f32>) -> SceneLfePcm {
    SceneLfePcm::new(LFE_ELEMENT_ID, Some(state), samples)
}

pub(crate) fn block(
    sample_rate: u32,
    start_frame: i64,
    duration_frames: u32,
    objects: Vec<SceneObjectPcm>,
    bed: Option<SceneLfePcm>,
    updates: Vec<SceneMetadataUpdate>,
) -> DecodedSceneBlock {
    DecodedSceneBlock::new(
        sample_rate,
        start_frame,
        duration_frames,
        0,
        0,
        Some(0),
        true,
        objects,
        bed,
        updates,
    )
}

/// Seconds one ground statement lasts, which is the demo's phase unit.
fn seconds_per_statement() -> f32 {
    60.0 * f32::from(u8::try_from(score::BEATS_PER_BAR * 2).unwrap_or(8))
        / f32::from(u8::try_from(score::TEMPO_BPM).unwrap_or(72))
}

/// The bed's declared state: always on, at unity, with no position of its own.
pub(crate) fn bed_state() -> SpatialObjectState {
    SpatialObjectState::new(true, None, Some(1.0), true)
}
