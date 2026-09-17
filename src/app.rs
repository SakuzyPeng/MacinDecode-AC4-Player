use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Align, Align2, Color32, Layout, RichText, Stroke};
use serde::{Deserialize, Serialize};

use crate::backend::{
    OutputDeviceSelection, OutputPhase, OutputSettings, OutputSnapshot, OutputStreamConfig,
    SpatialBackendKind, SpatialOutputController, SpeakerLayout,
};
use crate::bitstream_ui::{self, BitstreamAction};
use crate::decoder::{
    DecodeMetrics, DecodePhase, DecoderController, DecoderSnapshot, PREBUFFER_MILLISECONDS,
};
use crate::inspection::InspectionController;
use crate::library::{LibraryController, Mutation};
use crate::media::MediaSource;
use crate::model::SelectedSource;
use crate::playlist::{
    BrowseState, EntryId, PlaybackCursor, PlaybackMode, PlaylistId, SavedBrowse, SessionState,
};
use crate::playlist_ui;
use crate::preferences::{AppPreferences, DataDirectory};
mod library_integration;
mod visual_settings;
use crate::scene3d;
use crate::theme;

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent UI toggles and pointer interaction flags are not one state machine"
)]
pub struct PlayerApp {
    pub smoke: Option<crate::install_check::WindowSmoke>,
    about: crate::licenses::Window,
    sofa: crate::file_catalog::Catalog,
    hptf: crate::file_catalog::Catalog,
    skins: crate::skin_library::Catalog,
    library: LibraryController,
    browse: BrowseState,
    cursor: Option<PlaybackCursor>,
    playlist_ui: playlist_ui::State,
    file_picker: Option<library_integration::FilePick>,
    inspection_media: Option<MediaSource>,
    preferences: AppPreferences,
    preferences_observed: AppPreferences,
    preferences_dirty_at: Option<Instant>,
    browse_observed: SavedBrowse,
    browse_dirty_at: Option<Instant>,
    last_session_save: Instant,
    last_saved_intent: bool,
    checkpoint: SessionState,
    resume: Option<SessionState>,
    automatic_candidate: bool,
    failed_candidates: std::collections::HashSet<EntryId>,
    marked_failure_key: Option<(u64, u64)>,
    media_source: Option<MediaSource>,
    inspection: InspectionController,
    decoder: DecoderController,
    decoder_revision: u64,
    output: SpatialOutputController,
    output_revision: u64,
    backend: SpatialBackendKind,
    status: StatusLine,
    timeline_preview: f32,
    timeline_dragging: bool,
    playback_restore_pending: bool,
    playback_intent: bool,
    playback_mode: PlaybackMode,
    shuffle_history: Vec<EntryId>,
    shuffle_state: u64,
    automatic_reconfigure_guard: Option<(u64, u64)>,
    waiting_for_device: Option<DeviceWait>,
    volume: f32,
    muted: bool,
    bitstream_details_open: bool,
    diagnostics_open: bool,
    output_settings_open: bool,
    visual_settings_open: bool,
    pending_output_change: Option<OutputSettings>,
    audio_settings_error: Option<String>,
    sofa_picker: Option<Pin<Box<dyn Future<Output = Option<rfd::FileHandle>>>>>,
    hptf_picker: Option<Pin<Box<dyn Future<Output = Option<rfd::FileHandle>>>>>,
    /// The drawn profile, read once per path and design rate.
    hptf_drawing: Option<HptfDrawing>,
    /// The profile under the pointer, cached the same way so that sweeping the
    /// folder re-reads a file only when the row changes.
    hptf_ghost: Option<HptfDrawing>,
    skin_picker: Option<Pin<Box<dyn Future<Output = Option<rfd::FileHandle>>>>>,
    camera: scene3d::camera::Camera,
    /// Whether zero-based LFE / one-based dynamic-object numbers are printed
    /// on every element face.
    object_numbers_visible: bool,
    /// Whether the measured loudness channels are drawn. Measurement always
    /// runs on the audio side; this decides only what the picture carries.
    object_loudness_visible: bool,
    /// Whether sustained silence dims objects, independently of loudness display.
    fade_silent_objects: bool,
    /// Whether the meter bank occupies a strip to the right of the scene.
    meter_bank_open: bool,
    /// Which quantity that strip reads out.
    meter_readout: MeterReadout,
    /// Meter ballistics, the one part of the loudness picture that depends on
    /// time rather than on the current frame.
    object_meters: ObjectMeters,
    /// Listener pose. Head tracking will drive the two angles; until then the
    /// listener faces the room's front.
    figure: scene3d::figure::Figure,
    /// Reused across frames so rebuilding the scene does not reallocate.
    scene_mesh: scene3d::mesh::MeshBuilder,
    /// False when eframe is not on the wgpu backend. The stage then draws
    /// nothing at all but its frame, which is why it is checked rather than
    /// assumed.
    scene_renderer_ready: bool,
    /// Set by [`eframe::App::ui`] and taken by [`eframe::App::logic`] to choose
    /// the visible or hidden playback polling cadence without duplicating
    /// eframe's viewport visibility rules.
    stage_drawn: bool,
}

const OUTPUT_DEVICE_STORAGE_KEY: &str = "preferred-output-device-v1";
const OUTPUT_SETTINGS_STORAGE_KEY: &str = "spatial-output-settings-v1";
const SCENE_CAMERA_STORAGE_KEY: &str = "scene-camera-v1";

struct DialogWake(egui::Context);
impl Wake for DialogWake {
    fn wake(self: Arc<Self>) {
        self.0.request_repaint();
    }
}

/// Visible dynamic-object numbers describe the fixed scene slots, not Core's
/// lifetime-stable element IDs. LFE owns zero separately, so the dynamic range
/// remains 1..=20 whether or not the presentation carries LFE.
fn object_display_number(slot: usize) -> u64 {
    u64::try_from(slot.saturating_add(1)).unwrap_or(u64::MAX)
}

/// Per-slot meter ballistics over the mirror's energy bins.
///
/// The mirror stores energy rather than a meter reading on purpose, so the
/// ballistics live here, on the side that knows how much wall time has passed
/// since it last drew. That is also what makes the meter fall when the audio
/// stops: `SceneViewMirror::write` holds its last frame when a publication
/// carries no objects, so a decaying value stored there would freeze at
/// whatever it last was, while a value chased from here keeps releasing.
#[derive(Debug, Default)]
struct ObjectMeters {
    /// Linear level per slot, after attack and release.
    level: [f32; crate::scene_view::MAX_VIEW_OBJECTS],
    /// The momentary loudness of the same slot as a linear RMS: the whole
    /// 400 ms ring, with no ballistics at all.
    ///
    /// Ballistics are a feel, and BS.1770's momentary loudness is a
    /// measurement — putting an attack and a release on it would make it a
    /// different quantity that merely resembled the standard's. The bank draws
    /// this one exactly as measured and lets the fast meter above be the one
    /// that moves.
    momentary: [f32; crate::scene_view::MAX_VIEW_OBJECTS],
    /// How long each slot has been continuously below the silence floor, in
    /// seconds. Reset to zero the instant a level comes back, because coming
    /// back is what the view exists to show: only the disappearance is allowed
    /// to take time.
    silent_for: [f32; crate::scene_view::MAX_VIEW_OBJECTS],
    /// Peak markers for both readings, kept in step whichever one is on screen
    /// so that switching the bank's unit does not make a marker lurch.
    peaks: [[PeakHold; crate::scene_view::MAX_VIEW_OBJECTS]; 2],
    /// Seconds of clip indication still owed to each slot.
    clip_held: [f32; crate::scene_view::MAX_VIEW_OBJECTS],
    /// The playback these levels belong to. A superseded one starts silent
    /// rather than releasing from the previous stream's last reading.
    key: Option<crate::decoder::PlaybackKey>,
    last_advanced: Option<Instant>,
}

/// Which quantity the meter bank reads out.
///
/// Both are measured from the same K-weighted energy the mirror carries; they
/// differ in how much of it they take, and that is not a detail. The fast meter
/// answers "is this object making a sound right now", the momentary one answers
/// "how loud is it, by the standard's definition" — and the second question has
/// an answer that other tools can be held to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MeterReadout {
    /// The `METER_WINDOW_MILLISECONDS` meter with attack and release, the same
    /// reading the scene draws. Reported as dBFS.
    #[default]
    Fast,
    /// BS.1770 momentary loudness: the whole 400 ms window, unballistic.
    /// Reported as LUFS-M.
    Momentary,
}

impl MeterReadout {
    /// The unit this reads in, which is also the button's whole label.
    const fn unit(self) -> &'static str {
        match self {
            Self::Fast => "dBFS",
            Self::Momentary => "LUFS-M",
        }
    }
    const fn description(self) -> &'static str {
        match self {
            Self::Fast => {
                "dBFS, a 30 ms window with meter ballistics — the same reading the scene draws."
            }
            Self::Momentary => {
                "LUFS-M, the BS.1770 momentary loudness: the full 400 ms window, unballistic."
            }
        }
    }
    const fn other(self) -> Self {
        match self {
            Self::Fast => Self::Momentary,
            Self::Momentary => Self::Fast,
        }
    }
    /// Index into `ObjectMeters::peaks`.
    const fn lane(self) -> usize {
        match self {
            Self::Fast => 0,
            Self::Momentary => 1,
        }
    }
    /// The decibel offset from a K-weighted mean square to this unit.
    ///
    /// BS.1770-4 defines loudness as `-0.691 + 10*log10(mean square)`; the
    /// scene's dBFS reading is the same mean square without that offset. Naming
    /// the difference here is what keeps the two columns from being quietly
    /// the same number under two labels.
    const fn offset_decibels(self) -> f32 {
        match self {
            Self::Fast => 0.0,
            Self::Momentary => -0.691,
        }
    }
}

/// A meter's peak marker: hold it still, then let it fall.
#[derive(Debug, Clone, Copy, Default)]
struct PeakHold {
    level: f32,
    held_for: f32,
}

impl PeakHold {
    /// Take a new reading `elapsed` seconds after the last one.
    fn advance(&mut self, level: f32, elapsed: f32) {
        if level >= self.level {
            self.level = level;
            self.held_for = 0.0;
            return;
        }
        self.held_for += elapsed;
        // Only the part of this step that lands past the hold counts as fall
        // time. Charging the whole step would make the first frame after a long
        // hold drop the marker by the length of the hold.
        let falling = (self.held_for - scene3d::params::PEAK_HOLD_SECONDS).min(elapsed);
        if falling <= 0.0 {
            return;
        }
        // A fixed number of decibels per second, so the slide looks the same
        // wherever on the scale it starts. Never below the live level, which
        // would put the marker inside the bar it is supposed to lead.
        let fall = 10.0_f32.powf(-scene3d::params::PEAK_FALL_DECIBELS_PER_SECOND * falling / 20.0);
        self.level = (self.level * fall).max(level);
    }
}

impl ObjectMeters {
    /// Chase each slot's recent mean square, then hand back the levels.
    fn advance(
        &mut self,
        frame: &crate::scene_view::SceneViewFrame,
        key: crate::decoder::PlaybackKey,
        sample_rate: u32,
        now: Instant,
    ) {
        if self.key != Some(key) {
            self.level = [0.0; crate::scene_view::MAX_VIEW_OBJECTS];
            self.momentary = [0.0; crate::scene_view::MAX_VIEW_OBJECTS];
            // A fresh playback starts fully present rather than inheriting the
            // previous stream's silence: nothing has been measured yet, and
            // "not measured" is not "quiet".
            self.silent_for = [0.0; crate::scene_view::MAX_VIEW_OBJECTS];
            // Peaks and clips are statements about audio that was played. None
            // of it belongs to the stream that starts here.
            self.peaks = [[PeakHold::default(); crate::scene_view::MAX_VIEW_OBJECTS]; 2];
            self.clip_held = [0.0; crate::scene_view::MAX_VIEW_OBJECTS];
            self.key = Some(key);
            self.last_advanced = None;
        }
        let elapsed = self
            .last_advanced
            .map_or(0.0, |last| now.duration_since(last).as_secs_f32());
        self.last_advanced = Some(now);

        let window = window_frames(sample_rate);
        let momentary_window = momentary_window_frames(sample_rate);
        for slot in 0..crate::scene_view::MAX_VIEW_OBJECTS {
            // No audio measured yet is not the same as silence: leave the
            // meter where it is rather than releasing from a reading that was
            // never taken.
            let mut level = self.level[slot];
            let target = root_mean_square(frame.mean_square(slot, window)).unwrap_or(level);
            let tau = if target > level {
                scene3d::params::METER_ATTACK_MILLISECONDS
            } else {
                scene3d::params::METER_RELEASE_MILLISECONDS
            };
            // The same one-pole form the head tracker eases with.
            let alpha = 1.0 - (-elapsed * 1000.0 / tau).exp();
            level += (target - level) * alpha.clamp(0.0, 1.0);
            self.level[slot] = level;

            if level < scene3d::params::OBJECT_SILENT_GAIN {
                self.silent_for[slot] += elapsed;
            } else {
                self.silent_for[slot] = 0.0;
            }

            // The momentary reading takes the whole ring and no ballistics, so
            // "not measured yet" is the only state it has to hold through.
            if let Some(momentary) = root_mean_square(frame.mean_square(slot, momentary_window)) {
                self.momentary[slot] = momentary;
            }
            let momentary = self.momentary[slot];
            self.peaks[MeterReadout::Fast.lane()][slot].advance(level, elapsed);
            self.peaks[MeterReadout::Momentary.lane()][slot].advance(momentary, elapsed);

            // Clipping is about the samples the renderer was handed, not about
            // how loud they sounded, so it reads the unweighted peak and keeps
            // a clock of its own.
            if frame.sample_peak(slot) >= 1.0 {
                self.clip_held[slot] = scene3d::params::CLIP_HOLD_SECONDS;
            } else {
                self.clip_held[slot] = (self.clip_held[slot] - elapsed).max(0.0);
            }
        }
    }

    /// What the bank should draw for `slot`: the reading, its peak marker and
    /// whether the object clipped recently.
    fn bank_row(&self, slot: usize, readout: MeterReadout) -> (f32, f32, bool) {
        let level = match readout {
            MeterReadout::Fast => self.level[slot],
            MeterReadout::Momentary => self.momentary[slot],
        };
        (
            level,
            self.peaks[readout.lane()][slot].level,
            self.clip_held[slot] > 0.0,
        )
    }

    /// Linear level per slot, as the meter currently reads it.
    const fn levels(&self) -> &[f32; crate::scene_view::MAX_VIEW_OBJECTS] {
        &self.level
    }

    /// How present each slot should be drawn, `1.0` fully and `0.0` faded out.
    ///
    /// One number with two readings, because the nameplate and the object want
    /// different floors from the same fade: the plate takes it as an opacity
    /// and so disappears, while the cube uses it to recede toward the stage and
    /// stops at [`scene3d::params::SILENT_PRESENCE_FLOOR`].
    fn presence(&self, slot: usize) -> f32 {
        let Some(silent_for) = self.silent_for.get(slot) else {
            return 1.0;
        };
        // Expressed as time remaining rather than as the fraction elapsed. The
        // two are equivalent, but this one lands on exactly zero at the end of
        // the fade — both sides of the subtraction round the same way — where
        // the elapsed form leaves a sliver of presence behind and an object
        // that is invisible but still counted as present.
        let remaining = (scene3d::params::SILENCE_HOLD_SECONDS
            + scene3d::params::SILENCE_FADE_SECONDS)
            - silent_for;
        (remaining / scene3d::params::SILENCE_FADE_SECONDS).clamp(0.0, 1.0)
    }

    /// Presence as the scene should draw it, given the fade switch.
    ///
    /// With the fade off every object is fully present, but the clock
    /// underneath keeps running: the switch decides what is *drawn*, never what
    /// is measured, so turning it back on shows the state the objects are
    /// actually in instead of restarting every hold from zero. It is also
    /// deliberately blind to whether the loudness readout is on — that is a
    /// third, independent switch, and an object worth hiding is worth hiding
    /// whether or not its level is being printed.
    fn drawn_presence(&self, slot: usize, fading: bool) -> f32 {
        if fading { self.presence(slot) } else { 1.0 }
    }

    /// Slots that have faded out completely.
    fn faded_out(&self, objects: usize) -> usize {
        (0..objects.min(crate::scene_view::MAX_VIEW_OBJECTS))
            .filter(|slot| self.presence(*slot) <= 0.0)
            .count()
    }
}

/// Where a level sits on the nameplate strip, on the footprint's decibel scale
/// so the two readings of the same object agree.
fn loudness_fraction(loudness: f32) -> f32 {
    let quiet =
        (loudness.max(0.0).log10() / scene3d::params::OBJECT_SILENT_GAIN.log10()).clamp(0.0, 1.0);
    1.0 - quiet
}

/// One meter row's track: the level, the gain it was asked for, the peak
/// marker and a clip.
fn draw_meter_track(
    painter: &egui::Painter,
    track: egui::Rect,
    level: f32,
    gain: f32,
    peak: f32,
    clipped: bool,
) {
    /// The sliver of track at full scale that a clip lights up.
    const CLIP_SEGMENT: f32 = 3.0;

    painter.rect_filled(track, 1.0, theme::HOVER);
    let filled = track.width() * loudness_fraction(level);
    painter.rect_filled(
        egui::Rect::from_min_size(track.min, egui::vec2(filled, track.height())),
        1.0,
        theme::ACCENT,
    );
    // Gain on the same scale as the level, which is the whole point of putting
    // them on one track: a fill far short of the tick is an object that was
    // positioned and gained and has nothing in its track.
    let gain_x = track.left() + track.width() * loudness_fraction(gain);
    painter.line_segment(
        [
            egui::pos2(gain_x, track.top()),
            egui::pos2(gain_x, track.bottom()),
        ],
        Stroke::new(1.0, theme::MUTED),
    );
    if peak > 0.0 {
        let peak_x = track.left() + track.width() * loudness_fraction(peak);
        painter.line_segment(
            [
                egui::pos2(peak_x, track.top()),
                egui::pos2(peak_x, track.bottom()),
            ],
            Stroke::new(1.5, theme::INK),
        );
    }
    if clipped {
        // At the right end, because that is where full scale is.
        painter.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(track.right() - CLIP_SEGMENT, track.top()),
                egui::vec2(CLIP_SEGMENT, track.height()),
            ),
            1.0,
            theme::WARNING,
        );
    }
}

/// What a meter row says when the pointer rests on it: the things the row draws
/// as marks rather than as numbers.
fn meter_row_tooltip(
    slot: usize,
    object: &crate::scene_view::ObjectView,
    peak: f32,
    clipped: bool,
    readout: MeterReadout,
) -> String {
    let floor = scene3d::params::OBJECT_SILENT_GAIN;
    let (peak_sign, peak_magnitude) =
        decibel_cells((peak >= floor).then(|| readout_decibels(peak, readout)));
    let (gain_sign, gain_magnitude) = decibel_cells(
        (object.gain >= floor).then(|| readout_decibels(object.gain, MeterReadout::Fast)),
    );
    format!(
        "Object {} · element {}\npeak {}{} {} · gain {}{} dB{}",
        object_display_number(slot),
        object.element_id,
        peak_sign.trim(),
        peak_magnitude,
        readout.unit(),
        gain_sign.trim(),
        gain_magnitude,
        if clipped {
            "\nclipped: a sample reached full scale"
        } else {
            ""
        }
    )
}

/// A linear level in decibels, on the unit `readout` reads in.
fn readout_decibels(level: f32, readout: MeterReadout) -> f32 {
    20.0f32.mul_add(level.max(0.0).log10(), readout.offset_decibels())
}

/// The two cells a decibel readout occupies: a sign and a magnitude.
///
/// `None` is the silence floor, which has no finite reading. The nameplate and
/// the meter bank both lay the result out the same way — the sign owns one
/// cell, the magnitude is right-aligned against the last — so the same level
/// reads character for character the same in both places, and a reader
/// comparing the two is comparing values rather than typography.
fn decibel_cells(decibels: Option<f32>) -> (&'static str, String) {
    match decibels {
        None => ("−", "∞".to_owned()),
        // Just under zero is still zero once rounded, and "−0.0" reads as a
        // fault rather than as full scale.
        Some(value) if value > -0.05 => (" ", format!("{:.1}", value.abs())),
        Some(value) => ("−", format!("{:.1}", value.abs())),
    }
}

/// A nameplate's opacity: the dim it rests at, scaled by how present it is.
///
/// Two statements, deliberately kept apart. Dropping to
/// [`NAMEPLATE_SILENT_ALPHA`](scene3d::params::NAMEPLATE_SILENT_ALPHA) says
/// "this readout has nothing left to report", and happens the moment the level
/// crosses the floor — the same instant the digits themselves turn to `-∞`.
/// Presence then says how long that has been true, and takes the plate the rest
/// of the way: to nothing while the fade is switched on, or no further than the
/// dim while it is off.
fn plate_alpha(silent: bool, presence: f32) -> f32 {
    let rest = if silent {
        scene3d::params::NAMEPLATE_SILENT_ALPHA
    } else {
        1.0
    };
    rest * presence.clamp(0.0, 1.0)
}

/// A theme colour at a given opacity.
fn fade(colour: Color32, alpha: f32) -> Color32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "alpha is clamped to 0..=1 before scaling into a u8"
    )]
    let alpha = (alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), alpha)
}

/// Frames the fast meter averages before the ballistics see them.
/// A mean square from the mirror as a linear RMS, or `None` when the window
/// carried no audio at all — which is not the same as measuring silence.
fn root_mean_square((mean_square, frames): (f64, u32)) -> Option<f32> {
    if frames == 0 {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a mean square is a display value; f32 carries it well past the -36 dB floor"
    )]
    let rms = mean_square.sqrt() as f32;
    Some(rms)
}

/// The BS.1770 momentary window in frames, which is the mirror's whole ring.
///
/// Derived from the ring rather than restated as milliseconds: the ring was
/// sized to be exactly this window, and a second definition of the same 400 ms
/// is a second thing to keep in step.
fn momentary_window_frames(sample_rate: u32) -> u32 {
    let bins = u32::try_from(crate::scene_view::LOUDNESS_BINS).unwrap_or(u32::MAX);
    crate::scene_view::loudness_bin_frames(sample_rate).saturating_mul(bins)
}

fn window_frames(sample_rate: u32) -> u32 {
    (u64::from(sample_rate) * u64::from(scene3d::params::METER_WINDOW_MILLISECONDS) / 1000)
        .try_into()
        .unwrap_or(u32::MAX)
        .max(1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputSyncAction {
    Configure,
    Preserve,
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaylistStep {
    Previous,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeviceWait {
    request_id: u64,
    playback_epoch: u64,
    frame: u64,
}

impl DeviceWait {
    const fn belongs_to(self, request_id: u64, playback_epoch: u64) -> bool {
        self.request_id == request_id && self.playback_epoch == playback_epoch
    }
}

const fn output_sync_action(
    decode_phase: DecodePhase,
    configured_for_playback: bool,
) -> OutputSyncAction {
    match (decode_phase, configured_for_playback) {
        (DecodePhase::Ready | DecodePhase::EndOfStream, false) => OutputSyncAction::Configure,
        (
            DecodePhase::Seeking
            | DecodePhase::Buffering
            | DecodePhase::Ready
            | DecodePhase::EndOfStream,
            true,
        )
        | (DecodePhase::Seeking | DecodePhase::Buffering, false) => OutputSyncAction::Preserve,
        _ => OutputSyncAction::Reset,
    }
}

fn take_playback_restore(pending: &mut bool, playback_intent: bool) -> Option<bool> {
    std::mem::take(pending).then_some(playback_intent)
}

fn shuffle_seed() -> u64 {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_secs() ^ u64::from(duration.subsec_nanos())
        });
    if seed == 0 {
        0x9E37_79B9_7F4A_7C15
    } else {
        seed
    }
}

fn shuffled_source_index(
    selected_source: Option<usize>,
    item_count: usize,
    state: &mut u64,
) -> Option<usize> {
    let selected = selected_source.filter(|index| *index < item_count)?;
    let candidate_count = item_count.checked_sub(1)?;
    if candidate_count == 0 {
        return None;
    }

    let mut value = if *state == 0 {
        0x9E37_79B9_7F4A_7C15
    } else {
        *state
    };
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    *state = value;

    let candidate_count = u64::try_from(candidate_count).unwrap_or(u64::MAX);
    let candidate = usize::try_from(value % candidate_count).unwrap_or(0);
    Some(if candidate >= selected {
        candidate + 1
    } else {
        candidate
    })
}

fn should_handle_completed_item(
    output_phase: OutputPhase,
    playback_intent: bool,
    output_matches_decoder: bool,
) -> bool {
    output_phase == OutputPhase::Ended && playback_intent && output_matches_decoder
}

/// How soon the next pass has to run, given what the output is doing.
///
/// This is not only a smoothness knob. While the window is minimized or hidden,
/// eframe runs no egui pass at all and drives [`eframe::App::logic`] instead, so
/// what this returns is the only thing keeping a backgrounded player moving
/// through its playlist — nothing else asks for the pass that would notice the
/// current item ended.
const fn output_repaint_delay(
    phase: OutputPhase,
    playback_intent: bool,
    showing: bool,
) -> Option<Duration> {
    match phase {
        // Objects move while playing, and the stage now shows them, so the
        // repaint has to keep up with motion rather than with a text label:
        // 50 ms was invisible under a phase message and is a visible stutter
        // under a travelling object.
        OutputPhase::Playing if showing => Some(Duration::from_millis(16)),
        // Motion only needs a frame rate when there is a frame. Hidden, the
        // tick exists solely to keep playback correct, and everything that
        // depends on it — errors, device recovery, the seek index landing — is
        // event-driven and none the worse for a quarter of a second.
        OutputPhase::Playing => Some(Duration::from_millis(250)),
        OutputPhase::Initializing => Some(Duration::from_millis(50)),
        // The item finished and the next one is still being opened. Poll until
        // the hand-off lands, rather than leaving a second of silence between
        // tracks. Deliberately not conditioned on `showing`: the hand-off is
        // exactly what has to stay prompt when nobody is watching. This is
        // self-limiting anyway — completing it leaves `Ended`, and reaching the
        // end of the playlist clears the intent.
        OutputPhase::Ended if playback_intent => Some(Duration::from_millis(20)),
        _ => None,
    }
}

fn advance_scene_preview(
    output: &mut SpatialOutputController,
    playing: bool,
    context: &egui::Context,
    now: Instant,
) {
    let previous_revision = output.revision();
    output.advance_preview(playing, now);
    // Recovery is checked before preview advancement. At decoded EOS there is
    // no decoder poll left to wake the next pass, and Failed has no playback
    // repaint cadence. Schedule one pass for a newly reported failure, without
    // keeping a latched/unrecoverable error repainting indefinitely.
    if output.revision() != previous_revision && output.snapshot().phase() == OutputPhase::Failed {
        context.request_repaint();
    }
}

fn cancel_output_change_after_decode_failure(
    output: &mut SpatialOutputController,
    pending: &mut Option<OutputSettings>,
) -> bool {
    let previous = pending.take();
    let interrupted = previous.is_some() || output.settings_pending();
    output.pause();
    output.reset();
    let _ = output.take_settings_result();
    if let Some(previous) = previous {
        output.install_settings(previous);
    }
    interrupted
}

const fn should_replay_current_on_completion(mode: PlaybackMode, item_count: usize) -> bool {
    match mode {
        PlaybackMode::RepeatOne => true,
        PlaybackMode::RepeatAll => item_count == 1,
        PlaybackMode::Sequential | PlaybackMode::Shuffle => false,
    }
}

impl PlayerApp {
    pub fn new(
        creation_context: &eframe::CreationContext<'_>,
        directory: Arc<DataDirectory>,
    ) -> Self {
        theme::install(&creation_context.egui_ctx);
        let scene_renderer_ready = creation_context
            .wgpu_render_state
            .as_ref()
            .is_some_and(scene3d::gpu::SceneRenderer::install);
        Self::from_storage(
            &creation_context.egui_ctx,
            creation_context.storage,
            scene_renderer_ready,
            directory,
        )
    }

    fn from_storage(
        context: &egui::Context,
        storage: Option<&dyn eframe::Storage>,
        scene_renderer_ready: bool,
        directory: Arc<DataDirectory>,
    ) -> Self {
        let preferred_device = storage
            .and_then(|storage| eframe::get_value(storage, OUTPUT_DEVICE_STORAGE_KEY))
            .unwrap_or_default();
        let output = SpatialOutputController::new();
        let settings = storage
            .and_then(|storage| {
                eframe::get_value::<OutputSettings>(storage, OUTPUT_SETTINGS_STORAGE_KEY)
            })
            .unwrap_or_else(|| OutputSettings {
                native_device: preferred_device,
                ..Default::default()
            });
        // A view angle is something the user worked to find; losing it on every
        // restart is a small tax on the whole point of a free camera.
        let camera = storage
            .and_then(|storage| eframe::get_value(storage, SCENE_CAMERA_STORAGE_KEY))
            .map_or_else(
                scene3d::camera::Camera::default,
                scene3d::camera::Camera::from_state,
            );
        let preferences = AppPreferences {
            output: settings.validated(),
            camera: camera.state(),
            ..Default::default()
        };
        let sofa = crate::file_catalog::Catalog::new(
            crate::file_catalog::SOFA,
            directory.path.join(crate::file_catalog::SOFA.slug),
        );
        let hptf = crate::file_catalog::Catalog::new(
            crate::file_catalog::HPTF,
            directory.path.join(crate::file_catalog::HPTF.slug),
        );
        let skins = crate::skin_library::Catalog::new(directory.path.join("skins"));
        let library = LibraryController::new(directory, preferences.clone(), context.clone());
        Self {
            library,
            smoke: None,
            about: crate::licenses::Window::default(),
            sofa,
            hptf,
            skins,
            browse: BrowseState::default(),
            cursor: None,
            playlist_ui: playlist_ui::State::default(),
            file_picker: None,
            inspection_media: None,
            preferences_observed: preferences.clone(),
            preferences,
            preferences_dirty_at: None,
            browse_observed: SavedBrowse::default(),
            browse_dirty_at: None,
            last_session_save: Instant::now(),
            last_saved_intent: false,
            checkpoint: SessionState::default(),
            resume: None,
            automatic_candidate: false,
            failed_candidates: std::collections::HashSet::new(),
            marked_failure_key: None,
            media_source: None,
            inspection: InspectionController::new(),
            decoder: DecoderController::new(),
            decoder_revision: 0,
            output,
            output_revision: 0,
            backend: SpatialBackendKind::Automatic,
            status: StatusLine::idle("Add or drop AC-4 media files"),
            timeline_preview: 0.0,
            timeline_dragging: false,
            playback_restore_pending: false,
            playback_intent: false,
            playback_mode: PlaybackMode::default(),
            shuffle_history: Vec::new(),
            shuffle_state: shuffle_seed(),
            automatic_reconfigure_guard: None,
            waiting_for_device: None,
            volume: 0.8,
            muted: false,
            bitstream_details_open: false,
            diagnostics_open: false,
            output_settings_open: false,
            visual_settings_open: false,
            pending_output_change: None,
            audio_settings_error: None,
            sofa_picker: None,
            hptf_picker: None,
            hptf_drawing: None,
            hptf_ghost: None,
            skin_picker: None,
            camera,
            object_numbers_visible: true,
            object_loudness_visible: true,
            fade_silent_objects: true,
            meter_bank_open: false,
            meter_readout: MeterReadout::default(),
            object_meters: ObjectMeters::default(),
            figure: scene3d::figure::Figure::default(),
            scene_mesh: scene3d::mesh::MeshBuilder::default(),
            scene_renderer_ready,
            // Assume the window is showing until a `logic` without a preceding
            // `ui` proves otherwise, so initial playback uses the visible cadence.
            stage_drawn: true,
        }
    }

    fn replay_current_source(&mut self) {
        self.resume = None;
        self.playback_intent = true;
        self.automatic_reconfigure_guard = None;
        self.waiting_for_device = None;
        match self.decoder.seek(0) {
            Ok(()) => {
                self.timeline_preview = 0.0;
                self.playback_restore_pending = true;
                self.status = StatusLine::idle("Replaying from the beginning");
            }
            Err(error) => {
                self.playback_intent = false;
                self.playback_restore_pending = false;
                self.status = StatusLine::warning(error);
            }
        }
    }

    fn retry_current_source(&mut self) {
        self.retry_playback();
    }

    fn sync_inspection(&mut self, context: &egui::Context) {
        self.inspection.poll();
        if let Some(source) = self.browsed_media() {
            self.inspection.ensure_requested_source(source);
        } else {
            self.bitstream_details_open = false;
        }
        if self.inspection.has_pending() {
            context.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn sync_decoder(&mut self, context: &egui::Context) {
        if let Some(source) = self.playback_media() {
            self.decoder.ensure_open_source(&source);
        } else {
            self.decoder.close();
        }
        self.decoder.poll();
        if self.decoder_revision != self.decoder.revision() {
            self.decoder_revision = self.decoder.revision();
            self.status = decoder_status_line(self.decoder.snapshot());
        }
        if self.decoder.is_working() {
            context.request_repaint_after(Duration::from_millis(50));
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "output synchronization keeps decode, device, renderer, and UI revisions atomic"
    )]
    fn sync_output(&mut self, context: &egui::Context, showing: bool) {
        let request_id = self.decoder.request_id();
        let playback_epoch = self.decoder.playback_epoch();
        let master_gain = if self.muted { 0.0 } else { self.volume };
        self.output.set_master_gain(master_gain);
        if self.decoder.snapshot().phase() == DecodePhase::Failed {
            // A decoder failure can arrive before the replacement output is
            // configured. End the handoff here rather than waiting forever
            // for that output to acknowledge Ready/Failed.
            if cancel_output_change_after_decode_failure(
                &mut self.output,
                &mut self.pending_output_change,
            ) {
                self.audio_settings_error = Some(
                    "The audio settings change was cancelled after a decode failure; previous settings were retained.".into(),
                );
            }
            if self.handle_media_failure(context) {
                return;
            }
            self.playback_intent = false;
            self.playback_restore_pending = false;
            self.automatic_reconfigure_guard = None;
            self.waiting_for_device = None;
            self.timeline_dragging = false;
            self.output.poll();
            self.backend = self.output.settings().mode;
            self.output_revision = self.output.revision();
            self.status = decoder_status_line(self.decoder.snapshot());
            return;
        }
        self.output.poll();
        self.clear_successful_media_error();
        #[cfg(macinrender_output)]
        if self.finish_output_preparation(context) {
            return;
        }
        self.backend = self.output.settings().mode;
        let [yaw, pitch, roll] = self.output.head_snapshot().pose.euler();
        self.figure.head_yaw = yaw;
        self.figure.head_pitch = pitch;
        self.figure.head_roll = roll;
        if let Some(result) = self.output.take_settings_result() {
            self.audio_settings_error = result.as_ref().err().cloned();
            self.status = match result {
                Ok(()) => StatusLine::idle("Audio settings applied"),
                Err(error) => StatusLine::warning(format!("Audio settings unchanged: {error}")),
            };
        }
        if self
            .output
            .is_configured_for_playback(request_id, playback_epoch)
        {
            if self.output.snapshot().phase() == OutputPhase::Failed {
                if let Some(previous) = self.pending_output_change.take() {
                    let error = self
                        .output
                        .snapshot()
                        .error()
                        .unwrap_or("Output initialization failed")
                        .to_owned();
                    self.audio_settings_error = Some(error.clone());
                    let target = self.output.snapshot().playhead_frames();
                    if self.decoder.seek(target).is_ok() {
                        self.output.reset();
                        self.output.install_settings(previous);
                        self.playback_restore_pending = true;
                        self.status =
                            StatusLine::warning(format!("Restoring previous output: {error}"));
                        context.request_repaint_after(Duration::from_millis(20));
                        return;
                    }
                }
            } else if matches!(
                self.output.snapshot().phase(),
                OutputPhase::Ready
                    | OutputPhase::Playing
                    | OutputPhase::Paused
                    | OutputPhase::Ended
            ) {
                self.pending_output_change = None;
            }
        }

        let output_matches_decoder = self
            .output
            .is_configured_for_playback(request_id, playback_epoch);
        if self.handle_completed_playlist_item(context, output_matches_decoder) {
            return;
        }

        let ready_scene = matches!(
            self.decoder.snapshot().phase(),
            DecodePhase::Ready | DecodePhase::EndOfStream
        )
        .then(|| self.decoder.snapshot().metrics())
        .flatten()
        .map(|metrics| {
            (
                metrics.object_count(),
                metrics.duration_frames(),
                metrics.target_frame(),
            )
        });
        let desired_device = ready_scene
            .as_ref()
            .and_then(|(objects, _, _)| self.output.resolved_device(*objects));

        if self
            .waiting_for_device
            .is_some_and(|waiting| !waiting.belongs_to(request_id, playback_epoch))
        {
            self.waiting_for_device = None;
        }
        if let Some(waiting) = self.waiting_for_device {
            if desired_device.is_none() {
                self.output.pause();
                self.output.reset();
                self.status = StatusLine::warning(
                    "No active audio endpoint can host this Scene; waiting without closing the file",
                );
                context.request_repaint_after(Duration::from_secs(2));
                return;
            }
            match self.decoder.seek(waiting.frame) {
                Ok(()) => {
                    self.waiting_for_device = None;
                    self.playback_restore_pending = true;
                    self.status = StatusLine::idle("Compatible audio endpoint restored; resuming");
                    context.request_repaint_after(Duration::from_millis(20));
                    return;
                }
                Err(error) => {
                    self.status = StatusLine::warning(format!(
                        "Compatible endpoint restored, but playback cannot resume yet: {error}"
                    ));
                    context.request_repaint_after(Duration::from_millis(50));
                    return;
                }
            }
        }
        if let Some((_, duration, decoder_target)) = ready_scene
            && desired_device.is_none()
        {
            let frame = if self
                .output
                .is_configured_for_playback(request_id, playback_epoch)
            {
                self.output.snapshot().playhead_frames()
            } else {
                decoder_target
            }
            .min(duration.unwrap_or(u64::MAX));
            self.output.pause();
            self.output.reset();
            self.waiting_for_device = Some(DeviceWait {
                request_id,
                playback_epoch,
                frame,
            });
            self.status = StatusLine::warning(
                "No active audio endpoint can host this Scene; waiting without closing the file",
            );
            context.request_repaint_after(Duration::from_secs(2));
            return;
        }

        if let Some((guard_request, guard_frame)) = self.automatic_reconfigure_guard
            && guard_request == request_id
            && self.output.snapshot().playhead_frames() > guard_frame.saturating_add(2_048)
            && !matches!(self.output.snapshot().phase(), OutputPhase::Failed)
        {
            self.automatic_reconfigure_guard = None;
        }

        if matches!(self.output.snapshot().phase(), OutputPhase::Failed)
            && let Some(error) = self.output.snapshot().error()
            && is_reconfigurable_scene_error(error)
        {
            let target = self.output.snapshot().playhead_frames();
            if self.automatic_reconfigure_guard != Some((request_id, target)) {
                self.automatic_reconfigure_guard = Some((request_id, target));
                self.output.reset();
                match self.decoder.seek(target) {
                    Ok(()) => {
                        self.playback_restore_pending = true;
                        self.status =
                            StatusLine::idle("Adapting output to a Scene topology change");
                        context.request_repaint_after(Duration::from_millis(20));
                        return;
                    }
                    Err(seek_error) => self.status = StatusLine::warning(seek_error),
                }
            }
        }

        if matches!(
            self.decoder.snapshot().phase(),
            DecodePhase::Ready | DecodePhase::EndOfStream
        ) && self
            .output
            .is_configured_for_playback(request_id, playback_epoch)
            && let Some(metrics) = self.decoder.snapshot().metrics()
        {
            let desired_device = self.output.resolved_device(metrics.object_count());
            if desired_device.as_ref() != self.output.configured_device()
                && desired_device.is_some()
            {
                let target = self
                    .output
                    .snapshot()
                    .playhead_frames()
                    .min(metrics.duration_frames().unwrap_or(u64::MAX));
                self.output.pause();
                match self.decoder.seek(target) {
                    Ok(()) => {
                        self.playback_restore_pending = true;
                        self.status = StatusLine::idle("Switching Windows audio endpoint");
                        context.request_repaint_after(Duration::from_millis(20));
                        return;
                    }
                    Err(error) => self.status = StatusLine::warning(error),
                }
            }
        }

        let configured_for_playback = self
            .output
            .is_configured_for_playback(request_id, playback_epoch);
        match output_sync_action(self.decoder.snapshot().phase(), configured_for_playback) {
            OutputSyncAction::Configure => {
                let config = self
                    .decoder
                    .snapshot()
                    .metrics()
                    .ok_or_else(|| "Ready decoder state has no Scene metrics".to_owned())
                    .and_then(|metrics| {
                        let scene_signature =
                            metrics.scene_signature().cloned().ok_or_else(|| {
                                "Ready decoder state has no renderable Scene signature".to_owned()
                            })?;
                        let output_device = self
                            .output
                            .resolved_device(metrics.object_count())
                            .ok_or_else(|| {
                                "No active audio endpoint can host this Scene".to_owned()
                            })?;
                        OutputStreamConfig::new(
                            request_id,
                            playback_epoch,
                            metrics.target_frame(),
                            metrics.sample_rate(),
                            scene_signature,
                            output_device,
                        )
                    });
                match config {
                    Ok(config) => {
                        self.output
                            .ensure_configured(&config, self.decoder.scene_reader());
                        if self
                            .output
                            .is_configured_for_playback(request_id, playback_epoch)
                            && let Some(resume) = take_playback_restore(
                                &mut self.playback_restore_pending,
                                self.playback_intent,
                            )
                        {
                            if resume {
                                self.output.play();
                            } else {
                                self.output.pause();
                            }
                        }
                    }
                    Err(error) => {
                        self.output.reset();
                        self.status = StatusLine::warning(error);
                    }
                }
            }
            OutputSyncAction::Preserve => {}
            OutputSyncAction::Reset => self.output.reset(),
        }

        // The scene view's clock where no renderer provides one. Placed after
        // configuration so a preview built this frame also advances this frame,
        // and before the revision check so its snapshot reaches the status line
        // without a round trip.
        advance_scene_preview(
            &mut self.output,
            self.playback_intent,
            context,
            Instant::now(),
        );

        if self.output_revision != self.output.revision() {
            self.output_revision = self.output.revision();
            self.status = output_status_line(self.output.snapshot(), self.decoder.snapshot());
        }
        if !self.timeline_dragging
            && !matches!(self.decoder.snapshot().phase(), DecodePhase::Seeking)
            && self
                .output
                .is_configured_for_playback(request_id, playback_epoch)
            && let Some(metrics) = self.decoder.snapshot().metrics()
            && let Some(duration) = metrics.duration_frames()
            && duration > 0
        {
            const TIMELINE_STEPS: u64 = 10_000;
            let submitted = self.output.snapshot().playhead_frames().min(duration);
            let scaled = submitted.saturating_mul(TIMELINE_STEPS) / duration;
            let scaled = u16::try_from(scaled).unwrap_or(u16::MAX);
            self.timeline_preview = f32::from(scaled) / 10_000.0;
        }
        if let Some(delay) = output_repaint_delay(
            self.output.snapshot().phase(),
            self.playback_intent,
            showing,
        ) {
            context.request_repaint_after(delay);
        }
        #[cfg(spatial_output)]
        context.request_repaint_after(Duration::from_secs(2));
    }

    fn handle_bitstream_action(&mut self, action: Option<BitstreamAction>) {
        match action {
            Some(BitstreamAction::OpenDetails) => self.bitstream_details_open = true,
            Some(BitstreamAction::Retry) => {
                if let Some(path) = self.selected_path().map(Path::to_path_buf) {
                    self.inspection.retry(&path);
                }
            }
            None => {}
        }
    }

    fn accept_dropped_files(&mut self, context: &egui::Context) {
        let paths = context.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| {
                    let path = file.path();
                    (!path.as_os_str().is_empty()).then(|| path.to_path_buf())
                })
                .collect::<Vec<_>>()
        });
        if !paths.is_empty() {
            self.append_sources(paths);
            // The synchronisation that opens the file runs in `logic`, which is
            // the pass after this one. Nothing else would ask for it.
            context.request_repaint();
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the endpoint picker and header share one tightly coupled egui panel"
    )]
    fn draw_header(&mut self, root: &mut egui::Ui) {
        let compact_header = root.available_width() < 1_100.0;
        let output = self.output.snapshot();
        let devices = self.output.devices().to_vec();
        let preferred = self.output.preferred_device().clone();
        let preferred_label = match &preferred {
            OutputDeviceSelection::SystemDefault if output.is_preview() => {
                output.device_label().to_owned()
            }
            OutputDeviceSelection::SystemDefault => match output.phase() {
                OutputPhase::Unavailable => "Spatial output unavailable".to_owned(),
                OutputPhase::Idle => "System default".to_owned(),
                _ => format!("System default · {}", output.device_label()),
            },
            OutputDeviceSelection::EndpointId(id) => {
                devices.iter().find(|device| device.id() == id).map_or_else(
                    || format!("Preferred unavailable · {}", output.device_label()),
                    |device| device.label().to_owned(),
                )
            }
        };
        let source_objects = self
            .decoder
            .snapshot()
            .metrics()
            .and_then(|metrics| u32::try_from(metrics.object_count()).ok())
            .unwrap_or(0);
        let required_objects = self
            .output
            .required_dynamic_objects(source_objects as usize);
        let system_output =
            self.output.settings().mode.resolved() == SpatialBackendKind::SystemSpatial;
        let default_device = devices.iter().find(|device| device.is_default());
        let default_eligible = required_objects.is_none()
            || !self.output.device_catalog_ready()
            || default_device.is_some_and(|device| {
                device
                    .max_dynamic_objects()
                    .is_some_and(|available| available >= required_objects.unwrap_or(0))
            });
        let device_detail = if output.max_dynamic_objects() > 0 {
            format!(
                "{} dynamic-object slots available",
                output.max_dynamic_objects()
            )
        } else {
            output
                .error()
                .or_else(|| self.output.device_catalog_error())
                .unwrap_or("Select a decoded AC-4 scene to activate")
                .to_owned()
        };
        let mut selected = preferred.clone();
        let mut show_settings = self.output_settings_open;
        egui::Panel::top("header")
            .exact_size(72.0)
            .frame(
                egui::Frame::NONE
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(20, 0)),
            )
            .show(root, |ui| {
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("MacinDecode AC-4 Player")
                                .size(21.0)
                                .strong()
                                .color(theme::TEXT),
                        );
                        ui.label(
                            RichText::new(self.output.settings().mode.resolved().label())
                                .size(12.0)
                                .color(theme::MUTED),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("About").clicked() { self.about.open = true; }
                        if ui.button("Visual settings").clicked() { self.visual_settings_open = true; }
                        if ui.button("Audio settings").clicked() { show_settings = true; }
                        ui.add_enabled_ui(!system_output, |ui| {
                        egui::ComboBox::from_id_salt("output-device")
                            .selected_text(preferred_label)
                            .width(if compact_header { 160.0 } else { 220.0 })
                            .truncate()
                            .show_ui(ui, |ui| {
                                let default_response = ui
                                    .add_enabled_ui(default_eligible, |ui| {
                                        ui.selectable_value(
                                            &mut selected,
                                            OutputDeviceSelection::SystemDefault,
                                            "System default",
                                        )
                                    })
                                    .inner;
                                if !default_eligible {
                                    let reason = default_device
                                        .and_then(|device| device.spatial_error())
                                        .map_or_else(
                                            || {
                                                format!(
                                                    "System default cannot provide {} dynamic objects", required_objects.unwrap_or(0)
                                                )
                                            },
                                            str::to_owned,
                                        );
                                    default_response.on_hover_text(reason);
                                }
                                ui.separator();
                                for device in &devices {
                                    let capacity = device.max_dynamic_objects();
                                    let eligible = required_objects.is_none() || capacity
                                        .is_some_and(|available| available >= required_objects.unwrap_or(0));
                                    let label = if device.is_default() {
                                        format!("{} · default", device.label())
                                    } else {
                                        device.label().to_owned()
                                    };
                                    let response = ui
                                        .add_enabled_ui(eligible, |ui| {
                                            ui.selectable_value(
                                                &mut selected,
                                                OutputDeviceSelection::EndpointId(
                                                    device.id().to_owned(),
                                                ),
                                                label,
                                            )
                                        })
                                        .inner;
                                    if !eligible {
                                        let reason = device.spatial_error().map_or_else(
                                            || {
                                                format!(
                                                    "Needs {} dynamic objects; endpoint provides {}", required_objects.unwrap_or(0),
                                                    capacity.unwrap_or(0)
                                                )
                                            },
                                            str::to_owned,
                                        );
                                        response.on_hover_text(reason);
                                    }
                                }
                                ui.separator();
                                ui.add_enabled(false, egui::Label::new(device_detail));
                            });
                        });
                        if !compact_header {
                            ui.label(
                                RichText::new("OUTPUT DEVICE")
                                    .size(10.0)
                                    .strong()
                                    .color(theme::MUTED),
                            );
                        }
                    });
                });
                let clip = ui.clip_rect();
                let bottom = ui.max_rect().bottom() - 0.5;
                ui.painter().line_segment(
                    [
                        egui::pos2(clip.left(), bottom),
                        egui::pos2(clip.right(), bottom),
                    ],
                    Stroke::new(1.0, theme::BORDER),
                );
            });
        self.output_settings_open = show_settings;
        if selected != preferred {
            let mut settings = self.output.settings().clone();
            if settings.mode.resolved() == SpatialBackendKind::SafBinaural {
                settings.stereo_device = selected;
            } else {
                settings.native_device = selected;
            }
            self.change_output_settings(settings, root.ctx());
        }
    }

    fn change_output_settings(&mut self, settings: OutputSettings, context: &egui::Context) {
        let previous = self.output.settings().clone();
        if settings == previous {
            return;
        }
        self.audio_settings_error = None;
        if previous.needs_rebuild(&settings)
            && self.output.is_configured_for_playback(
                self.decoder.request_id(),
                self.decoder.playback_epoch(),
            )
        {
            let target = self.output.snapshot().playhead_frames().min(
                self.decoder
                    .snapshot()
                    .metrics()
                    .and_then(DecodeMetrics::duration_frames)
                    .unwrap_or(u64::MAX),
            );
            #[cfg(macinrender_output)]
            if matches!(
                settings.mode.resolved(),
                SpatialBackendKind::SafBinaural | SpatialBackendKind::SystemSpatial
            ) {
                let result = self.decoder.validate_seek(target).and_then(|()| {
                    let rate = self
                        .decoder
                        .snapshot()
                        .metrics()
                        .expect("validated seek metrics")
                        .sample_rate();
                    self.output.prepare_settings(
                        settings,
                        rate,
                        self.decoder.request_id(),
                        self.decoder.playback_epoch(),
                    )
                });
                match result {
                    Ok(()) => {
                        self.status = StatusLine::idle("Preparing audio settings");
                    }
                    Err(error) => {
                        self.audio_settings_error = Some(error.clone());
                        self.status =
                            StatusLine::warning(format!("Audio settings unchanged: {error}"));
                    }
                }
                context.request_repaint_after(Duration::from_millis(20));
                return;
            }
            match self.decoder.seek(target) {
                Ok(()) => {
                    self.pending_output_change = Some(previous);
                    self.output.pause();
                    self.output.reset();
                    self.output.install_settings(settings);
                    self.playback_restore_pending = true;
                    self.status =
                        StatusLine::idle("Reconfiguring audio from the current playback position");
                }
                Err(error) => {
                    self.audio_settings_error = Some(error.clone());
                    self.status = StatusLine::warning(format!("Audio settings unchanged: {error}"));
                }
            }
        } else {
            self.output.hot_settings(settings);
        }
        context.request_repaint_after(Duration::from_millis(20));
    }

    fn poll_sofa_picker(&mut self, context: &egui::Context) {
        let Some(picker) = self.sofa_picker.as_mut() else {
            return;
        };
        let waker = Waker::from(Arc::new(DialogWake(context.clone())));
        let Poll::Ready(path) = picker.as_mut().poll(&mut Context::from_waker(&waker)) else {
            return;
        };
        self.sofa_picker = None;
        if let Some(path) = path {
            self.sofa.refresh(Some(path.path().to_path_buf()), context);
        }
    }

    fn poll_hptf_picker(&mut self, context: &egui::Context) {
        if let Some(error) = self.output.take_hptf_error() {
            self.audio_settings_error = Some(error);
        }
        let Some(picker) = self.hptf_picker.as_mut() else {
            return;
        };
        let waker = Waker::from(Arc::new(DialogWake(context.clone())));
        let Poll::Ready(path) = picker.as_mut().poll(&mut Context::from_waker(&waker)) else {
            return;
        };
        self.hptf_picker = None;
        if let Some(path) = path {
            self.hptf.refresh(Some(path.path().to_path_buf()), context);
        }
    }

    #[cfg(macinrender_output)]
    fn finish_output_preparation(&mut self, context: &egui::Context) -> bool {
        let Some(prepared) = self
            .output
            .take_prepared_settings(self.decoder.request_id(), self.decoder.playback_epoch())
        else {
            return false;
        };
        let result = prepared.and_then(|prepared| {
            // Preparation may take seconds. Hand off at the current presented
            // position, never the position at which the user clicked.
            let target = self.output.snapshot().playhead_frames().min(
                self.decoder
                    .snapshot()
                    .metrics()
                    .and_then(DecodeMetrics::duration_frames)
                    .unwrap_or(u64::MAX),
            );
            self.decoder.seek(target)?;
            self.pending_output_change = Some(self.output.settings().clone());
            self.output.pause();
            self.output.install_prepared_settings(prepared);
            self.playback_restore_pending = true;
            Ok(())
        });
        match result {
            Ok(()) => {
                self.status = StatusLine::idle("Applying prepared audio settings");
                context.request_repaint_after(Duration::from_millis(20));
                true
            }
            Err(error) => {
                self.audio_settings_error = Some(error.clone());
                self.status = StatusLine::warning(format!("Audio settings unchanged: {error}"));
                false
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "audio settings share a single transactional update"
    )]
    fn draw_output_settings(&mut self, context: &egui::Context) {
        if !self.output_settings_open {
            return;
        }
        let mut open = true;
        let mut settings = self.output.settings().clone();
        let head = self.output.head_snapshot();
        let mut manual = None;
        let mut recenter = false;
        egui::Window::new("Audio settings")
            .open(&mut open)
            .resizable(false)
            .default_width(390.0)
            .show(context, |ui| {
                if let Some(error) = &self.audio_settings_error {
                    ui.colored_label(theme::WARNING, error);
                }
                if self.output.settings_pending() {
                    ui.label("Preparing audio settings…");
                }
                ui.add_enabled_ui(
                    !self.output.settings_pending()
                        && self.pending_output_change.is_none()
                        && self.sofa_picker.is_none() && !self.sofa.busy()
                        && self.hptf_picker.is_none() && !self.hptf.busy(),
                    |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Playback mode");
                            egui::ComboBox::from_id_salt("spatial-output-mode")
                                .selected_text(settings.mode.label())
                                .show_ui(ui, |ui| {
                                    for mode in SpatialBackendKind::ALL {
                                        ui.add_enabled_ui(mode.supported(), |ui| {
                                            ui.selectable_value(
                                                &mut settings.mode,
                                                mode,
                                                mode.label(),
                                            );
                                        });
                                    }
                                });
                        });
                        let mode = settings.mode.resolved();
                        if mode == SpatialBackendKind::SystemSpatial {
                            ui.horizontal(|ui| {
                                ui.label("Speaker layout");
                                egui::ComboBox::from_id_salt("speaker-layout")
                                    .selected_text(settings.layout.label())
                                    .show_ui(ui, |ui| {
                                        for layout in SpeakerLayout::ALL {
                                            ui.selectable_value(
                                                &mut settings.layout,
                                                layout,
                                                layout.label(),
                                            );
                                        }
                                    });
                            });
                            ui.label("Apple speaker geometry · system default output");
                            #[cfg(all(target_os = "macos", macinrender_output))]
                            {
                                let applicable = settings.atmos_label_applicable();
                                ui.add_enabled(applicable, egui::Checkbox::new(
                                    &mut settings.atmos_label_assist, "Control Center Atmos label"
                                )).on_hover_text("Available only for 7.1.4 system spatial output. Changes system content identification; AC-4 audio rendering stays the same.");
                            }
                            if settings.layout == SpeakerLayout::TwentyTwoTwo {
                                ui.horizontal(|ui| {
                                    ui.label("LFE routing");
                                    ui.selectable_value(
                                        &mut settings.split_lfe,
                                        true,
                                        "Equal-power copy",
                                    );
                                    ui.selectable_value(&mut settings.split_lfe, false, "Direct");
                                });
                            }
                        }
                        if mode == SpatialBackendKind::SafBinaural {
                            ui.separator();
                            ui.label(if settings.sofa.is_empty() {
                                "HRTF: built-in KEMAR".to_owned()
                            } else {
                                format!(
                                    "HRTF: {}",
                                    Path::new(&settings.sofa)
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                )
                            });
                            ui.horizontal(|ui| {
                                if ui.button("Choose SOFA…").clicked() {
                                    self.sofa_picker = Some(Box::pin(
                                        rfd::AsyncFileDialog::new()
                                            .set_directory(&self.sofa.root)
                                            .add_filter("SOFA HRIR", &["sofa"])
                                            .pick_file(),
                                    ));
                                    context.request_repaint();
                                }
                                if ui.button("Use KEMAR").clicked() {
                                    settings.sofa.clear();
                                }
                                if ui.button("Refresh SOFA folder").clicked() { self.sofa.refresh(None, context); }
                            });
                            ui.label(self.sofa.root.display().to_string());
                            ui.label(&self.sofa.message);
                            for file in &self.sofa.files {
                                let full_path = self.sofa.root.join(&file.path);
                                let selected = Path::new(&settings.sofa) == full_path;
                                let active = self.output.active_sofa().is_some_and(|path| Path::new(path) == full_path);
                                let status = file.display_status(active);
                                if ui.add_enabled(file.selectable(), egui::Button::selectable(selected, format!("{} · {status}", file.path.display()))).clicked() {
                                    if let Some(path) = full_path.to_str() { path.clone_into(&mut settings.sofa); }
                                    else { self.audio_settings_error = Some("This renderer requires a Unicode SOFA path".into()); }
                                }
                            }
                            ui.separator();
                            // Name the file and nothing else. A ParametricEQ profile
                            // carries no target or measurement provenance, so the
                            // player cannot tell what a given one equalises towards
                            // and must not imply that it knows.
                            ui.label(if settings.hptf.is_empty() {
                                "Profile: none".to_owned()
                            } else {
                                format!("Profile: {}", profile_name(&settings.hptf))
                            })
                            .on_hover_text(
                                "An AutoEq ParametricEQ profile, applied to the headphone feed exactly as written. Which target it equalises towards is decided by whoever generated it and is not recorded in the file; pair it with the reference field your SOFA was equalised to.",
                            );
                            ui.horizontal(|ui| {
                                if ui.button("Choose AutoEq profile…").clicked() {
                                    self.hptf_picker = Some(Box::pin(
                                        rfd::AsyncFileDialog::new()
                                            .set_directory(&self.hptf.root)
                                            .add_filter("AutoEq ParametricEQ", &["txt"])
                                            .pick_file(),
                                    ));
                                    context.request_repaint();
                                }
                                if ui.button("Turn off").clicked() {
                                    settings.hptf.clear();
                                }
                                if ui.button("Refresh profile folder").clicked() {
                                    // The curves are read from the files this
                                    // re-reads, so drop them with it rather than
                                    // let an edited profile keep its old picture.
                                    self.hptf_drawing = None;
                                    self.hptf_ghost = None;
                                    self.hptf.refresh(None, context);
                                }
                            });
                            ui.label(self.hptf.root.display().to_string());
                            ui.label(&self.hptf.message);
                            let mut hovered: Option<String> = None;
                            for file in &self.hptf.files {
                                let full_path = self.hptf.root.join(&file.path);
                                let selected = Path::new(&settings.hptf) == full_path;
                                let active = self
                                    .output
                                    .active_hptf()
                                    .is_some_and(|path| Path::new(path) == full_path);
                                let status = file.display_status(active);
                                let row = ui.add_enabled(
                                    file.selectable(),
                                    egui::Button::selectable(
                                        selected,
                                        format!("{} · {status}", file.path.display()),
                                    ),
                                );
                                // Only a row that can be chosen can be asked
                                // about; one still importing has no settled file
                                // to read.
                                if file.selectable() && row.hovered() {
                                    hovered = full_path.to_str().map(str::to_owned);
                                }
                                if row.clicked() {
                                    if let Some(path) = full_path.to_str() {
                                        path.clone_into(&mut settings.hptf);
                                    } else {
                                        self.audio_settings_error = Some(
                                            "This renderer requires a Unicode profile path".into(),
                                        );
                                    }
                                }
                            }
                            ui.checkbox(
                                &mut settings.hptf_auto_trim,
                                "Trim further if the curve still peaks above 0 dB",
                            )
                            .on_hover_text(
                                "Off uses the profile's own Preamp line verbatim. This bounds the frequency response, not transient peaks.",
                            );
                            let hptf = self.output.hptf_readout();
                            let running = self.output.active_hptf() == Some(settings.hptf.as_str());
                            // The renderer designs at the rate its output runs;
                            // 48 kHz is what a Scene output is created with, so
                            // that is the curve to draw before one exists.
                            let rate = if hptf.rate == 0 { 48_000 } else { hptf.rate };
                            let profile = settings.hptf.clone();
                            let ghost_path = hptf_ghost(hovered.as_deref(), &profile);
                            let drawn = hptf_curve(&mut self.hptf_drawing, &profile, rate);
                            let ghost = hptf_curve(
                                &mut self.hptf_ghost,
                                ghost_path.unwrap_or_default(),
                                rate,
                            );
                            // The drawn line is what the output does, so it takes
                            // the renderer's extra trim — but only while it is
                            // this profile that is running. A ghost never is one,
                            // so a ghost is drawn the way its own file reads.
                            let drawn_curve = drawn
                                .and_then(|drawing| drawing.curve.as_ref().ok())
                                .map(|curve| {
                                    curve.with_output_trim(if running {
                                        hptf.auto_trim_db
                                    } else {
                                        0.0
                                    })
                                });
                            let ghost_curve =
                                ghost.and_then(|drawing| drawing.curve.as_ref().ok());
                            if drawn_curve.is_some() {
                                ui.label(if running {
                                    "Applied response"
                                } else if settings.hptf_auto_trim {
                                    "Profile response · before automatic trim"
                                } else {
                                    "Profile response"
                                });
                            }
                            if drawn_curve.is_some() || ghost_curve.is_some() {
                                draw_hptf_curve(ui, drawn_curve.as_ref(), ghost_curve);
                            }
                            if let Some(path) = ghost_path {
                                // Which line is which. Nothing else in the panel
                                // names the profile being pointed at, and the
                                // pointer is about to leave it.
                                if let Some(Err(error)) = ghost.map(|drawing| &drawing.curve) {
                                    ui.colored_label(
                                        theme::MUTED,
                                        format!("{}: {error}", profile_name(path)),
                                    );
                                } else {
                                    ui.horizontal_wrapped(|ui| {
                                        if drawn_curve.is_some() {
                                            ui.colored_label(theme::ACCENT, profile_name(&profile));
                                            ui.label("vs");
                                        }
                                        // The heading above describes the drawn
                                        // line alone. When the output is trimming
                                        // that one, the two are not the same kind
                                        // of curve, and the legend has to say so.
                                        ui.colored_label(
                                            theme::MUTED,
                                            if running && hptf.auto_trim_db != 0.0 {
                                                format!("{} · as written", profile_name(path))
                                            } else {
                                                profile_name(path)
                                            },
                                        );
                                    });
                                }
                            }
                            // A profile nobody is pointing at gets no second
                            // line; one being pointed at says so quietly, the
                            // way its read errors already do.
                            if let Some(note) = ghost.and_then(|drawing| drawing.disagreement.as_deref())
                            {
                                ui.colored_label(theme::MUTED, note);
                            }
                            if let Some(Err(error)) = drawn.map(|drawing| &drawing.curve) {
                                ui.colored_label(
                                    theme::WARNING,
                                    format!("Cannot read this profile: {error}"),
                                );
                            }
                            // The parse comparison wins over the running one: it
                            // names the band, it does not wait for playback, and
                            // when the two parses differ the running comparison
                            // would only report the same fault as a moved peak.
                            if let Some(note) = drawn.and_then(|drawing| drawing.disagreement.as_deref())
                            {
                                ui.colored_label(theme::WARNING, note);
                            } else if running
                                && let Some(curve) = &drawn_curve
                                && let Some(note) = hptf_disagreement(curve, &hptf)
                            {
                                ui.colored_label(theme::WARNING, note);
                            }
                            if running {
                                ui.label(format!(
                                    "In use · {} bands · preamp {:+.1} dB{}",
                                    hptf.bands,
                                    hptf.preamp_db,
                                    if hptf.auto_trim_db == 0.0 {
                                        String::new()
                                    } else {
                                        format!(" · trimmed {:+.1} dB", hptf.auto_trim_db)
                                    }
                                ));
                                if hptf.max_response_db > 0.0 {
                                    ui.colored_label(
                                        theme::WARNING,
                                        format!(
                                            "Peaks {:+.1} dB above full scale; the output ceiling will pull it back.",
                                            hptf.max_response_db
                                        ),
                                    );
                                }
                            } else if !settings.hptf.is_empty() {
                                ui.label("Selected; not yet running.");
                            }
                        }
                        ui.separator();
                        if matches!(
                            mode,
                            SpatialBackendKind::SafBinaural
                                | SpatialBackendKind::WindowsSpatialAudio
                        ) {
                            ui.horizontal(|ui| {
                                ui.label("Head orientation");
                                egui::ComboBox::from_id_salt("head-source")
                                    .selected_text(settings.head_source.label())
                                    .show_ui(ui, |ui| {
                                        for source in crate::head_tracking::HeadSource::ALL {
                                            ui.add_enabled_ui(
                                                source != crate::head_tracking::HeadSource::AirPods
                                                    || cfg!(target_os = "macos"),
                                                |ui| {
                                                    ui.selectable_value(
                                                        &mut settings.head_source,
                                                        source,
                                                        source.label(),
                                                    );
                                                },
                                            );
                                        }
                                    });
                            });
                            let mut angles = head.pose.euler();
                            let mut changed = false;
                            ui.horizontal(|ui| {
                                for (index, label) in
                                    ["Yaw", "Pitch", "Roll"].into_iter().enumerate()
                                {
                                    ui.label(label);
                                    let limit = if index == 1 { 85.0 } else { 180.0 };
                                    changed |= ui
                                        .add(
                                            egui::DragValue::new(&mut angles[index])
                                                .speed(0.5)
                                                .range(-limit..=limit)
                                                .suffix("°"),
                                        )
                                        .changed();
                                }
                            });
                            let (rect, response) = ui
                                .allocate_exact_size(egui::vec2(370.0, 54.0), egui::Sense::drag());
                            ui.painter().rect_filled(rect, 4.0, theme::SURFACE);
                            ui.painter().text(
                                rect.center(),
                                Align2::CENTER_CENTER,
                                "Drag here to turn your head",
                                egui::FontId::proportional(12.0),
                                theme::MUTED,
                            );
                            if response.dragged() {
                                let delta = ui.input(|input| input.pointer.delta());
                                angles[0] -= delta.x * 0.35;
                                angles[1] = (angles[1] - delta.y * 0.35).clamp(-85.0, 85.0);
                                changed = true;
                            }
                            if changed {
                                manual = Some(angles);
                                settings.head_source = crate::head_tracking::HeadSource::Manual;
                            }
                            recenter = ui.button("Recenter").clicked();
                            ui.label(head.status.label());
                        } else {
                            ui.label("Head orientation is controlled by the system spatializer.");
                        }
                    },
                );
            });
        self.output_settings_open = open;
        self.change_output_settings(settings, context);
        if let Some(angles) = manual {
            self.output.manual_head(angles);
            self.preferences.manual_head = angles;
        }
        if recenter {
            self.output.recenter_head();
            self.preferences.manual_head = [0.0; 3];
        }
    }

    /// The meter bank: one row per object, beside the scene rather than in it.
    ///
    /// The scene answers "where is the sound", and everything it draws has to
    /// survive being rotated, occluded and shared with nineteen other objects.
    /// This panel answers the questions that do not fit in there — exactly how
    /// loud, how loud was the peak, did it clip, and how does the level compare
    /// with the gain the metadata asked for — by giving up position entirely
    /// and taking a fixed row instead. Neither view is a smaller copy of the
    /// other.
    ///
    /// It is off by default: it costs the scene 240 points of width, which on a
    /// narrow window is the difference between a room and a corridor. The
    /// measurement itself runs regardless, so opening the panel mid-stream
    /// shows the objects as they are rather than starting from nothing.
    fn draw_meter_bank(
        &mut self,
        root: &mut egui::Ui,
        mirror_frame: Option<&crate::scene_view::SceneViewFrame>,
    ) {
        if !self.meter_bank_open {
            return;
        }
        let readout = self.meter_readout;
        egui::Panel::right("meter-bank")
            .exact_size(240.0)
            .resizable(false)
            // The frame already provides a clipped border. The automatic
            // separator is painted on the parent and can cross the transport.
            .show_separator_line(false)
            .frame(
                egui::Frame::NONE
                    .fill(theme::SURFACE)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::same(18)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("METER BANK")
                            .size(10.0)
                            .strong()
                            .color(theme::MUTED),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // The unit is the whole button: there are two of them,
                        // and the one not shown is the one a click gives you.
                        if ui
                            .add_sized(
                                [58.0, 22.0],
                                egui::Button::new(
                                    RichText::new(readout.unit())
                                        .size(10.0)
                                        .strong()
                                        .color(theme::TEXT),
                                ),
                            )
                            .on_hover_text(format!(
                                "Showing {}\n\nClick for {}",
                                readout.description(),
                                readout.other().description()
                            ))
                            .clicked()
                        {
                            self.meter_readout = readout.other();
                        }
                    });
                });
                // Both ends of the scale, in the unit on screen and through the
                // same two functions the rows use, so the legend cannot come to
                // describe a scale the bars are not drawn on.
                let (floor_sign, floor) = decibel_cells(Some(readout_decibels(
                    scene3d::params::OBJECT_SILENT_GAIN,
                    readout,
                )));
                let (top_sign, top) = decibel_cells(Some(readout_decibels(1.0, readout)));
                // Two lines on purpose. Left to wrap on its own this breaks in
                // the middle of "line = peak", which reads as three marks named
                // badly rather than as a scale and a key.
                ui.label(
                    RichText::new(format!(
                        "{}{floor} → {}{top} {}",
                        floor_sign.trim(),
                        top_sign.trim(),
                        readout.unit(),
                    ))
                    .size(10.0)
                    .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("tick = gain · line = peak · red = clip")
                        .size(10.0)
                        .color(theme::MUTED),
                );
                ui.add_space(10.0);

                let objects =
                    mirror_frame.map_or(&[][..], crate::scene_view::SceneViewFrame::objects);
                if objects.is_empty() {
                    ui.label(
                        RichText::new("Nothing playing")
                            .size(11.0)
                            .color(theme::MUTED),
                    );
                    return;
                }
                egui::ScrollArea::vertical()
                    .id_salt("meter-bank-rows")
                    .max_height(ui.available_height().max(0.0))
                    .min_scrolled_height(0.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.draw_meter_rows(ui, objects, readout);
                        let hidden = mirror_frame
                            .map_or(0, crate::scene_view::SceneViewFrame::hidden_objects);
                        if hidden > 0 {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(format!("+{hidden} beyond the view limit"))
                                    .size(10.0)
                                    .color(theme::MUTED),
                            );
                        }
                    });
            });
    }

    /// One row per object: number, track, readout.
    fn draw_meter_rows(
        &self,
        ui: &mut egui::Ui,
        objects: &[crate::scene_view::ObjectView],
        readout: MeterReadout,
    ) {
        /// Row height in points, before the theme's inter-row spacing.
        const ROW_HEIGHT: f32 = 18.0;
        /// Width reserved for the row number, wide enough for two digits.
        const NUMBER_CELL: f32 = 16.0;
        const GAP: f32 = 8.0;

        let font = egui::FontId::monospace(scene3d::params::NAMEPLATE_TEXT_POINTS);
        let measure = |text: String| {
            ui.painter()
                .layout_no_wrap(text, font.clone(), theme::TEXT)
                .rect
                .width()
        };
        // Measured from the widest readout the scale can produce, exactly as
        // the nameplate is, so the two columns of digits line up.
        let cell = measure("0".to_owned());
        let readout_width = measure("0".repeat(scene3d::params::NAMEPLATE_CELLS));

        for (slot, object) in objects.iter().enumerate() {
            let (level, peak, clipped) = self.object_meters.bank_row(slot, readout);
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), ROW_HEIGHT),
                egui::Sense::hover(),
            );
            let silent = level < scene3d::params::OBJECT_SILENT_GAIN;
            let decibels = readout_decibels(level, readout);
            let (sign, magnitude) = decibel_cells((!silent).then_some(decibels));

            let painter = ui.painter();
            painter.text(
                egui::pos2(rect.left() + NUMBER_CELL, rect.center().y),
                Align2::RIGHT_CENTER,
                object_display_number(slot).to_string(),
                font.clone(),
                // An inactive element has metadata the renderer would not
                // spatialize; its level is still real, its identity is not.
                if object.active {
                    theme::TEXT
                } else {
                    theme::MUTED
                },
            );

            let track = egui::Rect::from_min_max(
                egui::pos2(rect.left() + NUMBER_CELL + GAP, rect.top() + 3.0),
                egui::pos2(rect.right() - readout_width - GAP, rect.bottom() - 3.0),
            );
            draw_meter_track(painter, track, level, object.gain, peak, clipped);

            let colour = if clipped {
                theme::WARNING
            } else if silent {
                theme::MUTED
            } else {
                theme::TEXT
            };
            painter.text(
                egui::pos2(rect.right() - readout_width, rect.center().y),
                Align2::LEFT_CENTER,
                sign,
                font.clone(),
                colour,
            );
            // Infinity has no decimal point to align, so it sits against the
            // sign instead of against the last cell — the same rule the
            // nameplate follows, for the same reason.
            if silent {
                painter.text(
                    egui::pos2(rect.right() - readout_width + cell, rect.center().y),
                    Align2::LEFT_CENTER,
                    magnitude,
                    font.clone(),
                    colour,
                );
            } else {
                painter.text(
                    egui::pos2(rect.right(), rect.center().y),
                    Align2::RIGHT_CENTER,
                    magnitude,
                    font.clone(),
                    colour,
                );
            }

            response.on_hover_text(meter_row_tooltip(slot, object, peak, clipped, readout));
        }
    }

    fn draw_source_sidebar(&mut self, root: &mut egui::Ui) {
        egui::Panel::left("source-sidebar")
            .exact_size(310.0)
            .resizable(false)
            .frame(
                egui::Frame::NONE
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::same(18)),
            )
            .show(root, |ui| {
                // Noto Sans CJK makes the complete ready-state card about 249 points tall.
                const INFO_BLOCK_HEIGHT: f32 = 256.0;
                const BLOCK_GAP: f32 = 18.0;

                let available = ui.available_rect_before_wrap();
                let info_rect = egui::Rect::from_min_max(
                    egui::pos2(available.left(), available.bottom() - INFO_BLOCK_HEIGHT),
                    available.right_bottom(),
                );
                let source_rect = egui::Rect::from_min_max(
                    available.min,
                    egui::pos2(available.right(), info_rect.top() - BLOCK_GAP),
                );

                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(source_rect)
                        .layout(Layout::top_down(Align::Min)),
                    |ui| {
                        section_title(ui, "SOURCE");
                        card(ui, |ui| self.draw_source_card(ui));
                    },
                );
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(info_rect)
                        .layout(Layout::top_down(Align::Min)),
                    |ui| self.draw_bitstream_info(ui),
                );
            });
    }

    fn draw_bitstream_info(&mut self, ui: &mut egui::Ui) {
        section_title(ui, "BITSTREAM INFO");
        let source = self.selected_source();
        let state = source.and_then(|source| self.inspection.state(source.path()));
        let action = card(ui, |ui| bitstream_ui::draw_card(ui, source, state));
        self.handle_bitstream_action(action);
    }

    fn draw_source_card(&mut self, ui: &mut egui::Ui) {
        // Allocate the entire card once. The footer has its own fixed slot,
        // so extra controls or virtual rows cannot push it into BITSTREAM INFO.
        let bounds = ui.available_rect_before_wrap();
        ui.set_min_size(bounds.size());
        if let Some(action) = playlist_ui::header(
            ui,
            &self.library.summaries,
            self.library.desired_browse,
            &mut self.playlist_ui,
        ) {
            self.handle_playlist_action(action);
        }
        ui.separator();
        let status_text = self
            .library
            .error
            .as_deref()
            .unwrap_or(&self.library.message)
            .lines()
            .next()
            .unwrap_or_default();
        let status_height = ui
            .painter()
            .layout_no_wrap(
                status_text.to_owned(),
                egui::FontId::proportional(10.0),
                theme::MUTED,
            )
            .size()
            .y
            + if self.library.error.is_some() {
                4.0
            } else {
                0.0
            };
        let footer_height =
            ui.spacing().interact_size.y + ui.spacing().item_spacing.y + status_height + 2.0;
        let footer = egui::Rect::from_min_max(
            egui::pos2(
                bounds.left(),
                (bounds.bottom() - footer_height).max(ui.cursor().top()),
            ),
            bounds.right_bottom(),
        );
        let rows = egui::Rect::from_min_max(
            ui.cursor().min,
            egui::pos2(
                bounds.right(),
                (footer.top() - ui.spacing().item_spacing.y).max(ui.cursor().top()),
            ),
        );
        if let Some(list) = self.library.browse.clone() {
            for action in playlist_ui::contents(
                ui,
                rows,
                &list,
                &mut self.browse,
                self.cursor.as_ref(),
                &self.library.media_errors,
            ) {
                self.handle_playlist_action(action);
            }
        } else {
            ui.put(rows, egui::Label::new("Loading playlist…"));
        }
        let mut footer_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(footer)
                .layout(Layout::top_down(Align::Min)),
        );
        footer_ui.set_clip_rect(footer.intersect(ui.clip_rect()));
        footer_ui.add_enabled_ui(self.library.ready, |ui| {
            for action in playlist_ui::actions(ui, &self.library.summaries, &self.browse) {
                self.handle_playlist_action(action);
            }
        });
        self.draw_library_status(&mut footer_ui, status_height);
    }

    fn draw_library_status(&mut self, ui: &mut egui::Ui, status_height: f32) {
        if let Some(error) = self.library.error.clone() {
            ui.spacing_mut().interact_size.y = status_height;
            ui.spacing_mut().button_padding.y = 2.0;
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Retry").size(10.0)).clicked() {
                    self.library.retry();
                }
                ui.add(
                    egui::Label::new(
                        RichText::new(error.lines().next().unwrap_or_default())
                            .size(10.0)
                            .color(theme::WARNING),
                    )
                    .truncate(),
                )
                .on_hover_text(error);
            });
        } else {
            ui.add(
                egui::Label::new(
                    RichText::new(&self.library.message)
                        .size(10.0)
                        .color(theme::MUTED),
                )
                .truncate(),
            )
            .on_hover_text(&self.library.message);
        }
    }

    fn draw_scene(
        &mut self,
        root: &mut egui::Ui,
        mirror_frame: Option<&crate::scene_view::SceneViewFrame>,
    ) {
        let decoder = self.decoder.snapshot().clone();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::same(20)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(RichText::new("Object scene").color(theme::TEXT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_sized([36.0, 30.0], egui::Button::new("..."))
                            .on_hover_text("Open diagnostics")
                            .clicked()
                        {
                            self.diagnostics_open = true;
                        }
                        ui.add_space(4.0);
                        let format = decoder.metrics().map_or_else(
                            || "48 kHz · planar f32".to_owned(),
                            |metrics| format!("{} Hz · planar f32", metrics.sample_rate()),
                        );
                        ui.label(RichText::new(format).size(11.0).color(theme::MUTED));
                    });
                });
                ui.add_space(10.0);

                metric_strip(ui, &decoder);
                self.draw_tracking_status(ui);

                ui.add_space(16.0);
                self.draw_stage(ui, &decoder, mirror_frame);
            });
    }

    fn draw_tracking_status(&mut self, ui: &mut egui::Ui) {
        let Some(frame) = self.output.scene_view().read(self.decoder.playback_key()) else {
            return;
        };
        let tracking = frame.tracking();
        if tracking.unsupported > 0 {
            ui.colored_label(
                theme::WARNING,
                format!(
                    "{} object(s) use fixed-scene fallback · see diagnostics",
                    tracking.unsupported
                ),
            );
        }
        if tracking.head_relative > 0
            && self.output.settings().mode.resolved() == SpatialBackendKind::SystemSpatial
        {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(
                    theme::WARNING,
                    "This output cannot keep individual sounds attached to your head.",
                );
                if ui
                    .add_enabled(
                        !self.output.settings_pending() && self.pending_output_change.is_none(),
                        egui::Button::new("Use software binaural"),
                    )
                    .clicked()
                {
                    let mut settings = self.output.settings().clone();
                    settings.mode = SpatialBackendKind::SafBinaural;
                    self.change_output_settings(settings, ui.ctx());
                }
            });
        }
    }

    /// The object scene stage.
    ///
    /// The `STAGE`-filled, hairline-bordered frame is unchanged from when this
    /// was a text-only box: the 3D view paints onto the same ground so there is
    /// no seam, and it deliberately gets no darker "viewport" backdrop of its
    /// own — that is the surest way to break the paper metaphor.
    ///
    fn draw_stage(
        &mut self,
        ui: &mut egui::Ui,
        decoder: &DecoderSnapshot,
        mirror_frame: Option<&crate::scene_view::SceneViewFrame>,
    ) {
        let silent_objects = mirror_frame.and_then(|mirrored| {
            self.fade_silent_objects
                .then(|| self.object_meters.faded_out(mirrored.objects().len()))
        });
        draw_tracking_counts(
            ui,
            mirror_frame.map(crate::scene_view::SceneViewFrame::tracking),
            silent_objects,
        );
        ui.add_space(6.0);
        let available_height = ui.available_height();
        let frame = egui::Frame::NONE
            .fill(theme::STAGE)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .inner_margin(egui::Margin::same(18));
        let margins = frame.total_margin();
        let content_height = (available_height - margins.top - margins.bottom).max(180.0);
        // The mirror is gated on the decoder's current playback key, so a frame
        // written before a seek, a source change or a device recovery is
        // rejected rather than drawn and the stage falls back to an empty room.
        // Off Windows nothing ever writes it, so that is the permanent state.
        //
        // These are locals because a SceneObject borrows its trail out of the
        // frame, which on `self` would be a self-referential struct. The array
        // is sized to the object budget, so it costs no allocation either way.
        let mut objects =
            [scene3d::scene::SceneObject::default(); crate::scene_view::MAX_VIEW_OBJECTS];
        let mut hidden_objects = 0usize;
        let mut object_count = 0usize;
        let levels = *self.object_meters.levels();
        if let Some(mirrored) = mirror_frame {
            hidden_objects = mirrored.hidden_objects();
            for (slot, object) in mirrored.objects().iter().enumerate() {
                objects[slot] = scene3d::scene::SceneObject {
                    display_number: object_display_number(slot),
                    position: object.position,
                    active: object.active,
                    gain: object.gain,
                    loudness: levels.get(slot).copied().unwrap_or(0.0),
                    presence: self
                        .object_meters
                        .drawn_presence(slot, self.fade_silent_objects),
                    head_locked: object.tracking.head_locked(),
                    trail: mirrored.trail(slot),
                    trail_jumps: mirrored.trail_jumps(slot),
                    trail_loudness: mirrored.trail_loudness(slot),
                };
                object_count = object_count.saturating_add(1);
            }
        }
        let objects = &objects[..object_count];
        frame.show(ui, |ui| {
            ui.set_min_height(content_height);
            // The stage claims drag and scroll itself so the camera never fights
            // the surrounding panel for the pointer.
            let response = ui.allocate_rect(
                egui::Rect::from_min_size(
                    ui.cursor().min,
                    egui::vec2(ui.available_width(), content_height),
                ),
                egui::Sense::drag(),
            );
            let rect = response.rect;
            ui.advance_cursor_after_rect(rect);

            self.drive_camera(ui, &response);

            if self.scene_renderer_ready && rect.width() > 0.0 && rect.height() > 0.0 {
                scene3d::scene::build(
                    &mut self.scene_mesh,
                    &self.camera,
                    rect.height(),
                    scene3d::scene::SceneInput {
                        objects,
                        show_element_numbers: self.object_numbers_visible,
                        show_loudness: self.object_loudness_visible,
                        // Not from the mirror: the presentation's LFE layout is
                        // known as soon as the decoder reports metrics, well
                        // before the render callback produces a first quantum,
                        // and the slot should be drawn correctly from then on.
                        has_lfe: decoder.metrics().is_some_and(DecodeMetrics::has_lfe),
                        figure: self.figure,
                        skin: self.skins.active.as_ref(),
                    },
                );
                let matrix = self.camera.view_projection(rect.width() / rect.height());
                let callback = scene3d::gpu::SceneCallback::new(&self.scene_mesh, matrix);
                if !callback.is_empty() {
                    ui.painter().add(callback.into_shape(rect));
                }
            }

            if self.object_loudness_visible {
                self.draw_object_nameplates(ui, rect, objects);
            }
            self.draw_camera_presets(ui, rect);
            self.draw_camera_readout(ui, rect, hidden_objects);
        });
    }

    /// The floating loudness readout above each object.
    ///
    /// Screen space, because `MeshBuilder` emits world geometry only and a
    /// readout has to stay the same size at any zoom. That costs the depth test
    /// the face numbers get for free, which is the trade this carries
    /// deliberately: the plate reports loudness and nothing else, so it never
    /// has to be big enough for an identity as well, and the number that says
    /// *which* object this is stays printed on the cube's six faces where the
    /// depth buffer still hides the ones behind it. Neither toggle moves the
    /// other; each reading has one home.
    fn draw_object_nameplates(
        &self,
        ui: &egui::Ui,
        stage: egui::Rect,
        objects: &[scene3d::scene::SceneObject<'_>],
    ) {
        let font = egui::FontId::monospace(scene3d::params::NAMEPLATE_TEXT_POINTS);
        let pad = scene3d::params::NAMEPLATE_PAD_POINTS;
        let strip = scene3d::params::NAMEPLATE_STRIP_POINTS;
        // Measured from the widest readout the scale can produce, once, rather
        // than from the value each plate happens to show.
        let widest = "0".repeat(scene3d::params::NAMEPLATE_CELLS);
        let measure = |text: String| {
            ui.painter()
                .layout_no_wrap(text, font.clone(), theme::TEXT)
                .rect
                .width()
        };
        let cell = measure("0".to_owned());
        let plate_width = measure(widest) + pad * 2.0;
        let plate_height = scene3d::params::NAMEPLATE_TEXT_POINTS + strip + 6.0;

        // Farthest first, so a nearer plate lands on top of the one behind it.
        // Without a depth buffer this is the only ordering available, and it is
        // at least the ordering the objects themselves have.
        let direction = self.camera.direction();
        let mut order: Vec<usize> = (0..objects.len()).collect();
        order.sort_by(|left, right| {
            let depth = |index: usize| {
                let position = scene3d::scene::object_world_position(objects[index].position);
                position[0] * direction[0] + position[1] * direction[1] + position[2] * direction[2]
            };
            depth(*left).total_cmp(&depth(*right))
        });

        let painter = ui.painter().with_clip_rect(stage);
        for index in order {
            let object = &objects[index];
            if !object.active {
                continue;
            }
            let [x, y, z] = scene3d::scene::object_world_position(object.position);
            let anchor = self.camera.project(
                [
                    x,
                    y + scene3d::params::OBJECT_EDGE / 2.0 + scene3d::params::NAMEPLATE_OFFSET,
                    z,
                ],
                stage,
            );
            // A plate that says nothing but -∞ forever is what crowds the
            // view, so it steps back as soon as it has nothing to report and,
            // once the fade has carried it all the way out, is not laid out at
            // all. The count above the stage is where that object is accounted
            // for from then on.
            let silent = object.loudness < scene3d::params::OBJECT_SILENT_GAIN;
            let alpha = plate_alpha(silent, object.presence);
            if alpha <= 0.0 {
                continue;
            }
            let plate = egui::Rect::from_min_size(
                egui::pos2(anchor.x - plate_width / 2.0, anchor.y - plate_height),
                egui::vec2(plate_width, plate_height),
            );
            painter.rect_filled(plate, 2.5, fade(theme::INK, 0.70 * alpha));

            // The sign owns its own cell and the digits are right-aligned
            // against the last, so neither walks sideways as the level moves.
            // Infinity has no decimal point to align, so it sits next to the
            // sign instead — which is what keeps the sign itself still in every
            // state the readout has.
            let decibels = readout_decibels(object.loudness, MeterReadout::Fast);
            let baseline = plate.top() + (plate.height() - strip) / 2.0;
            let text_colour = if decibels > -0.5 && !silent {
                fade(theme::WARNING, alpha)
            } else {
                fade(theme::BACKGROUND, alpha)
            };
            let (sign, magnitude) = decibel_cells((!silent).then_some(decibels));
            painter.text(
                egui::pos2(plate.left() + pad, baseline),
                Align2::LEFT_CENTER,
                sign,
                font.clone(),
                text_colour,
            );
            if silent {
                painter.text(
                    egui::pos2(plate.left() + pad + cell, baseline),
                    Align2::LEFT_CENTER,
                    magnitude,
                    font.clone(),
                    text_colour,
                );
            } else {
                painter.text(
                    egui::pos2(plate.right() - pad, baseline),
                    Align2::RIGHT_CENTER,
                    magnitude,
                    font.clone(),
                    text_colour,
                );
            }

            // The same value as a strip, which is what makes a row of plates
            // scannable without reading every digit.
            let filled = plate_width * loudness_fraction(object.loudness);
            let track = egui::Rect::from_min_size(
                egui::pos2(plate.left(), plate.bottom() - strip),
                egui::vec2(plate_width, strip),
            );
            painter.rect_filled(track, 0.0, fade(theme::INK, 0.35 * alpha));
            painter.rect_filled(
                egui::Rect::from_min_size(track.min, egui::vec2(filled, strip)),
                0.0,
                fade(theme::ACCENT, alpha),
            );
        }
    }

    /// Live camera state, bottom-left of the stage.
    ///
    /// `faces` is the number of an axis-aligned box's faces still turned toward
    /// the viewer. It drops below three as the view approaches an axis, which is
    /// exactly when three-tone shading stops separating faces and the projection
    /// stops carrying depth — so it is worth showing rather than discovering.
    fn draw_camera_readout(&self, ui: &mut egui::Ui, stage: egui::Rect, hidden_objects: usize) {
        let faces = scene3d::mesh::visible_face_count(self.camera.direction());
        if hidden_objects > 0 {
            // The object array is fixed so the audio thread never allocates, so
            // a scene past the budget is genuinely incomplete on screen and has
            // to admit it rather than quietly showing the first twenty.
            ui.painter().text(
                egui::pos2(stage.left(), stage.bottom() - 18.0),
                Align2::LEFT_BOTTOM,
                format!("+{hidden_objects} more not shown"),
                egui::FontId::monospace(10.0),
                theme::WARNING,
            );
        }
        let text = format!(
            "{}   az {:.0}°   el {:.0}°   zoom {:.2}   faces/box {faces}",
            self.camera.projection_mode().label(),
            self.camera.azimuth_degrees(),
            self.camera.elevation_degrees(),
            self.camera.ortho_height(),
        );
        ui.painter().text(
            egui::pos2(stage.left(), stage.bottom() - 4.0),
            Align2::LEFT_BOTTOM,
            text,
            egui::FontId::monospace(10.0),
            if faces < 3 {
                theme::WARNING
            } else {
                theme::MUTED
            },
        );
    }

    /// Orbit, zoom and pan. Every angle stays reachable — this is an inspection
    /// tool, and a single fixed viewpoint leaves object positions ambiguous.
    fn drive_camera(&mut self, ui: &egui::Ui, response: &egui::Response) {
        let viewport_height = response.rect.height();
        if response.dragged() {
            self.camera.cancel_animation();
            let delta = response.drag_delta();
            if ui.input(|input| input.modifiers.shift) {
                self.camera.pan([delta.x, delta.y], viewport_height);
            } else {
                self.camera.orbit([delta.x, delta.y]);
            }
        }
        if response.drag_stopped() && !ui.input(|input| input.modifiers.shift) {
            self.camera.snap_if_near();
        }
        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll.abs() > f32::EPSILON {
                self.camera.cancel_animation();
                self.camera.zoom(-scroll);
            }
        }

        let delta_seconds = ui.input(|input| input.stable_dt);
        if self.camera.advance(delta_seconds) {
            ui.ctx().request_repaint();
        }
    }

    /// Named viewpoints, in the theme's own square hairline idiom — no floating
    /// gizmo, no rounded HUD.
    fn draw_camera_presets(&mut self, ui: &mut egui::Ui, stage: egui::Rect) {
        use scene3d::camera::{Preset, ProjectionMode};

        let projection_mode = self.camera.projection_mode();
        let projection_hint = match projection_mode {
            ProjectionMode::Orthographic => "Switch to perspective projection",
            ProjectionMode::Perspective => "Switch to orthographic projection",
        };
        let buttons = [
            ("ISO", Some(Preset::Iso)),
            ("TOP", Some(Preset::Top)),
            ("BACK", Some(Preset::Back)),
            ("SIDE", Some(Preset::Side)),
            ("RESET", None),
        ];
        let strip = egui::Rect::from_min_size(
            egui::pos2(stage.left(), stage.top()),
            egui::vec2(stage.width(), 30.0),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(strip)
                .layout(Layout::right_to_left(Align::Min)),
            |ui| {
                if ui
                    .add_sized(
                        [58.0, 26.0],
                        egui::Button::new(
                            RichText::new(projection_mode.label())
                                .size(10.0)
                                .strong()
                                .color(theme::MUTED),
                        ),
                    )
                    .on_hover_text(projection_hint)
                    .clicked()
                {
                    self.camera.toggle_projection();
                }
                ui.add_space(4.0);

                for (label, preset) in buttons.into_iter().rev() {
                    if ui
                        .add_sized(
                            [52.0, 26.0],
                            egui::Button::new(
                                RichText::new(label).size(10.0).strong().color(theme::MUTED),
                            ),
                        )
                        .clicked()
                    {
                        if let Some(preset) = preset {
                            self.camera.apply_preset(
                                preset,
                                stage.width() / stage.height().max(f32::EPSILON),
                            );
                        } else {
                            self.camera.reset_view();
                        }
                    }
                }
            },
        );
    }

    fn draw_diagnostics_window(&mut self, context: &egui::Context) {
        if !self.diagnostics_open {
            return;
        }

        let decoder = self.decoder.snapshot().clone();
        let output = self.output.snapshot().clone();
        let tracking = self
            .output
            .scene_view()
            .read(self.decoder.playback_key())
            .map(|frame| frame.tracking());
        let remains_open = context.show_viewport_immediate(
            egui::ViewportId::from_hash_of("playback-diagnostics"),
            egui::ViewportBuilder::default()
                .with_title("MacinDecode AC-4 Diagnostics")
                .with_icon(crate::app_icon::load())
                .with_inner_size([460.0, 390.0])
                .with_min_inner_size([400.0, 300.0]),
            |root, _class| {
                let close_requested = root.ctx().input(|input| input.viewport().close_requested());
                draw_diagnostics_content(root, self.backend, &decoder, &output, tracking);
                !close_requested
            },
        );
        self.diagnostics_open = remains_open;
    }

    fn draw_bitstream_details_window(&mut self, context: &egui::Context) {
        if !self.bitstream_details_open {
            return;
        }
        let Some(source) = self.selected_source() else {
            self.bitstream_details_open = false;
            return;
        };
        let state = self.inspection.state(source.path());
        let mut requested_action = None;
        let remains_open = context.show_viewport_immediate(
            egui::ViewportId::from_hash_of("bitstream-details"),
            egui::ViewportBuilder::default()
                .with_title("MacinDecode AC-4 Bitstream Details")
                .with_icon(crate::app_icon::load())
                .with_inner_size([760.0, 680.0])
                .with_min_inner_size([560.0, 420.0]),
            |root, _class| {
                let close_requested = root.ctx().input(|input| input.viewport().close_requested());
                requested_action = bitstream_ui::draw_details(root, source, state);
                !close_requested
            },
        );
        self.bitstream_details_open = remains_open;
        self.handle_bitstream_action(requested_action);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "transport layout and its immediate seek actions are intentionally colocated"
    )]
    fn draw_transport(&mut self, root: &mut egui::Ui) {
        let output_phase = self.output.snapshot().phase();
        let retry_failed_decode =
            self.decoder.snapshot().phase() == DecodePhase::Failed && self.cursor.is_some();
        let can_toggle = (self.cursor.is_none() && self.selected_source().is_some())
            || self.resume.is_some()
            || self.output.snapshot().can_play()
            || (output_phase == OutputPhase::Ended && self.cursor.is_some())
            || retry_failed_decode;
        let playing = self.output.snapshot().is_playing();
        let can_previous = self.can_select_neighbor(PlaylistStep::Previous);
        let can_next = self.can_select_neighbor(PlaylistStep::Next);
        let mut selected_mode = self.effective_playback_mode();
        let (duration_frames, sample_rate, seekable) =
            self.decoder
                .snapshot()
                .metrics()
                .map_or((None, 0, false), |metrics| {
                    (
                        metrics.duration_frames(),
                        metrics.sample_rate(),
                        metrics.seekable_from_frame().is_some(),
                    )
                });
        let (action, timeline) = egui::Panel::bottom("transport")
            .exact_size(136.0)
            .frame(
                egui::Frame::NONE
                    .fill(theme::SURFACE)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::symmetric(22, 14)),
            )
            .show(root, |ui| {
                let mut action = None;
                let mut timeline = TimelineInteraction::default();
                let (content, _) =
                    ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
                let status_rect =
                    egui::Rect::from_min_size(content.min, egui::vec2(content.width(), 20.0));
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(status_rect)
                        .layout(Layout::left_to_right(Align::Center)),
                    |ui| {
                        ui.colored_label(self.status.color(), "●");
                        ui.label(RichText::new(&self.status.text).color(theme::MUTED));
                    },
                );

                let control_height = 34.0 + ui.spacing().item_spacing.y + 20.0;
                let control_rect = egui::Rect::from_center_size(
                    content.center(),
                    egui::vec2(content.width(), control_height),
                );
                let volume_width = if content.width() >= 700.0 {
                    150.0
                } else {
                    120.0
                };
                let side_reserve = volume_width + 20.0;
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(control_rect)
                        .layout(Layout::top_down(Align::Center)),
                    |ui| {
                        action =
                            transport_buttons(ui, [can_previous, can_toggle, can_next], playing);
                        timeline = transport_progress(
                            ui,
                            &mut self.timeline_preview,
                            side_reserve,
                            duration_frames,
                            sample_rate,
                            seekable,
                        );
                    },
                );

                let volume_rect = egui::Rect::from_center_size(
                    egui::pos2(
                        content.right() - volume_width / 2.0,
                        control_rect.bottom() - 10.0,
                    ),
                    egui::vec2(volume_width, 28.0),
                );
                volume_control(ui, volume_rect, &mut self.volume, &mut self.muted);

                let mode_rect = egui::Rect::from_center_size(
                    egui::pos2(
                        content.left() + volume_width / 2.0,
                        control_rect.bottom() - 10.0,
                    ),
                    egui::vec2(volume_width, 28.0),
                );
                ui.add_enabled_ui(
                    self.cursor
                        .as_ref()
                        .is_none_or(|cursor| cursor.playlist.is_some()),
                    |ui| {
                        playback_mode_control(ui, mode_rect, &mut selected_mode);
                    },
                );
                (action, timeline)
            })
            .inner;

        if self.effective_playback_mode() != selected_mode {
            if let Some(id) = self
                .cursor
                .as_ref()
                .and_then(|c| c.playlist)
                .or(self.browse.playlist)
            {
                self.library.mutate(Mutation::Mode(id, selected_mode));
            }
            self.shuffle_history.clear();
            self.status =
                StatusLine::idle(format!("Playback mode: {}", selected_mode.description()));
        }

        self.timeline_dragging = timeline.dragging;
        if let Some(target_frame) = timeline.seek_target {
            self.resume = None;
            self.output.pause();
            match self.decoder.seek(target_frame) {
                Ok(()) => {
                    self.playback_restore_pending = true;
                    self.status = StatusLine::idle(format!(
                        "Seeking to {}",
                        format_timestamp(target_frame, sample_rate)
                    ));
                }
                Err(error) => {
                    self.playback_restore_pending = false;
                    if self.playback_intent {
                        self.output.play();
                    }
                    self.status = StatusLine::warning(error);
                }
            }
        }

        match action {
            Some(TransportAction::Previous) => {
                self.select_neighbor(PlaylistStep::Previous);
            }
            Some(TransportAction::Toggle) if self.cursor.is_none() => {
                if let Some(id) = self.browse.saved.focus {
                    self.play_browsed_entry(id);
                }
            }
            Some(TransportAction::Toggle) if self.resume.is_some() => {
                self.playback_intent = !self.playback_intent;
            }
            Some(TransportAction::Toggle) if retry_failed_decode => {
                self.retry_current_source();
            }
            Some(TransportAction::Toggle) if output_phase == OutputPhase::Ended => {
                self.replay_current_source();
            }
            Some(TransportAction::Toggle) if playing => {
                self.playback_intent = false;
                self.output.pause();
            }
            Some(TransportAction::Toggle) => {
                self.playback_intent = true;
                self.output.play();
            }
            Some(TransportAction::Next) => {
                self.select_neighbor(PlaylistStep::Next);
            }
            None => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportAction {
    Previous,
    Toggle,
    Next,
}

fn transport_buttons(
    ui: &mut egui::Ui,
    enabled: [bool; 3],
    playing: bool,
) -> Option<TransportAction> {
    let [can_previous, can_toggle, can_next] = enabled;
    let button_size = egui::vec2(44.0, 34.0);
    let group_width = button_size.x * 3.0 + ui.spacing().item_spacing.x * 2.0;
    let (row, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), button_size.y),
        egui::Sense::hover(),
    );
    let group = egui::Rect::from_center_size(row.center(), egui::vec2(group_width, button_size.y));
    let mut action = None;
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(group)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            if ui
                .add_enabled(
                    can_previous,
                    egui::Button::new("◀◀")
                        .min_size(button_size)
                        .fill(theme::HOVER),
                )
                .on_hover_text("Previous playlist item")
                .clicked()
            {
                action = Some(TransportAction::Previous);
            }
            let toggle_response = ui
                .add_enabled(
                    can_toggle,
                    egui::Button::new(if playing { "" } else { "▶" })
                        .min_size(button_size)
                        .fill(theme::HOVER),
                )
                .on_hover_text(if playing { "Pause" } else { "Play" });
            if playing {
                paint_pause_icon(ui, toggle_response.rect, can_toggle);
            }
            if toggle_response.clicked() {
                action = Some(TransportAction::Toggle);
            }
            if ui
                .add_enabled(
                    can_next,
                    egui::Button::new("▶▶")
                        .min_size(button_size)
                        .fill(theme::HOVER),
                )
                .on_hover_text("Next playlist item")
                .clicked()
            {
                action = Some(TransportAction::Next);
            }
        },
    );
    action
}

fn playback_mode_control(ui: &mut egui::Ui, rect: egui::Rect, mode: &mut PlaybackMode) {
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().interact_size.y = rect.height();
            egui::ComboBox::from_id_salt("playback-mode")
                .selected_text(mode.label())
                .width(rect.width())
                .show_ui(ui, |ui| {
                    for option in PlaybackMode::ALL {
                        ui.selectable_value(mode, option, option.label())
                            .on_hover_text(option.description());
                    }
                });
        },
    );
}

fn paint_pause_icon(ui: &egui::Ui, button_rect: egui::Rect, enabled: bool) {
    let color = if enabled { theme::TEXT } else { theme::MUTED };
    let center = button_rect.center();
    let bar_size = egui::vec2(3.0, 14.0);
    for offset in [-3.5, 3.5] {
        let bar_center = egui::pos2(center.x + offset, center.y);
        ui.painter().rect_filled(
            egui::Rect::from_center_size(bar_center, bar_size),
            0.0,
            color,
        );
    }
}

#[derive(Default)]
struct TimelineInteraction {
    seek_target: Option<u64>,
    dragging: bool,
}

fn transport_progress(
    ui: &mut egui::Ui,
    timeline_preview: &mut f32,
    side_reserve: f32,
    duration_frames: Option<u64>,
    sample_rate: u32,
    seekable: bool,
) -> TimelineInteraction {
    let row_width = ui.available_width();
    let time_width = 50.0;
    let spacing = ui.spacing().item_spacing.x;
    let max_group_width = (row_width - side_reserve * 2.0).max(260.0);
    let max_progress_width = (max_group_width - time_width * 2.0 - spacing * 2.0).max(140.0);
    let progress_width = (row_width * 0.45)
        .clamp(160.0, 420.0)
        .min(max_progress_width);
    let group_width = progress_width + time_width * 2.0 + ui.spacing().item_spacing.x * 2.0;
    let (row, _) = ui.allocate_exact_size(egui::vec2(row_width, 20.0), egui::Sense::hover());
    let group = egui::Rect::from_center_size(row.center(), egui::vec2(group_width, row.height()));

    let mut interaction = TimelineInteraction::default();
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(group)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            let duration = duration_frames.unwrap_or(0);
            let preview_frame = normalized_frame(*timeline_preview, duration);
            ui.add_sized(
                [time_width, 18.0],
                egui::Label::new(
                    RichText::new(format_timestamp(preview_frame, sample_rate))
                        .monospace()
                        .color(theme::MUTED),
                )
                .halign(Align::RIGHT),
            );
            ui.add_enabled_ui(seekable && duration > 0, |ui| {
                ui.spacing_mut().interact_size.y = 18.0;
                ui.spacing_mut().slider_width = progress_width;
                let response =
                    ui.add(egui::Slider::new(timeline_preview, 0.0..=1.0).show_value(false));
                interaction.dragging = response.dragged() || response.is_pointer_button_down_on();
                if response.drag_stopped() || response.clicked() {
                    interaction.seek_target = Some(normalized_frame(*timeline_preview, duration));
                }
            });
            ui.add_sized(
                [time_width, 18.0],
                egui::Label::new(
                    RichText::new(if duration > 0 {
                        format_timestamp(duration, sample_rate)
                    } else {
                        "--:--".to_owned()
                    })
                    .monospace()
                    .color(theme::MUTED),
                )
                .halign(Align::LEFT),
            );
        },
    );
    interaction
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "the normalized UI slider is clamped to a finite 0..=1 range"
)]
fn normalized_frame(value: f32, duration_frames: u64) -> u64 {
    if !value.is_finite() || duration_frames == 0 {
        return 0;
    }
    (f64::from(value.clamp(0.0, 1.0)) * duration_frames as f64).round() as u64
}

fn format_timestamp(frames: u64, sample_rate: u32) -> String {
    if sample_rate == 0 {
        return "--:--".to_owned();
    }
    let total_seconds = frames / u64::from(sample_rate);
    let seconds = total_seconds % 60;
    let minutes = (total_seconds / 60) % 60;
    let hours = total_seconds / 3_600;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

fn volume_control(ui: &mut egui::Ui, rect: egui::Rect, volume: &mut f32, muted: &mut bool) {
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            if speaker_button(ui, *muted, *volume).clicked() {
                if *muted {
                    if *volume <= f32::EPSILON {
                        *volume = 0.5;
                    }
                    *muted = false;
                } else {
                    *muted = true;
                }
            }

            let slider_width = (rect.width() - 28.0 - ui.spacing().item_spacing.x).max(56.0);
            ui.spacing_mut().interact_size.y = 18.0;
            ui.spacing_mut().slider_width = slider_width;
            let response = ui.add(egui::Slider::new(volume, 0.0..=1.0).show_value(false));
            if response.changed() {
                *muted = *volume <= f32::EPSILON;
            }
            response.on_hover_text(format!("Volume: {:.0}%", *volume * 100.0));
        },
    );
}

fn speaker_button(ui: &mut egui::Ui, muted: bool, volume: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
    let response = response.on_hover_text(if muted { "Unmute" } else { "Mute" });
    let fill = if response.hovered() {
        theme::HOVER
    } else {
        theme::SURFACE
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    ui.painter().rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, theme::BORDER),
        egui::StrokeKind::Inside,
    );

    let center = rect.center();
    let color = if muted { theme::WARNING } else { theme::MUTED };
    ui.painter().rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(center.x - 9.0, center.y - 3.0),
            egui::pos2(center.x - 5.0, center.y + 3.0),
        ),
        0.0,
        color,
    );
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(center.x - 5.0, center.y - 4.0),
            egui::pos2(center.x + 1.0, center.y - 8.0),
            egui::pos2(center.x + 1.0, center.y + 8.0),
            egui::pos2(center.x - 5.0, center.y + 4.0),
        ],
        color,
        Stroke::NONE,
    ));

    if muted {
        ui.painter().line_segment(
            [
                egui::pos2(center.x + 5.0, center.y - 4.0),
                egui::pos2(center.x + 11.0, center.y + 4.0),
            ],
            Stroke::new(1.5, color),
        );
        ui.painter().line_segment(
            [
                egui::pos2(center.x + 11.0, center.y - 4.0),
                egui::pos2(center.x + 5.0, center.y + 4.0),
            ],
            Stroke::new(1.5, color),
        );
    } else if volume > f32::EPSILON {
        ui.painter().line_segment(
            [
                egui::pos2(center.x + 5.0, center.y - 4.0),
                egui::pos2(center.x + 8.0, center.y),
            ],
            Stroke::new(1.5, color),
        );
        ui.painter().line_segment(
            [
                egui::pos2(center.x + 8.0, center.y),
                egui::pos2(center.x + 5.0, center.y + 4.0),
            ],
            Stroke::new(1.5, color),
        );
        if volume > 0.5 {
            ui.painter().line_segment(
                [
                    egui::pos2(center.x + 8.0, center.y - 6.0),
                    egui::pos2(center.x + 12.0, center.y),
                ],
                Stroke::new(1.5, color),
            );
            ui.painter().line_segment(
                [
                    egui::pos2(center.x + 12.0, center.y),
                    egui::pos2(center.x + 8.0, center.y + 6.0),
                ],
                Stroke::new(1.5, color),
            );
        }
    }

    response
}

impl eframe::App for PlayerApp {
    /// The half of the frame that has to keep running with nothing on screen.
    ///
    /// eframe runs **no egui pass at all** while the window is minimized or
    /// hidden — it calls this instead, precisely so that app logic keeps
    /// ticking. With the synchronisation living in [`Self::ui`], a backgrounded
    /// player reached the end of an item and simply stopped: nothing polled the
    /// output, so nothing saw `Ended` and nothing advanced the playlist, and
    /// because the repaint requests are issued from here too, nothing asked for
    /// the pass that would have noticed. The window slept until it was restored.
    ///
    /// eframe calls this immediately before every `ui` as well, so the visible
    /// path keeps the order it always had.
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        // eframe calls `logic` immediately before every `ui`, and *only* `logic`
        // while the window is hidden, so what the flag carries into this call is
        // exactly "was anything drawn since the last one". Reading it beats
        // re-deriving visibility from `ViewportInfo`: the rule that actually
        // decides whether a pass runs is eframe's own, and copying it here would
        // leave two versions to drift apart.
        let showing = std::mem::take(&mut self.stage_drawn);
        self.tick(context, showing);
        if self.library.ready && !self.sofa.started {
            self.sofa.files = self.library.sofa_index.take().unwrap_or_default();
            self.sofa.refresh(None, context);
        }
        if self.library.ready && !self.hptf.started {
            self.hptf.files = self.library.hptf_index.take().unwrap_or_default();
            self.hptf.refresh(None, context);
        }
        if let Some((files, imported)) = self.sofa.poll() {
            self.library
                .save_file_index(crate::file_catalog::SOFA, files);
            if let Some(path) = imported {
                if let Some(path) = path.to_str() {
                    let mut settings = self.output.settings().clone();
                    path.clone_into(&mut settings.sofa);
                    self.change_output_settings(settings, context);
                } else {
                    self.audio_settings_error =
                        Some("This renderer requires a Unicode SOFA path".into());
                }
            }
        }
        if let Some((files, imported)) = self.hptf.poll() {
            self.library
                .save_file_index(crate::file_catalog::HPTF, files);
            if let Some(path) = imported {
                if let Some(path) = path.to_str() {
                    let mut settings = self.output.settings().clone();
                    path.clone_into(&mut settings.hptf);
                    self.change_output_settings(settings, context);
                } else {
                    self.audio_settings_error =
                        Some("This renderer requires a Unicode profile path".into());
                }
            }
        }
    }

    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = root.ctx().clone();
        self.stage_drawn = true;
        // `logic` necessarily sees the previous pass, so its first call after a
        // restore still schedules the hidden 250 ms cadence. A running `ui` is
        // authoritative evidence that the window is visible now; asking for the
        // visible cadence here makes the earliest request win without copying
        // eframe's viewport visibility rule.
        if let Some(delay) =
            output_repaint_delay(self.output.snapshot().phase(), self.playback_intent, true)
        {
            context.request_repaint_after(delay);
        }
        // Deliberately not in `logic`: while the window is hidden the input is
        // frozen at the last shown pass, so re-reading the drop list there would
        // append the same files again on every tick.
        self.accept_dropped_files(&context);
        // Read once for the whole pass. The bank, the counts and the geometry
        // then describe the same presentation-clock instant instead of racing
        // the audio side for three separate snapshots — and the ballistics
        // advance once, which they must, because a second call would charge the
        // same wall time to the meters twice.
        let mirror_frame = self
            .output
            .scene_view()
            .read(self.decoder.playback_key())
            .map(|frame| frame.in_world_space(self.output.head_snapshot().pose));
        if let Some(mirrored) = mirror_frame.as_ref() {
            let sample_rate = self
                .decoder
                .snapshot()
                .metrics()
                .map_or(48_000, DecodeMetrics::sample_rate);
            self.object_meters.advance(
                mirrored,
                self.decoder.playback_key(),
                sample_rate,
                Instant::now(),
            );
        }
        self.draw_header(root);
        self.draw_source_sidebar(root);
        self.draw_transport(root);
        self.draw_meter_bank(root, mirror_frame.as_ref());
        self.draw_scene(root, mirror_frame.as_ref());
        self.draw_bitstream_details_window(&context);
        self.draw_diagnostics_window(&context);
        self.draw_output_settings(&context);
        self.draw_visual_settings(&context);
        self.about.draw(&context);
        if let Some(smoke) = &mut self.smoke {
            smoke.frame(
                &context,
                self.library.ready,
                self.library.error.as_deref(),
                self.scene_renderer_ready,
            );
        }
        for action in
            playlist_ui::management(&context, &self.library.summaries, &mut self.playlist_ui)
        {
            self.handle_playlist_action(action);
        }

        if context.input(|input| !input.raw.hovered_files.is_empty()) {
            draw_drop_overlay(&context);
        }
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.flush_persistence();
    }

    fn on_exit(&mut self) {
        self.sofa.shutdown();
        self.hptf.shutdown();
        self.flush_persistence();
        self.library.shutdown();
        if let Some(error) = &self.library.error {
            rfd::MessageDialog::new()
                .set_title("MacinDecode AC-4 Player")
                .set_description(format!("Some changes could not be saved.\n{error}"))
                .set_level(rfd::MessageLevel::Error)
                .show();
        }
    }
}

struct StatusLine {
    kind: StatusKind,
    text: String,
}

impl StatusLine {
    fn idle(text: impl Into<String>) -> Self {
        Self {
            kind: StatusKind::Idle,
            text: text.into(),
        }
    }

    fn ready(text: impl Into<String>) -> Self {
        Self {
            kind: StatusKind::Ready,
            text: text.into(),
        }
    }

    fn warning(text: impl Into<String>) -> Self {
        Self {
            kind: StatusKind::Warning,
            text: text.into(),
        }
    }

    const fn color(&self) -> Color32 {
        match self.kind {
            StatusKind::Idle => theme::MUTED,
            StatusKind::Ready => theme::SUCCESS,
            StatusKind::Warning => theme::WARNING,
        }
    }
}

enum StatusKind {
    Idle,
    Ready,
    Warning,
}

/// The name a profile goes by on screen. The folder is one line above it and the
/// full path is rarely the useful half.
fn profile_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// A profile read once for drawing, with what the renderer's own parser made of
/// the same text.
struct HptfDrawing {
    path: String,
    rate: u32,
    curve: Result<crate::hptf_profile::Curve, String>,
    /// What the renderer read differently, if anything. `None` also covers
    /// "there is no renderer in this binary to ask" — see `backend::hptf_parse`.
    disagreement: Option<String>,
}

/// The drawing for `path` at `rate`, kept in `slot` and re-read from disk only
/// when one of them changes. An empty path is "no profile", and clears the slot.
///
/// A biquad's response depends on the rate it was designed at, so the rate is
/// half of the key: the same file at two rates is two curves. The cross-check
/// against the renderer's own parser rides along here because it wants the same
/// text and the same once-per-change cadence, and because it needs no output —
/// a profile can be checked while it is being looked at, not only once it plays.
fn hptf_curve<'a>(
    slot: &'a mut Option<HptfDrawing>,
    path: &str,
    rate: u32,
) -> Option<&'a HptfDrawing> {
    if path.is_empty() {
        *slot = None;
        return None;
    }
    if slot
        .as_ref()
        .is_none_or(|drawing| drawing.path != path || drawing.rate != rate)
    {
        let text = std::fs::read_to_string(path).map_err(|error| error.to_string());
        let ours = text
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|text| crate::hptf_profile::Profile::parse(text));
        // Two independent readings of one documented format. All four outcomes
        // say something; only "both read it the same way" says nothing.
        let renderer = text
            .as_ref()
            .ok()
            .and_then(|text| crate::backend::hptf_parse(text));
        let disagreement = match (&ours, &renderer) {
            (Ok(ours), Some(Ok(renderer))) => ours.disagreement(renderer),
            (Ok(_), Some(Err(error))) => {
                Some(format!("The renderer will not read this profile: {error}"))
            }
            (Err(_), Some(Ok(_))) => Some(
                "The renderer reads this profile without complaint, so it is this side's reading of it that is wrong."
                    .to_owned(),
            ),
            _ => None,
        };
        *slot = Some(HptfDrawing {
            path: path.to_owned(),
            rate,
            curve: ours.map(|ours| crate::hptf_profile::Curve::of(&ours, rate)),
            disagreement,
        });
    }
    slot.as_ref()
}

/// Which profile to ghost behind the drawn one.
///
/// Pointing at a row is a question, not a choice: the answer is drawn and
/// nothing reaches the renderer, so comparing two profiles cannot crossfade the
/// audio being judged. Ghosting the drawn profile behind itself would answer
/// nothing, and that is not a corner case — clicking a row leaves the pointer
/// on it, so the frame that selects a profile is also a frame that hovers it.
fn hptf_ghost<'a>(hovered: Option<&'a str>, drawn: &str) -> Option<&'a str> {
    hovered.filter(|path| Path::new(path) != Path::new(drawn))
}

/// The decibel range the plot has to cover, top first.
///
/// Fixed at +3 to -12 dB rather than fitted to the data: two profiles looked at a
/// minute apart have to stay comparable by eye, which a rescaling axis quietly
/// destroys. A curve that runs past either end extends the range instead — and
/// the ghost counts, because a comparison drawn outside the frame is worse than
/// no comparison.
fn hptf_axis(curves: [Option<&crate::hptf_profile::Curve>; 2]) -> (f32, f32) {
    let shown = || curves.into_iter().flatten();
    (
        shown().fold(3.0_f32, |top, curve| top.max(curve.max_response_db.ceil())),
        shown().fold(-12.0_f32, |bottom, curve| bottom.min(curve.floor().floor())),
    )
}

/// The response a profile's cascade applies to the headphone feed, with
/// whatever is being pointed at drawn faintly behind it.
///
/// Drawn with the preamp folded in, so the top rule is full scale and the height
/// of the curve below it is the headroom the preamp bought.
#[allow(
    clippy::cast_precision_loss,
    reason = "the index is bounded by the stored curve's length, far inside \
              what f32 represents exactly"
)]
fn draw_hptf_curve(
    ui: &mut egui::Ui,
    curve: Option<&crate::hptf_profile::Curve>,
    ghost: Option<&crate::hptf_profile::Curve>,
) {
    const LABELS: f32 = 13.0;
    const COLUMNS: usize = 512;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 104.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, theme::STAGE);
    painter.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, theme::BORDER),
        egui::StrokeKind::Inside,
    );

    let plot = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.max.y - LABELS));
    let (top, bottom) = hptf_axis([ghost, curve]);
    let y = |db: f32| plot.top() + (top - db) / (top - bottom) * plot.height();
    let x = |hz: f64| plot.left() + crate::hptf_profile::Curve::position(hz) * plot.width();

    for (hz, label) in [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")] {
        painter.line_segment(
            [
                egui::pos2(x(hz), plot.top()),
                egui::pos2(x(hz), plot.bottom()),
            ],
            Stroke::new(1.0, theme::BORDER),
        );
        painter.text(
            egui::pos2(x(hz), rect.max.y - 1.0),
            Align2::CENTER_BOTTOM,
            label,
            egui::FontId::monospace(8.0),
            theme::MUTED,
        );
    }
    for (hz, align, label) in [
        (crate::hptf_profile::MIN_HZ, Align2::LEFT_BOTTOM, "20 Hz"),
        (crate::hptf_profile::MAX_HZ, Align2::RIGHT_BOTTOM, "20k"),
    ] {
        painter.text(
            egui::pos2(x(hz), rect.max.y - 1.0),
            align,
            label,
            egui::FontId::monospace(8.0),
            theme::MUTED,
        );
    }
    for (db, colour, label) in [
        (-6.0_f32, theme::BORDER, "-6"),
        (0.0, theme::MUTED, "0 dBFS"),
    ] {
        painter.line_segment(
            [
                egui::pos2(plot.left(), y(db)),
                egui::pos2(plot.right(), y(db)),
            ],
            Stroke::new(1.0, colour),
        );
        painter.text(
            egui::pos2(plot.left() + 3.0, y(db) - 1.0),
            Align2::LEFT_BOTTOM,
            label,
            egui::FontId::monospace(8.0),
            theme::MUTED,
        );
    }

    // The stored curve is far denser than any panel is wide, and the extra
    // vertices only cost tessellation. COLUMNS still leaves more than one sample
    // per pixel at the settings window's width.
    let trace = |curve: &crate::hptf_profile::Curve, stroke: Stroke| {
        let last = curve.points.len().saturating_sub(1);
        let step = curve.points.len().div_ceil(COLUMNS).max(1);
        let points: Vec<_> = (0..curve.points.len())
            .step_by(step)
            .chain(std::iter::once(last))
            .map(|index| {
                let across = index as f32 / last.max(1) as f32;
                egui::pos2(plot.left() + across * plot.width(), y(curve.points[index]))
            })
            .collect();
        painter.add(egui::Shape::line(points, stroke));
    };
    // Behind, and thin enough to read as the question rather than the answer.
    if let Some(ghost) = ghost {
        trace(ghost, Stroke::new(1.4, theme::MUTED.gamma_multiply(0.55)));
    }
    if let Some(curve) = curve {
        trace(curve, Stroke::new(1.5, theme::ACCENT));
        // Only the drawn profile gets its peak marked: that dot is the number
        // the renderer is asked to confirm, and the ghost is not running.
        painter.circle_filled(
            egui::pos2(x(f64::from(curve.peak_hz)), y(curve.max_response_db)),
            2.5,
            theme::ACCENT,
        );
    }
}

/// Where our reading of a profile and the renderer's disagree.
///
/// Two independent parses of one documented format: when they differ, the
/// running cascade is the truth and the drawing is the thing to distrust, so say
/// so rather than let a wrong picture stand.
fn hptf_disagreement(
    curve: &crate::hptf_profile::Curve,
    readout: &crate::backend::HptfReadout,
) -> Option<String> {
    if readout.bands != curve.bands {
        return Some(format!(
            "Showing {} bands, but the renderer is running {}.",
            curve.bands, readout.bands
        ));
    }
    if (readout.preamp_db - curve.preamp_db).abs() > 0.01 {
        return Some(format!(
            "Showing a preamp of {:+.2} dB, but the renderer read {:+.2} dB.",
            curve.preamp_db, readout.preamp_db
        ));
    }
    // Both peaks include the file preamp and any additional output trim.
    if (readout.max_response_db - curve.max_response_db).abs() > 0.1 {
        return Some(format!(
            "Showing a peak of {:+.2} dB, but the renderer measured {:+.2} dB.",
            curve.max_response_db, readout.max_response_db
        ));
    }
    None
}

fn is_reconfigurable_scene_error(error: &str) -> bool {
    [
        "Scene dynamic-object count changed",
        "Scene LFE layout changed",
        "Scene configuration generation changed",
        "Selected Scene presentation changed",
        "Scene dynamic-object element IDs changed",
        "Scene LFE element ID changed",
    ]
    .iter()
    .any(|prefix| error.starts_with(prefix))
}

fn decoder_status_line(decoder: &DecoderSnapshot) -> StatusLine {
    let source = decoder
        .path()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("AC-4 source");
    match decoder.phase() {
        DecodePhase::Unavailable => StatusLine::idle(
            decoder
                .detail()
                .unwrap_or("The Windows decode worker is unavailable"),
        ),
        DecodePhase::Idle => StatusLine::idle("Add or select an AC-4 media file"),
        DecodePhase::Opening => StatusLine::idle(format!("Opening {source} with MacinDecode Core")),
        DecodePhase::Seeking => {
            let target = decoder.metrics().map_or(0, DecodeMetrics::target_frame);
            StatusLine::idle(format!("Seeking {source} to frame {target}"))
        }
        DecodePhase::Buffering => {
            let buffered = decoder
                .metrics()
                .map_or(0, DecodeMetrics::buffered_milliseconds);
            StatusLine::idle(format!(
                "MacinDecode Core buffered {buffered} / {PREBUFFER_MILLISECONDS} ms"
            ))
        }
        DecodePhase::Ready => {
            let metrics = decoder.metrics().expect("ready decode state has metrics");
            StatusLine::ready(format!(
                "MacinDecode Core ready: {} objects + {} LFE, {} ms buffered{}",
                metrics.object_count(),
                u8::from(metrics.has_lfe()),
                metrics.buffered_milliseconds(),
                seek_index_suffix(decoder)
            ))
        }
        DecodePhase::EndOfStream => {
            let metrics = decoder
                .metrics()
                .expect("end-of-stream decode state has metrics");
            StatusLine::ready(format!(
                "Decoded {source} to end: {} AUs, {} ms of scene PCM",
                metrics.decoded_access_units(),
                metrics.decoded_milliseconds()
            ))
        }
        DecodePhase::Failed => StatusLine::warning(format!(
            "MacinDecode Core failed for {source}: {}. Press Play to retry from the beginning.",
            decoder.detail().unwrap_or("unknown decode error")
        )),
    }
}

fn output_status_line(output: &OutputSnapshot, decoder: &DecoderSnapshot) -> StatusLine {
    if output.is_preview() {
        return preview_status_line(output, decoder);
    }
    match output.phase() {
        OutputPhase::Unavailable | OutputPhase::Idle => decoder_status_line(decoder),
        OutputPhase::Initializing => StatusLine::idle(format!("Opening {}", output.device_label())),
        OutputPhase::Ready => StatusLine::ready(format!(
            "Windows Spatial Audio ready: {} of {} dynamic slots reserved{}",
            output.reserved_dynamic_objects(),
            output.max_dynamic_objects(),
            seek_index_suffix(decoder)
        )),
        OutputPhase::Playing if output.is_buffering() => StatusLine::idle(format!(
            "Buffering audio output at frame {}{}",
            output.playhead_frames(),
            seek_index_suffix(decoder)
        )),
        OutputPhase::Playing if output.queued_output_frames().is_some() => {
            StatusLine::ready(format!(
                "Spatial playback at frame {}: {} queued output frames, {} underruns{}",
                output.playhead_frames(),
                output.queued_output_frames().unwrap_or(0),
                output.underruns(),
                seek_index_suffix(decoder)
            ))
        }
        OutputPhase::Playing => StatusLine::ready(format!(
            "Spatial playback at frame {}: {} object buffers, {} underruns{}",
            output.playhead_frames(),
            output.object_buffer_submissions(),
            output.underruns(),
            seek_index_suffix(decoder)
        )),
        OutputPhase::Paused => StatusLine::idle(format!(
            "Spatial playback paused at {} frames",
            output.playhead_frames()
        )),
        OutputPhase::Ended => StatusLine::ready(format!(
            "Spatial playback ended at frame {}",
            output.playhead_frames()
        )),
        OutputPhase::Failed => StatusLine::warning(format!(
            "Spatial audio output failed: {}",
            output.error().unwrap_or("unknown native output error")
        )),
    }
}

/// The same states, worded so nobody reads the stage as audible.
///
/// The preview walks the decoded scene without an audio device behind it, so
/// every line here says so rather than borrowing playback's vocabulary.
fn preview_status_line(output: &OutputSnapshot, decoder: &DecoderSnapshot) -> StatusLine {
    match output.phase() {
        OutputPhase::Playing => StatusLine::ready(format!(
            "Scene preview at frame {} · no audio output{}",
            output.playhead_frames(),
            seek_index_suffix(decoder)
        )),
        OutputPhase::Paused => StatusLine::idle(format!(
            "Scene preview paused at {} frames · no audio output",
            output.playhead_frames()
        )),
        OutputPhase::Ended => StatusLine::ready(format!(
            "Scene preview ended at frame {}",
            output.playhead_frames()
        )),
        OutputPhase::Failed => StatusLine::warning(format!(
            "Scene preview failed: {}",
            output.error().unwrap_or("unknown scene error")
        )),
        _ => StatusLine::ready(format!(
            "Scene preview ready: {} objects, no audio output on this build{}",
            output.reserved_dynamic_objects(),
            seek_index_suffix(decoder)
        )),
    }
}

fn seek_index_suffix(decoder: &DecoderSnapshot) -> String {
    let Some(metrics) = decoder.metrics() else {
        return String::new();
    };
    if metrics.is_indexing() {
        " · indexing seek map".to_owned()
    } else {
        metrics
            .index_error()
            .map_or_else(String::new, |error| format!(" · seek unavailable: {error}"))
    }
}

const fn decode_phase_label(phase: DecodePhase) -> &'static str {
    match phase {
        DecodePhase::Unavailable => "Unavailable",
        DecodePhase::Idle => "Idle",
        DecodePhase::Opening => "Opening",
        DecodePhase::Seeking => "Seeking",
        DecodePhase::Buffering => "Buffering",
        DecodePhase::Ready => "Ready",
        DecodePhase::EndOfStream => "End of stream",
        DecodePhase::Failed => "Failed",
    }
}

const fn output_phase_label(phase: OutputPhase) -> &'static str {
    match phase {
        OutputPhase::Unavailable => "Unavailable",
        OutputPhase::Idle => "Idle",
        OutputPhase::Initializing => "Initializing",
        OutputPhase::Ready => "Ready",
        OutputPhase::Playing => "Playing",
        OutputPhase::Paused => "Paused",
        OutputPhase::Ended => "End of stream",
        OutputPhase::Failed => "Failed",
    }
}

/// The count strip above the stage.
///
/// `silent` is `None` when there is nothing to report — the badge is then left
/// out entirely rather than drawn as a dash, because a dash already means "no
/// data" for the two reference-frame counts and the two senses must not blur.
fn draw_tracking_counts(
    ui: &mut egui::Ui,
    tracking: Option<crate::decoder::TrackingSummary>,
    silent: Option<usize>,
) {
    let scene_relative =
        tracking.map(|value| value.scene_relative + value.unspecified + value.unsupported);
    let head_locked = tracking.map(|value| value.head_relative);
    ui.horizontal_wrapped(|ui| {
        for (label, count, colour, hint) in [
            ("Scene relative", scene_relative, Color32::BLACK, "Objects fixed in the scene at the current playback position, including unspecified policies and scene-relative fallbacks. Includes objects beyond the view limit; excludes LFE."),
            ("Head locked", head_locked, Color32::WHITE, "Objects with a head-relative policy at the current playback position. Includes objects beyond the view limit; excludes LFE."),
        ] {
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(11.0).color(theme::MUTED));
                draw_tracking_count_badge(ui, count, theme::ACCENT, colour);
            }).response.on_hover_text(hint);
        }
        // Deliberately a different fill from the two above. Those split the
        // objects by reference frame and add up to the scene; this one reports
        // a level state that a scene-relative and a head-locked object can both
        // be in, so a reader who saw three accent badges in a row would be
        // right to try to add them up and wrong about what they got.
        if let Some(silent) = silent {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Silent").size(11.0).color(theme::MUTED));
                draw_tracking_count_badge(ui, Some(silent), theme::MUTED, theme::BACKGROUND);
            })
            .response
            .on_hover_text(
                "Objects whose measured level has stayed below the silence floor long enough to fade out of the scene. Counts only the objects the view can draw; excludes LFE. Their gain rings stay on the floor.",
            );
        }
    });
}

fn draw_tracking_count_badge(
    ui: &mut egui::Ui,
    count: Option<usize>,
    fill: Color32,
    colour: Color32,
) {
    let text = count.map_or_else(|| "—".to_owned(), |value| value.to_string());
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(28.0), egui::Sense::hover());
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), &text));
    ui.painter().rect_filled(rect, 0.0, fill);

    let mut galley =
        ui.painter()
            .layout_no_wrap(text.clone(), egui::FontId::monospace(12.0), colour);
    let available = rect.shrink(4.0).size();
    let size = galley.mesh_bounds.size();
    let scale = (available.x / size.x.max(1.0))
        .min(available.y / size.y.max(1.0))
        .min(1.0);
    if scale < 1.0 {
        galley = ui
            .painter()
            .layout_no_wrap(text, egui::FontId::monospace(12.0 * scale), colour);
    }
    // Centre the visible digits, including font bearings, rather than the line's leading.
    let origin = rect.center() - galley.mesh_bounds.center().to_vec2();
    ui.painter().galley(origin, galley, colour);
}

fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).size(11.0).strong().color(theme::MUTED));
    ui.add_space(4.0);
}

fn card<R>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::NONE
        .fill(theme::SURFACE)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .inner_margin(egui::Margin::same(14))
        .show(ui, contents)
        .inner
}

fn key_value(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).size(12.0).color(theme::MUTED));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).size(12.0).color(theme::TEXT));
        });
    });
}

fn decode_metric_values(decoder: &DecoderSnapshot) -> [(&'static str, String, String); 4] {
    let metrics = decoder.metrics();
    let phase = decode_phase_label(decoder.phase());
    [
        (
            "OBJECTS",
            metrics.map_or_else(|| "—".to_owned(), |value| value.object_count().to_string()),
            metrics.map_or_else(
                || phase.to_owned(),
                |value| format!("presentation {}", value.presentation_index()),
            ),
        ),
        (
            "LFE",
            metrics.map_or_else(
                || "—".to_owned(),
                |value| if value.has_lfe() { "1" } else { "0" }.to_owned(),
            ),
            "Native bed component".to_owned(),
        ),
        (
            "POSITION",
            metrics.map_or_else(
                || "—".to_owned(),
                |value| {
                    if value.state_complete() {
                        "READY"
                    } else {
                        "WAIT"
                    }
                    .to_owned()
                },
            ),
            metrics.map_or_else(
                || "OAMD pending".to_owned(),
                |value| format!("{} in-frame updates", value.metadata_updates()),
            ),
        ),
        (
            "BUFFER",
            metrics.map_or_else(
                || "—".to_owned(),
                |value| format!("{} ms", value.buffered_milliseconds()),
            ),
            metrics.map_or_else(
                || "Scene FIFO offline".to_owned(),
                |value| {
                    format!(
                        "{} / {} frames",
                        value.buffered_frames(),
                        value.buffer_capacity_frames()
                    )
                },
            ),
        ),
    ]
}

fn metric_strip(ui: &mut egui::Ui, decoder: &DecoderSnapshot) {
    let values = decode_metric_values(decoder);

    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 96.0), egui::Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, theme::SURFACE);
    let cell_width = rect.width() / 4.0;
    let first_cell = egui::Rect::from_min_size(rect.min, egui::vec2(cell_width, rect.height()));
    painter.rect_filled(first_cell, 0.0, theme::STAGE);
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, theme::BORDER),
        egui::StrokeKind::Inside,
    );

    let mut left = rect.left();
    let value_count = values.len();
    for (index, (title, value, detail)) in values.into_iter().enumerate() {
        let right = if index + 1 == value_count {
            rect.right()
        } else {
            left + cell_width
        };
        if index > 0 {
            painter.line_segment(
                [
                    egui::pos2(left, rect.top()),
                    egui::pos2(left, rect.bottom()),
                ],
                Stroke::new(1.0, theme::BORDER),
            );
        }
        let cell = egui::Rect::from_min_max(
            egui::pos2(left + 16.0, rect.top() + 12.0),
            egui::pos2(right - 12.0, rect.bottom() - 10.0),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(cell)
                .layout(Layout::top_down(Align::Min)),
            |ui| {
                ui.label(RichText::new(title).size(10.0).strong().color(theme::MUTED));
                ui.label(RichText::new(&value).size(21.0).strong().color(theme::TEXT));
                ui.label(RichText::new(&detail).size(10.0).color(theme::MUTED));
            },
        );
        left = right;
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the diagnostics viewport deliberately keeps its ordered key/value layout together"
)]
fn draw_diagnostics_content(
    root: &mut egui::Ui,
    backend: SpatialBackendKind,
    decoder: &DecoderSnapshot,
    output: &OutputSnapshot,
    tracking: Option<crate::decoder::TrackingSummary>,
) {
    egui::CentralPanel::default()
        .frame(
            egui::Frame::NONE
                .fill(theme::BACKGROUND)
                .inner_margin(egui::Margin::same(22)),
        )
        .show(root, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.heading(RichText::new("Playback diagnostics").color(theme::TEXT));
                    ui.label(
                        RichText::new(
                            "Live status for the decoder and native spatial output path.",
                        )
                        .size(11.0)
                        .color(theme::MUTED),
                    );
                    ui.add_space(16.0);
                    if let Some(tracking) = tracking {
                        section_title(ui, "CONTENT TRACKING");
                        card(ui, |ui| {
                            key_value(ui, "Scene / head relative", &format!("{} / {}", tracking.scene_relative, tracking.head_relative));
                            key_value(ui, "Unspecified · scene default", &tracking.unspecified.to_string());
                            key_value(ui, "Unsupported · scene fallback", &tracking.unsupported.to_string());
                            if let Some((element, issue)) = tracking.first_issue {
                                ui.colored_label(theme::WARNING, format!("Object {element}: {issue}"));
                            }
                        });
                        ui.add_space(16.0);
                    }
                    section_title(ui, "SESSION");
                    card(ui, |ui| {
                        let metrics = decoder.metrics();
                        key_value(
                            ui,
                            "Container",
                            metrics.map_or("Not connected", |value| value.container().label()),
                        );
                        ui.separator();
                        key_value(ui, "Decoder session", decode_phase_label(decoder.phase()));
                        ui.separator();
                        key_value(
                            ui,
                            "Scene elements",
                            &metrics.map_or_else(
                                || "—".to_owned(),
                                |value| {
                                    format!(
                                        "{} objects + {} LFE",
                                        value.object_count(),
                                        u8::from(value.has_lfe())
                                    )
                                },
                            ),
                        );
                        ui.separator();
                        key_value(
                            ui,
                            "Decoded AUs / frames",
                            &metrics.map_or_else(
                                || "0 / 0".to_owned(),
                                |value| {
                                    format!(
                                        "{} / {}",
                                        value.decoded_access_units(),
                                        value.decoded_scene_frames()
                                    )
                                },
                            ),
                        );
                        ui.separator();
                        key_value(
                            ui,
                            "Scene buffer",
                            &metrics.map_or_else(
                                || "0 ms".to_owned(),
                                |value| format!("{} ms", value.buffered_milliseconds()),
                            ),
                        );
                        ui.separator();
                        key_value(ui, "Backend policy", backend.label());
                        ui.separator();
                        key_value(
                            ui,
                            "Available playback modes",
                            &SpatialBackendKind::ALL.into_iter().filter(|mode| *mode != SpatialBackendKind::Automatic && mode.supported()).count().to_string(),
                        );
                        ui.separator();
                        key_value(ui, "Spatial stream", output_phase_label(output.phase()));
                        ui.separator();
                        key_value(ui, "Endpoint", output.device_label());
                        ui.separator();
                        key_value(
                            ui,
                            if backend.resolved() == SpatialBackendKind::WindowsSpatialAudio { "Dynamic objects" } else { "Scene objects" },
                            &if backend.resolved() == SpatialBackendKind::WindowsSpatialAudio { format!(
                                "{} active / {} reserved / {} max",
                                output.active_dynamic_objects(),
                                output.reserved_dynamic_objects(),
                                output.max_dynamic_objects()
                            ) } else { format!("{} active / {} total", output.active_dynamic_objects(), output.reserved_dynamic_objects()) },
                        );
                        ui.separator();
                        key_value(ui, "Playback clock", output.clock_label());
                        ui.separator();
                        key_value(ui, "Queued output frames", &output.queued_output_frames().map_or_else(|| "Not reported".into(), |n| n.to_string()));
                        ui.separator();
                        key_value(
                            ui,
                            "Render updates / frames",
                            &format!(
                                "{} / {}",
                                output.render_updates(),
                                output.submitted_frames()
                            ),
                        );
                        ui.separator();
                        key_value(
                            ui,
                            "Object buffers / positions",
                            &format!(
                                "{} / {}",
                                output.object_buffer_submissions(),
                                output.position_updates()
                            ),
                        );
                        ui.separator();
                        key_value(ui, "Underruns", &output.underruns().to_string());
                        #[cfg(all(target_os = "macos", macinrender_output))]
                        if let Some(status) = output.atmos_assist_status() {
                            ui.separator();
                            ui.label(RichText::new("Atmos label helper").size(11.0).color(theme::MUTED));
                            ui.add(egui::Label::new(RichText::new(status).size(11.0).color(theme::TEXT)).wrap());
                        }
                    });
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(format!(
                            "{} Decoder ownership remains independent of the native output stream.{}",
                            backend.availability(),
                            output
                                .error()
                                .map_or_else(String::new, |error| format!(" Error: {error}"))
                        ))
                        .size(10.0)
                        .color(theme::MUTED),
                    );
                });
        });
}

fn draw_drop_overlay(context: &egui::Context) {
    let painter = context.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("drop-overlay"),
    ));
    let rect = context.content_rect().shrink(24.0);
    painter.rect_filled(
        rect,
        0.0,
        Color32::from_rgba_unmultiplied(255, 253, 247, 244),
    );
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(2.0, theme::ACCENT),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "Drop AC-4 media files",
        egui::FontId::proportional(22.0),
        theme::TEXT,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The claim `hptf_profile` exists to make — that it reads the format the
    /// way the renderer does — checked against the renderer rather than against
    /// a hand-written expectation.
    #[cfg(macinrender_output)]
    #[test]
    fn both_readings_of_one_profile_agree_band_for_band() {
        // Every token the table maps, plus the shapes where a drift would hide:
        // a disabled band, a missing Gain and Q, a slot with no type at all, an
        // unknown type, a comment and a CRLF line.
        const PROFILE: &str = "\
# a profile written to exercise the token table\r
Preamp: -4.1 dB
Filter 1: ON LSC Fc 105 Hz Gain 9.7 dB Q 0.70
Filter 2: ON PK Fc 46 Hz Gain -9.2 dB Q 0.37
Filter 3: OFF PK Fc 900 Hz Gain 3.0 dB Q 2.00
Filter 4: ON HSC Fc 10000 Hz Gain -1.1 dB Q 0.70
Filter 5: ON LP Fc 19000 Hz
Filter 6: ON HP Fc 21 Hz Q 0.50
Filter 7: ON BP Fc 3000 Hz Gain 0 dB Q 1.41
Filter 8: ON NO Fc 8000 Hz Gain 0 dB Q 6.00
Filter 9: ON
Filter 10: ON WAT Fc 500 Hz Gain 1 dB Q 1
";
        let ours = crate::hptf_profile::Profile::parse(PROFILE).unwrap();
        let renderer = crate::backend::hptf_parse(PROFILE)
            .expect("a build with a renderer must have one to ask")
            .expect("the renderer must read this profile");
        assert_eq!(ours.disagreement(&renderer), None);
        // Seven enabled: the OFF band, the typeless slot and the unknown type
        // are dropped, and both sides have to drop the same three.
        assert_eq!(ours.bands(), 7);

        // Agreement means nothing unless the comparison can fail, so move one
        // digit of one Q and watch it.
        let drifted =
            crate::hptf_profile::Profile::parse(&PROFILE.replace("Q 0.37", "Q 0.38")).unwrap();
        assert!(drifted.disagreement(&renderer).is_some());
    }

    #[cfg(macinrender_output)]
    #[test]
    fn displayed_profile_response_matches_native_auto_trim() {
        use macindecode_macinrender as native;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("boost.txt");
        std::fs::write(
            &path,
            b"Preamp: 0 dB\nFilter 1: ON PK Fc 1000 Hz Gain 6 dB Q 1\n",
        )
        .unwrap();
        let mut session = native::Session::new(&native::Config {
            renderer: native::RendererSettings {
                binaural: true,
                layout: "4+7+0".into(),
                sofa: String::new(),
                split_lfe: true,
            },
            output: native::OutputKind::Null,
            device_id: String::new(),
            input_rate: 48_000,
        })
        .unwrap();
        let control = session.control();
        let profile = path.to_str().unwrap();
        for auto_trim in [false, true] {
            let revision = 1 + u64::from(auto_trim);
            assert_eq!(
                control.set_hptf(
                    &native::HptfSettings {
                        profile: profile.to_owned(),
                        auto_trim,
                    },
                    revision
                ),
                Ok(true)
            );
            session.reset(revision, 0).unwrap();
            let status = control.hptf_status().unwrap();
            assert_eq!(status.applied_revision, revision);
            let readout = crate::backend::HptfReadout {
                enabled: status.enabled,
                bands: status.bands,
                rate: status.rate,
                preamp_db: status.preamp_db,
                auto_trim_db: status.auto_trim_db,
                max_response_db: status.max_response_db,
            };
            let original = crate::hptf_profile::Curve::read(profile, status.rate).unwrap();
            let mut drawn = original.with_output_trim(status.auto_trim_db);
            assert!(original.max_response_db > 5.9);
            if auto_trim {
                assert!(drawn.max_response_db.abs() < 0.01);
            }
            for (before, after) in original.points.iter().zip(&drawn.points) {
                assert!((after - before - status.auto_trim_db).abs() < 0.0001);
            }
            assert_eq!(hptf_disagreement(&drawn, &readout), None);
            drawn.max_response_db += 1.0;
            assert!(
                hptf_disagreement(&drawn, &readout).is_some(),
                "auto trim must not suppress a real discrepancy"
            );
        }
    }

    #[test]
    fn decode_failure_unlocks_output_settings_and_restores_the_previous_choice() {
        let mut output = SpatialOutputController::new();
        let previous = output.settings().clone();
        let mut replacement = previous.clone();
        replacement.native_device = OutputDeviceSelection::EndpointId("replacement".into());
        output.install_settings(replacement.clone());
        let mut pending = Some(previous.clone());
        assert!(cancel_output_change_after_decode_failure(
            &mut output,
            &mut pending
        ));
        assert!(pending.is_none());
        assert!(!output.settings_pending());
        assert_eq!(output.settings(), &previous);
        output.hot_settings(replacement.clone());
        assert_eq!(output.take_settings_result(), Some(Ok(())));
        assert_eq!(output.settings(), &replacement);
        assert!(!cancel_output_change_after_decode_failure(
            &mut output,
            &mut pending
        ));
        assert_eq!(
            output.settings(),
            &replacement,
            "later failed-state polls must preserve a new user choice"
        );
    }

    #[cfg(macinrender_output)]
    #[test]
    fn decode_failure_discards_a_preparation_that_has_not_handed_off() {
        let mut output = SpatialOutputController::new();
        let previous = output.settings().clone();
        output
            .prepare_settings(
                OutputSettings {
                    mode: SpatialBackendKind::SafBinaural,
                    sofa: "missing-decode-recovery-test.sofa".into(),
                    null_output: true,
                    ..previous.clone()
                },
                48_000,
                1,
                0,
            )
            .unwrap();
        let mut pending = None;
        assert!(output.settings_pending());
        assert!(cancel_output_change_after_decode_failure(
            &mut output,
            &mut pending
        ));
        assert!(!output.settings_pending());
        assert!(output.take_prepared_settings(1, 0).is_none());
        output.poll();
        assert_eq!(output.settings(), &previous);
    }

    #[test]
    #[cfg(all(feature = "decode", not(spatial_output)))]
    fn a_preview_failure_after_decoded_eos_schedules_recovery_without_spinning() {
        use crate::decoder::{
            DecodedSceneBlock, PlaybackKey, SceneObjectPcm, SceneSignature, scene_queue_pair,
        };

        let key = PlaybackKey::new(1, 0);
        let (queue, reader) = scene_queue_pair(key);
        for (start, id) in [(0, 7), (2048, 8)] {
            queue
                .try_push(
                    key,
                    DecodedSceneBlock::new(
                        48_000,
                        start,
                        2048,
                        1,
                        0,
                        None,
                        true,
                        vec![SceneObjectPcm::new(id, None, vec![0.0; 2048])],
                        None,
                        Vec::new(),
                    ),
                )
                .expect("queue scene");
        }
        queue.mark_end_of_stream(key);
        let config = OutputStreamConfig::new(
            1,
            0,
            0,
            48_000,
            SceneSignature::new(1, 0, None, vec![7], None),
            OutputDeviceSelection::SystemDefault,
        )
        .expect("preview config");
        let mut output = SpatialOutputController::new();
        output.ensure_configured(&config, reader);
        let context = egui::Context::default();
        let raw = egui::RawInput::default();
        for _ in 0..3 {
            let _ = context.run_logic(&raw, |_| {});
        }
        let now = Instant::now();
        let _ = context.run_logic(&raw, |ctx| {
            advance_scene_preview(&mut output, true, ctx, now);
        });
        assert!(!context.has_requested_repaint());
        let _ = context.run_logic(&raw, |ctx| {
            advance_scene_preview(&mut output, true, ctx, now + Duration::from_millis(100));
        });
        assert_eq!(output.snapshot().phase(), OutputPhase::Failed);
        assert!(
            context.has_requested_repaint(),
            "recovery needs another logic pass at decoded EOS"
        );
        for tick in 2..=4 {
            let _ = context.run_logic(&raw, |ctx| {
                advance_scene_preview(
                    &mut output,
                    true,
                    ctx,
                    now + Duration::from_millis(tick * 100),
                );
            });
        }
        assert!(
            !context.has_requested_repaint(),
            "a latched failure must not keep waking the UI"
        );
    }

    #[test]
    fn presence_holds_then_fades_and_a_returning_level_restores_it_at_once() {
        let mut meters = ObjectMeters::default();
        assert!((meters.presence(0) - 1.0).abs() < f32::EPSILON);

        // Inside the hold, a silent object is still fully present: the hold is
        // there to cross phrase gaps and decay tails without flicker.
        meters.silent_for[0] = scene3d::params::SILENCE_HOLD_SECONDS - 0.01;
        assert!((meters.presence(0) - 1.0).abs() < f32::EPSILON);

        // Halfway through the fade, halfway out.
        meters.silent_for[0] =
            scene3d::params::SILENCE_HOLD_SECONDS + scene3d::params::SILENCE_FADE_SECONDS / 2.0;
        let half = meters.presence(0);
        assert!(
            (half - 0.5).abs() < 0.01,
            "half a fade in, presence was {half}"
        );

        // Past the fade it stays at zero rather than going negative.
        meters.silent_for[0] =
            scene3d::params::SILENCE_HOLD_SECONDS + scene3d::params::SILENCE_FADE_SECONDS * 4.0;
        assert!(meters.presence(0).abs() < f32::EPSILON);

        // Coming back is immediate. Disappearing may take time; reappearing may
        // not, because reappearing is the event the view exists to show.
        meters.silent_for[0] = 0.0;
        assert!((meters.presence(0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_silent_count_reports_only_the_slots_that_faded_all_the_way_out() {
        let mut meters = ObjectMeters::default();
        let gone = scene3d::params::SILENCE_HOLD_SECONDS + scene3d::params::SILENCE_FADE_SECONDS;
        meters.silent_for[0] = gone;
        meters.silent_for[1] = gone;
        // Still inside the hold, so not yet counted — the badge reports what has
        // actually left the picture, not what is merely quiet this instant.
        meters.silent_for[2] = scene3d::params::SILENCE_HOLD_SECONDS / 2.0;
        assert_eq!(meters.faded_out(4), 2);
        // A slot beyond the reported object count is not counted at all.
        meters.silent_for[5] = gone;
        assert_eq!(meters.faded_out(4), 2);
        assert_eq!(meters.faded_out(6), 3);
    }

    #[test]
    fn turning_the_fade_off_restores_presence_without_stopping_the_silence_clock() {
        let mut meters = ObjectMeters::default();
        meters.silent_for[0] =
            scene3d::params::SILENCE_HOLD_SECONDS + scene3d::params::SILENCE_FADE_SECONDS;
        assert!(meters.drawn_presence(0, true).abs() < f32::EPSILON);

        // Switching the fade off is a drawing decision, so the object is back
        // at once and at full strength.
        assert!((meters.drawn_presence(0, false) - 1.0).abs() < f32::EPSILON);

        // The clock it came back from is still running underneath, so switching
        // the fade on again finds the object where it actually is instead of
        // handing it a fresh hold. Nothing here reads the loudness switch: the
        // readout and the fade are separate controls.
        assert!(meters.presence(0).abs() < f32::EPSILON);
        assert!(meters.drawn_presence(0, true).abs() < f32::EPSILON);
    }

    #[test]
    fn a_silent_nameplate_rests_at_a_dim_of_its_own_and_only_the_fade_takes_it_away() {
        // A plate with something to report is at full strength, however long
        // its object has been playing.
        assert!((plate_alpha(false, 1.0) - 1.0).abs() < f32::EPSILON);

        // With the fade off, presence is pinned at 1.0, so this is where a plate
        // reading -∞ spends the entire session. It has to be a step back rather
        // than a disappearance: the readout is still correct, it just has
        // nothing left to say.
        let resting = plate_alpha(true, 1.0);
        assert!(
            (resting - scene3d::params::NAMEPLATE_SILENT_ALPHA).abs() < f32::EPSILON,
            "a silent plate rested at {resting}"
        );
        assert!(resting > 0.0, "resting is a dim, not a disappearance");

        // With the fade on, the same dim is only where the plate starts. Only
        // presence takes it to nothing, which is where the draw loop stops
        // laying it out at all.
        assert!(plate_alpha(true, 0.5) < resting);
        assert!(plate_alpha(true, 0.0).abs() < f32::EPSILON);
    }

    /// A published object whose window measured `mean_square`, peaking at `peak`.
    fn measured(mean_square: f64, frames: u32, peak: f32) -> crate::scene_view::ObjectView {
        crate::scene_view::ObjectView {
            active: true,
            gain: 1.0,
            energy: crate::scene_view::ObjectEnergy {
                weighted_sum_squares: mean_square * f64::from(frames),
                frames,
                peak,
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_peak_marker_rises_at_once_holds_still_and_then_falls_at_a_fixed_rate() {
        let mut peak = PeakHold::default();
        peak.advance(0.5, 0.1);
        assert!(
            (peak.level - 0.5).abs() < f32::EPSILON,
            "a marker that lags the level it marks is not a peak marker"
        );

        // Through the hold it does not move, however far the level has fallen.
        peak.advance(0.001, scene3d::params::PEAK_HOLD_SECONDS - 0.05);
        assert!((peak.level - 0.5).abs() < f32::EPSILON);

        // Crossing the end of the hold charges only the part of the step that
        // is past it, so a long hold does not buy a long fall in one frame.
        peak.advance(0.001, 0.1);
        let crossed = 20.0 * (0.5_f32 / peak.level).log10();
        assert!(
            (crossed - scene3d::params::PEAK_FALL_DECIBELS_PER_SECOND * 0.05).abs() < 0.01,
            "the step across the boundary fell {crossed} dB"
        );

        // Past it, a fixed number of decibels per second.
        let before = peak.level;
        peak.advance(0.001, 0.5);
        let fallen = 20.0 * (before / peak.level).log10();
        assert!(
            (fallen - scene3d::params::PEAK_FALL_DECIBELS_PER_SECOND * 0.5).abs() < 0.01,
            "the marker fell {fallen} dB in half a second"
        );

        // And never below the live level: a marker inside the bar it is meant
        // to lead would be reporting the present as the past.
        peak.advance(0.3, 10.0);
        assert!(peak.level >= 0.3);
    }

    #[test]
    fn a_clip_latches_long_enough_to_be_seen_and_then_expires_on_its_own() {
        let mirror = crate::scene_view::SceneViewMirror::new();
        let key = crate::decoder::PlaybackKey::new(1, 1);
        let bin = crate::scene_view::loudness_bin_frames(48_000);
        let mut meters = ObjectMeters::default();
        let start = Instant::now();

        mirror.write(key, [measured(0.25, bin, 1.0)], 0, 48_000);
        meters.advance(&mirror.read(key).unwrap(), key, 48_000, start);
        assert!(
            meters.bank_row(0, MeterReadout::Fast).2,
            "a sample at full scale has to light the indicator on the frame it lands"
        );

        // Push the clipping bin out of the ring with quiet audio. Until it is
        // gone the indication is simply still true.
        let bins = u32::try_from(crate::scene_view::LOUDNESS_BINS).unwrap();
        for step in 1..=bins {
            let at = i64::from(step) * i64::from(bin);
            mirror.write(key, [measured(0.0, bin, 0.0)], at, 48_000);
        }
        let quiet = mirror.read(key).unwrap();
        assert!(quiet.sample_peak(0).abs() < f32::EPSILON);

        // The hold outlives the ring, so the indicator is still lit ...
        meters.advance(&quiet, key, 48_000, start + Duration::from_secs_f32(0.5));
        assert!(meters.bank_row(0, MeterReadout::Fast).2);
        // ... and then goes out by itself, because twenty objects is too many
        // to ask anyone to reset by hand.
        meters.advance(
            &quiet,
            key,
            48_000,
            start + Duration::from_secs_f32(scene3d::params::CLIP_HOLD_SECONDS + 0.6),
        );
        assert!(!meters.bank_row(0, MeterReadout::Fast).2);
    }

    #[test]
    fn the_bank_reads_whichever_window_its_unit_names() {
        let mirror = crate::scene_view::SceneViewMirror::new();
        let key = crate::decoder::PlaybackKey::new(1, 1);
        let bin = crate::scene_view::loudness_bin_frames(48_000);
        let bins = u32::try_from(crate::scene_view::LOUDNESS_BINS).unwrap();
        // Fill the ring with full-scale audio, then let the newest few bins go
        // quiet: the fast window sees only the silence, the momentary window
        // still holds most of what came before.
        for step in 0..bins {
            let at = i64::from(step) * i64::from(bin);
            mirror.write(key, [measured(1.0, bin, 0.0)], at, 48_000);
        }
        for step in bins..bins + 4 {
            let at = i64::from(step) * i64::from(bin);
            mirror.write(key, [measured(0.0, bin, 0.0)], at, 48_000);
        }
        let frame = mirror.read(key).unwrap();

        let mut meters = ObjectMeters::default();
        let start = Instant::now();
        // Several long steps, so the ballistics have finished releasing and the
        // reading under test is the window rather than the decay.
        for step in 0..6 {
            meters.advance(&frame, key, 48_000, start + Duration::from_secs(step));
        }

        let (fast, ..) = meters.bank_row(0, MeterReadout::Fast);
        let (momentary, ..) = meters.bank_row(0, MeterReadout::Momentary);
        assert!(
            fast < scene3d::params::OBJECT_SILENT_GAIN,
            "the fast meter should have released onto the silence, read {fast}"
        );
        let decibels = readout_decibels(momentary, MeterReadout::Momentary);
        assert!(
            (decibels + 1.15).abs() < 0.1,
            "thirty-six bins of full scale in a forty-bin window read {decibels} LUFS-M"
        );
    }

    #[test]
    fn the_two_units_differ_by_the_offset_the_standard_defines() {
        // Same energy, same window; only the unit changes. If these ever drift
        // apart by anything else, one of the two columns is lying about what it
        // measured rather than about how it labels it.
        let fast = readout_decibels(0.25, MeterReadout::Fast);
        let momentary = readout_decibels(0.25, MeterReadout::Momentary);
        assert!((fast - momentary - 0.691).abs() < 1e-4);
        assert_eq!(MeterReadout::Fast.other(), MeterReadout::Momentary);
        assert_eq!(MeterReadout::Momentary.other(), MeterReadout::Fast);
        assert_ne!(MeterReadout::Fast.lane(), MeterReadout::Momentary.lane());
    }

    #[test]
    fn a_readout_never_outgrows_the_cells_reserved_for_it() {
        // The nameplate and the bank both lay out a sign cell and a magnitude
        // right-aligned against the last. That only aligns if no value on the
        // scale, in either unit, needs more room than was reserved.
        for readout in [MeterReadout::Fast, MeterReadout::Momentary] {
            for step in 0..=40_u8 {
                let level = scene3d::params::OBJECT_SILENT_GAIN.powf(f32::from(step) / 40.0);
                let (sign, magnitude) = decibel_cells(Some(readout_decibels(level, readout)));
                assert!(
                    sign == "−" || sign == " ",
                    "{readout:?} signed with {sign:?}"
                );
                // One cell goes to the sign, so the digits get the rest.
                assert!(
                    magnitude.chars().count() < scene3d::params::NAMEPLATE_CELLS,
                    "{readout:?} rendered {sign}{magnitude} at level {level}"
                );
            }
        }
        // Silence is the one reading with no digits, and it still owns a sign.
        let (sign, magnitude) = decibel_cells(None);
        assert_eq!(sign, "−");
        assert_eq!(magnitude, "∞");
    }

    #[test]
    fn the_momentary_window_is_the_whole_ring_and_nothing_else() {
        // Stated once, in the mirror. A second definition in milliseconds here
        // would be a second thing to keep in step with the ring's size.
        let expected = crate::scene_view::loudness_bin_frames(48_000)
            * u32::try_from(crate::scene_view::LOUDNESS_BINS).unwrap();
        assert_eq!(momentary_window_frames(48_000), expected);
        assert_eq!(momentary_window_frames(48_000), 19_200);
    }

    #[test]
    fn the_nameplate_strip_and_the_footprint_share_one_decibel_floor() {
        // Two readings of the same object in two places; if their floors ever
        // drift, a strip can sit empty under a footprint core that is not, and
        // the picture contradicts itself.
        assert!(loudness_fraction(0.0).abs() < f32::EPSILON);
        assert!(loudness_fraction(scene3d::params::OBJECT_SILENT_GAIN).abs() < f32::EPSILON);
        assert!((loudness_fraction(1.0) - 1.0).abs() < f32::EPSILON);
        assert!((loudness_fraction(4.0) - 1.0).abs() < f32::EPSILON);
        // Monotonic in between, so a louder object never reads shorter.
        let mut previous = 0.0;
        for step in 0..=20_u8 {
            let gain = scene3d::params::OBJECT_SILENT_GAIN.powf(1.0 - f32::from(step) / 20.0);
            let fraction = loudness_fraction(gain);
            assert!(
                fraction >= previous,
                "gain {gain} gave {fraction} after {previous}"
            );
            previous = fraction;
        }
    }

    #[test]
    fn dynamic_object_numbers_are_one_based_and_do_not_reserve_lfe_zero() {
        assert_eq!(object_display_number(0), 1);
        assert_eq!(
            object_display_number(crate::scene_view::MAX_VIEW_OBJECTS - 1),
            20
        );
    }

    #[test]
    fn output_sync_preserves_same_request_during_buffering() {
        assert_eq!(
            output_sync_action(DecodePhase::Buffering, true),
            OutputSyncAction::Preserve
        );
    }

    #[test]
    fn output_sync_preserves_renderer_while_a_new_epoch_buffers() {
        assert_eq!(
            output_sync_action(DecodePhase::Buffering, false),
            OutputSyncAction::Preserve
        );
    }

    #[test]
    fn output_sync_preserves_renderer_while_seeking() {
        assert_eq!(
            output_sync_action(DecodePhase::Seeking, false),
            OutputSyncAction::Preserve
        );
    }

    #[test]
    fn output_sync_configures_an_unconfigured_ready_decoder() {
        assert_eq!(
            output_sync_action(DecodePhase::Ready, false),
            OutputSyncAction::Configure
        );
    }

    #[test]
    fn output_sync_resets_during_a_new_open_request() {
        assert_eq!(
            output_sync_action(DecodePhase::Opening, true),
            OutputSyncAction::Reset
        );
    }

    #[test]
    fn playback_restore_uses_the_latest_transport_intent() {
        let mut pending = true;
        assert_eq!(take_playback_restore(&mut pending, true), Some(true));
        assert!(!pending);

        let mut pending = true;
        assert_eq!(take_playback_restore(&mut pending, false), Some(false));
    }

    #[test]
    fn shuffle_never_immediately_repeats_the_current_item() {
        let mut state = 7;
        for current in 0..4 {
            for _ in 0..32 {
                let next = shuffled_source_index(Some(current), 4, &mut state)
                    .expect("a four-item playlist has another shuffle target");
                assert!(next < 4);
                assert_ne!(next, current);
            }
        }
        assert_eq!(shuffled_source_index(Some(0), 1, &mut state), None);
    }

    #[test]
    fn sequential_playback_is_the_default_mode() {
        assert_eq!(PlaybackMode::default(), PlaybackMode::Sequential);
    }

    #[test]
    fn repeat_modes_replay_a_single_item_playlist() {
        assert!(should_replay_current_on_completion(
            PlaybackMode::RepeatOne,
            3
        ));
        assert!(should_replay_current_on_completion(
            PlaybackMode::RepeatAll,
            1
        ));
        assert!(!should_replay_current_on_completion(
            PlaybackMode::RepeatAll,
            3
        ));
        assert!(!should_replay_current_on_completion(
            PlaybackMode::Sequential,
            1
        ));
    }

    #[test]
    fn a_backgrounded_player_keeps_asking_for_the_passes_it_needs() {
        // While the window is hidden these delays are the only thing that runs
        // the player at all: eframe skips the egui pass entirely and drives
        // `App::logic`, which is reached only because a previous pass asked for
        // it. Returning None in a state that still has work to do is what left a
        // minimized player stopped at the end of a track.
        assert_eq!(
            output_repaint_delay(OutputPhase::Playing, true, true),
            Some(Duration::from_millis(16))
        );
        assert_eq!(
            output_repaint_delay(OutputPhase::Initializing, true, true),
            Some(Duration::from_millis(50))
        );
        assert!(
            output_repaint_delay(OutputPhase::Ended, true, true).is_some(),
            "an item ended with more to play and nothing would drive the hand-off"
        );
        assert_eq!(
            output_repaint_delay(OutputPhase::Ended, false, true),
            None,
            "a finished playlist must settle instead of polling forever"
        );
        assert_eq!(output_repaint_delay(OutputPhase::Paused, true, true), None);
        assert_eq!(output_repaint_delay(OutputPhase::Ready, true, true), None);
    }

    #[test]
    fn a_hidden_player_slows_down_without_dropping_the_hand_off() {
        // Motion only needs a frame rate when there is a frame. What must not
        // slow down is the one thing a listener notices with the window away:
        // the gap between one item and the next.
        let showing = output_repaint_delay(OutputPhase::Playing, true, true);
        let hidden = output_repaint_delay(OutputPhase::Playing, true, false);
        assert!(
            hidden > showing,
            "hidden playback still asks for a frame rate: {hidden:?}"
        );

        assert_eq!(
            output_repaint_delay(OutputPhase::Ended, true, false),
            output_repaint_delay(OutputPhase::Ended, true, true),
            "the playlist hand-off must not slow down just because nobody looks"
        );
        assert_eq!(
            output_repaint_delay(OutputPhase::Ended, false, false),
            None,
            "a finished playlist must settle whether or not it is on screen"
        );
    }

    #[test]
    fn completion_is_handled_only_for_the_current_playback_epoch() {
        assert!(should_handle_completed_item(OutputPhase::Ended, true, true));
        assert!(!should_handle_completed_item(
            OutputPhase::Ended,
            true,
            false
        ));
        assert!(!should_handle_completed_item(
            OutputPhase::Playing,
            true,
            true
        ));
        assert!(!should_handle_completed_item(
            OutputPhase::Ended,
            false,
            true
        ));
    }

    #[test]
    fn device_wait_is_scoped_to_the_playback_epoch() {
        let waiting = DeviceWait {
            request_id: 7,
            playback_epoch: 3,
            frame: 96_000,
        };
        assert!(waiting.belongs_to(7, 3));
        assert!(!waiting.belongs_to(7, 4));
        assert!(!waiting.belongs_to(8, 3));
    }

    #[test]
    fn normalized_timeline_maps_to_absolute_frames() {
        assert_eq!(normalized_frame(0.0, 480_000), 0);
        assert_eq!(normalized_frame(0.25, 480_000), 120_000);
        assert_eq!(normalized_frame(1.0, 480_000), 480_000);
    }

    #[test]
    fn timestamp_uses_hours_only_when_needed() {
        assert_eq!(format_timestamp(48_000 * 65, 48_000), "01:05");
        assert_eq!(format_timestamp(48_000 * 3_661, 48_000), "01:01:01");
        assert_eq!(format_timestamp(0, 0), "--:--");
    }

    #[test]
    fn only_scene_signature_failures_trigger_automatic_reconfiguration() {
        assert!(is_reconfigurable_scene_error(
            "Scene configuration generation changed from 1 to 2"
        ));
        assert!(!is_reconfigurable_scene_error(
            "Windows returned an invalid Spatial Audio object buffer"
        ));
    }

    #[test]
    fn a_profile_is_read_again_only_when_the_file_or_the_design_rate_changes() {
        let band = "Filter 1: ON PK Fc 1000 Hz Gain 1.0 dB Q 1.00\n";
        let folder = tempfile::tempdir().unwrap();
        let first = folder.path().join("first.txt");
        let second = folder.path().join("second.txt");
        std::fs::write(&first, band).unwrap();
        std::fs::write(&second, band.repeat(2)).unwrap();
        let first = first.to_str().unwrap();
        let second = second.to_str().unwrap();

        // The band count says whether the file was read again, and says it in an
        // integer.
        let mut slot = None;
        let bands =
            |slot: &Option<HptfDrawing>| slot.as_ref().unwrap().curve.as_ref().unwrap().bands;

        assert!(hptf_curve(&mut slot, first, 48_000).is_some());
        assert_eq!(bands(&slot), 1);

        // Rewriting the file behind the cache is how the next call proves it did
        // not go back to disk.
        std::fs::write(first, band.repeat(3)).unwrap();
        assert!(hptf_curve(&mut slot, first, 48_000).is_some());
        assert_eq!(bands(&slot), 1);

        // A rate change is a different curve from the same file, so it re-reads.
        assert!(hptf_curve(&mut slot, first, 44_100).is_some());
        assert_eq!(bands(&slot), 3);
        assert!(hptf_curve(&mut slot, second, 44_100).is_some());
        assert_eq!(bands(&slot), 2);

        // Turning the profile off has to take its picture with it.
        assert!(hptf_curve(&mut slot, "", 44_100).is_none());
        assert!(slot.is_none());
    }

    #[test]
    fn the_plot_frames_the_ghost_too_but_only_when_a_curve_leaves_the_fixed_axis() {
        let curve = |max: f32, min: f32| crate::hptf_profile::Curve {
            bands: 1,
            preamp_db: 0.0,
            max_response_db: max,
            peak_hz: 1_000.0,
            points: vec![min, max],
        };
        let axis = |top: f32, bottom: f32, range: (f32, f32)| {
            (range.0 - top).abs() < f32::EPSILON && (range.1 - bottom).abs() < f32::EPSILON
        };

        // Inside the fixed frame nothing rescales, which is the whole point of
        // fixing it.
        let quiet = curve(1.5, -4.0);
        assert!(axis(3.0, -12.0, hptf_axis([None, None])));
        assert!(axis(3.0, -12.0, hptf_axis([None, Some(&quiet)])));
        assert!(axis(3.0, -12.0, hptf_axis([Some(&quiet), Some(&quiet)])));

        // Either curve running past either end extends the frame, so a ghost is
        // never cropped to keep the drawn one's scale.
        let loud = curve(7.2, -20.4);
        assert!(axis(8.0, -21.0, hptf_axis([Some(&loud), Some(&quiet)])));
        assert!(axis(8.0, -21.0, hptf_axis([Some(&quiet), Some(&loud)])));
    }

    #[test]
    fn a_profile_is_never_ghosted_behind_itself() {
        assert_eq!(
            hptf_ghost(Some("/hptf/other.txt"), "/hptf/drawn.txt"),
            Some("/hptf/other.txt")
        );
        // The frame that selects a row is also a frame that hovers it.
        assert_eq!(hptf_ghost(Some("/hptf/drawn.txt"), "/hptf/drawn.txt"), None);
        assert_eq!(hptf_ghost(None, "/hptf/drawn.txt"), None);
        // With nothing selected the ghost is the whole drawing.
        assert_eq!(
            hptf_ghost(Some("/hptf/other.txt"), ""),
            Some("/hptf/other.txt")
        );
    }
}
