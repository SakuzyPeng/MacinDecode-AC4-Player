use super::screen::ScreenSettings;
use super::{OutputDeviceSelection, SpatialBackendKind};
use crate::head_tracking::HeadSource;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SpeakerRenderer {
    #[default]
    #[serde(rename = "saf-vbap")]
    SafVbap,
    #[serde(rename = "triple-balance")]
    TripleBalance,
}
impl SpeakerRenderer {
    pub const fn label(self) -> &'static str {
        match self {
            Self::SafVbap => "SAF VBAP",
            Self::TripleBalance => "Triple Balance",
        }
    }
}

/// Which renderer forms the headphone feed under software binaural output.
///
/// The binaural counterpart of [`SpeakerRenderer`]: both renderers write the
/// same two channels to the same stereo device, so the choice lives inside the
/// mode rather than beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BinauralRenderer {
    #[default]
    #[serde(rename = "saf-hrtf")]
    SafHrtf,
    /// `MacinRender`'s native room renderer: a modelled room around a fixed
    /// listening position, with personal HRIRs and 3DoF head rotation.
    #[serde(rename = "room")]
    Room,
}
impl BinauralRenderer {
    pub const ALL: [Self; 2] = [Self::SafHrtf, Self::Room];
    pub const fn label(self) -> &'static str {
        match self {
            Self::SafHrtf => "SAF HRTF",
            Self::Room => "Room",
        }
    }
}

/// How many metres one unit of Scene distance spans in the modelled room.
///
/// The range and the default are the renderer's own (`adm_room_config_t`):
/// outside it the renderer refuses the configuration rather than clamping.
pub const ROOM_METRES_PER_UNIT_DEFAULT: f64 = 1.5;
pub const ROOM_METRES_PER_UNIT_RANGE: std::ops::RangeInclusive<f64> = 0.01..=100.0;

/// Why room playback cannot start yet, before any output is asked for.
///
/// The interface ships ahead of the renderer: until the pinned `MacinRender`
/// carries room rendering (C ABI 1.46) and `backend/macinrender.rs` drives it,
/// choosing the room renderer is stored but refuses to play, rather than
/// quietly playing SAF HRTF under a room label.
pub const ROOM_PLAYBACK_UNAVAILABLE: &str = "Room binaural playback is not available in this build yet: it needs MacinRender's room renderer (C ABI 1.46)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SpeakerLayout {
    #[default]
    #[serde(rename = "7.1.4")]
    SevenOneFour,
    #[serde(rename = "9.1.6")]
    NineOneSix,
    #[serde(rename = "22.2")]
    TwentyTwoTwo,
}
impl SpeakerLayout {
    pub const ALL: [Self; 3] = [Self::SevenOneFour, Self::NineOneSix, Self::TwentyTwoTwo];
    pub const fn label(self) -> &'static str {
        match self {
            Self::SevenOneFour => "7.1.4",
            Self::NineOneSix => "9.1.6",
            Self::TwentyTwoTwo => "22.2",
        }
    }
    #[cfg(macinrender_output)]
    pub const fn core_id(self) -> &'static str {
        match self {
            Self::SevenOneFour => "4+7+0",
            Self::NineOneSix => "9.1.6",
            Self::TwentyTwoTwo => "9+10+3",
        }
    }
    pub const fn dynamic_budget(self) -> u32 {
        match self {
            Self::SevenOneFour => 0,
            Self::NineOneSix => 4,
            Self::TwentyTwoTwo => 11,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent output switches, not one state machine: LFE routing, the \
              Control Center label assist and the headphone preamp policy answer to \
              different parts of the chain"
)]
pub struct OutputSettings {
    #[cfg(test)]
    #[serde(skip)]
    pub null_output: bool,
    pub mode: SpatialBackendKind,
    pub speaker_renderer: SpeakerRenderer,
    pub layout: SpeakerLayout,
    pub split_lfe: bool,
    pub atmos_label_assist: bool,
    pub binaural_renderer: BinauralRenderer,
    pub sofa: String,
    /// The room package (`room.json`) the room renderer models; empty is none
    /// chosen, which the renderer cannot play.
    pub room: String,
    /// Personal HRIR package (`hrir.json`) for the room renderer; empty uses
    /// the HRIR the room package binds. A SOFA file is not one: it has to be
    /// prepared into this form first.
    pub room_hrir: String,
    pub room_metres_per_unit: f64,
    /// Screen mapping for screen-referenced objects; room renderer only.
    pub room_screen: ScreenSettings,
    /// `AutoEq` `ParametricEQ.txt` headphone compensation; empty is off.
    pub hptf: String,
    pub hptf_auto_trim: bool,
    /// The listener's own two knobs on top of that profile, in decibels and in
    /// decibels per octave. Both at rest means the profile reaches the renderer
    /// exactly as its file reads.
    pub hptf_bass_db: f32,
    pub hptf_tilt_db: f32,
    pub native_device: OutputDeviceSelection,
    pub stereo_device: OutputDeviceSelection,
    pub head_source: HeadSource,
    pub posebridge: crate::posebridge::Preferences,
}
impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            #[cfg(test)]
            null_output: false,
            mode: SpatialBackendKind::Automatic,
            speaker_renderer: SpeakerRenderer::default(),
            layout: SpeakerLayout::default(),
            split_lfe: true,
            atmos_label_assist: true,
            binaural_renderer: BinauralRenderer::default(),
            sofa: String::new(),
            room: String::new(),
            room_hrir: String::new(),
            room_metres_per_unit: ROOM_METRES_PER_UNIT_DEFAULT,
            room_screen: ScreenSettings::default(),
            hptf: String::new(),
            hptf_auto_trim: false,
            hptf_bass_db: 0.0,
            hptf_tilt_db: 0.0,
            native_device: OutputDeviceSelection::SystemDefault,
            stereo_device: OutputDeviceSelection::SystemDefault,
            head_source: HeadSource::Automatic,
            posebridge: crate::posebridge::Preferences::default(),
        }
    }
}
impl OutputSettings {
    #[cfg_attr(not(all(target_os = "macos", macinrender_output)), allow(dead_code))]
    pub fn atmos_label_applicable(&self) -> bool {
        #[cfg(test)]
        if self.null_output {
            return false;
        }
        self.mode.resolved() == SpatialBackendKind::SystemSpatial
            && self.layout == SpeakerLayout::SevenOneFour
    }
    /// Headphone compensation applies to the two-channel feed this player
    /// renders itself. The system-spatial and Windows passthrough paths hand a
    /// bed to the operating system, which forms the final stereo signal where
    /// we cannot filter it, so the control is unavailable rather than silent.
    #[cfg_attr(
        not(macinrender_output),
        allow(
            dead_code,
            reason = "only the renderer-backed build forms a headphone feed to compensate"
        )
    )]
    pub fn hptf_applicable(&self) -> bool {
        self.mode.resolved() == SpatialBackendKind::SafBinaural
    }
    #[cfg(macinrender_output)]
    pub fn hptf(&self) -> macindecode_macinrender::HptfSettings {
        macindecode_macinrender::HptfSettings {
            profile: if self.hptf_applicable() {
                self.hptf.clone()
            } else {
                String::new()
            },
            auto_trim: self.hptf_auto_trim,
        }
    }
    /// The adjustment as the profile code understands it, already clamped and
    /// already silent on an output with no headphone feed to adjust.
    #[cfg_attr(
        not(macinrender_output),
        allow(
            dead_code,
            reason = "only the renderer-backed build forms a headphone feed to adjust"
        )
    )]
    pub fn hptf_adjustment(&self) -> crate::hptf_profile::Adjustment {
        if !self.hptf_applicable() || self.hptf.is_empty() {
            return crate::hptf_profile::Adjustment::default();
        }
        crate::hptf_profile::Adjustment {
            bass_db: f64::from(self.hptf_bass_db),
            tilt_db_per_octave: f64::from(self.hptf_tilt_db),
        }
        .clamped()
    }
    /// Whether the room renderer is the one forming the headphone feed.
    pub fn room_selected(&self) -> bool {
        self.mode.resolved() == SpatialBackendKind::SafBinaural
            && self.binaural_renderer == BinauralRenderer::Room
    }
    /// Why these settings cannot start an output, when the reason is already
    /// known from the settings alone. The output paths ask before creating a
    /// session, so the refusal arrives as an ordinary output error.
    pub fn playback_blocker(&self) -> Option<&'static str> {
        if !self.room_selected() {
            None
        } else if self.room.is_empty() {
            Some("Choose a room package for room binaural playback")
        } else {
            Some(ROOM_PLAYBACK_UNAVAILABLE)
        }
    }
    pub fn validated(mut self) -> Self {
        self.posebridge.validate();
        if self.head_source == HeadSource::PoseBridge && !cfg!(posebridge_input) {
            self.head_source = HeadSource::Manual;
        }
        if !self.mode.supported() {
            self.mode = SpatialBackendKind::Automatic;
        }
        if self.sofa.contains('\0') {
            self.sofa.clear();
        }
        for path in [&mut self.hptf, &mut self.room, &mut self.room_hrir] {
            if path.contains('\0') {
                path.clear();
            }
        }
        self.room_metres_per_unit = if self.room_metres_per_unit.is_finite() {
            self.room_metres_per_unit.clamp(
                *ROOM_METRES_PER_UNIT_RANGE.start(),
                *ROOM_METRES_PER_UNIT_RANGE.end(),
            )
        } else {
            ROOM_METRES_PER_UNIT_DEFAULT
        };
        self.room_screen = self.room_screen.validated();
        // A stored file is not a trusted one: a knob that came back as NaN would
        // reach the biquad design as NaN, and `clamped` cannot fix that.
        for knob in [&mut self.hptf_bass_db, &mut self.hptf_tilt_db] {
            if !knob.is_finite() {
                *knob = 0.0;
            }
        }
        if self.head_source == HeadSource::AirPods && !cfg!(target_os = "macos") {
            self.head_source = HeadSource::Manual;
        }
        for device in [&mut self.native_device, &mut self.stereo_device] {
            if let OutputDeviceSelection::EndpointId(id) = device
                && (id.is_empty() || id.contains('\0'))
            {
                *device = OutputDeviceSelection::SystemDefault;
            }
        }
        self
    }
    pub fn needs_rebuild(&self, other: &Self) -> bool {
        self.mode.resolved() != other.mode.resolved()
            || (self.mode.resolved() == SpatialBackendKind::SystemSpatial
                && (self.layout != other.layout
                    || self.speaker_renderer != other.speaker_renderer
                    || (self.layout == SpeakerLayout::TwentyTwoTwo
                        && self.split_lfe != other.split_lfe)))
            // A room session fixes its HRIR, its scale and its screens when it
            // is created; only the room itself can change under a running one.
            || (self.mode.resolved() == SpatialBackendKind::SafBinaural
                && (self.binaural_renderer != other.binaural_renderer
                    || (self.binaural_renderer == BinauralRenderer::Room
                        && (self.room_hrir != other.room_hrir
                            || self.room_screen != other.room_screen
                            || self.room_metres_per_unit.to_bits()
                                != other.room_metres_per_unit.to_bits()))))
            || self.native_device != other.native_device
            || self.stereo_device != other.stereo_device
    }
    #[cfg(macinrender_output)]
    pub fn renderer(&self) -> macindecode_macinrender::RendererSettings {
        macindecode_macinrender::RendererSettings {
            binaural: self.mode.resolved() == SpatialBackendKind::SafBinaural,
            speaker_renderer: if self.mode.resolved() == SpatialBackendKind::SystemSpatial
                && self.speaker_renderer == SpeakerRenderer::TripleBalance
            {
                macindecode_macinrender::SpeakerRenderer::TripleBalance
            } else {
                macindecode_macinrender::SpeakerRenderer::SafVbap
            },
            layout: self.layout.core_id().into(),
            sofa: if self.mode.resolved() == SpatialBackendKind::SafBinaural {
                self.sofa.clone()
            } else {
                String::new()
            },
            split_lfe: self.split_lfe,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_renderer_preference_migrates_and_only_rebuilds_speaker_output() {
        let old: OutputSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(old.speaker_renderer, SpeakerRenderer::SafVbap);
        let mut before = OutputSettings {
            mode: SpatialBackendKind::SystemSpatial,
            ..old
        };
        let mut after = before.clone();
        after.speaker_renderer = SpeakerRenderer::TripleBalance;
        let reloaded: OutputSettings =
            serde_json::from_str(&serde_json::to_string(&after).unwrap()).unwrap();
        assert_eq!(reloaded.speaker_renderer, SpeakerRenderer::TripleBalance);
        assert!(before.needs_rebuild(&after) && after.needs_rebuild(&before));
        before.mode = SpatialBackendKind::SafBinaural;
        after.mode = SpatialBackendKind::SafBinaural;
        assert!(!before.needs_rebuild(&after));
        #[cfg(macinrender_output)]
        assert_eq!(
            before.renderer(),
            after.renderer(),
            "a saved speaker choice must not change headphone rendering"
        );
    }
    #[test]
    fn room_settings_migrate_and_rebuild_only_what_a_room_session_fixes() {
        // Settings written before the room renderer existed keep SAF HRTF.
        let old: OutputSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(old.binaural_renderer, BinauralRenderer::SafHrtf);
        assert!(old.room.is_empty() && old.room_hrir.is_empty());
        assert_eq!(
            old.room_metres_per_unit.to_bits(),
            ROOM_METRES_PER_UNIT_DEFAULT.to_bits()
        );

        let saf = OutputSettings {
            mode: SpatialBackendKind::SafBinaural,
            ..old
        };
        let room = OutputSettings {
            binaural_renderer: BinauralRenderer::Room,
            room: "rooms/studio-a/room.json".into(),
            ..saf.clone()
        };
        let reloaded: OutputSettings =
            serde_json::from_str(&serde_json::to_string(&room).unwrap()).unwrap();
        assert_eq!(reloaded, room);
        assert!(saf.needs_rebuild(&room) && room.needs_rebuild(&saf));

        // The room itself is switched under a running session.
        let mut other_room = room.clone();
        other_room.room = "rooms/studio-b/room.json".into();
        assert!(!room.needs_rebuild(&other_room));
        // The HRIR and the scale are fixed when the session is created.
        let mut hrir = room.clone();
        hrir.room_hrir = "hrir/personal/hrir.json".into();
        assert!(room.needs_rebuild(&hrir));
        let mut scale = room.clone();
        scale.room_metres_per_unit = 2.0;
        assert!(room.needs_rebuild(&scale));
        let mut screen = room.clone();
        screen.room_screen.reproduction_enabled = true;
        assert!(room.needs_rebuild(&screen));

        // A room choice saved while another mode plays changes nothing audible.
        let mut speakers = room.clone();
        speakers.mode = SpatialBackendKind::SystemSpatial;
        let mut speakers_saf = speakers.clone();
        speakers_saf.binaural_renderer = BinauralRenderer::SafHrtf;
        speakers_saf.room_metres_per_unit = 3.0;
        assert!(!speakers.needs_rebuild(&speakers_saf));
        assert!(!speakers.room_selected());
        assert_eq!(speakers.playback_blocker(), None);
    }

    #[test]
    fn room_playback_refuses_until_the_renderer_can_run_it() {
        let mut settings = OutputSettings {
            mode: SpatialBackendKind::SafBinaural,
            ..OutputSettings::default()
        };
        assert_eq!(settings.playback_blocker(), None);
        settings.binaural_renderer = BinauralRenderer::Room;
        assert!(
            settings
                .playback_blocker()
                .is_some_and(|reason| reason.contains("room package"))
        );
        settings.room = "rooms/studio-a/room.json".into();
        assert_eq!(settings.playback_blocker(), Some(ROOM_PLAYBACK_UNAVAILABLE));
    }

    #[test]
    fn stored_room_settings_are_not_trusted() {
        let mut settings = OutputSettings {
            room: "room\0.json".into(),
            room_hrir: "hrir\0.json".into(),
            room_metres_per_unit: f64::NAN,
            ..OutputSettings::default()
        };
        settings = settings.validated();
        assert!(settings.room.is_empty() && settings.room_hrir.is_empty());
        assert_eq!(
            settings.room_metres_per_unit.to_bits(),
            ROOM_METRES_PER_UNIT_DEFAULT.to_bits()
        );
        for (stored, kept) in [(0.0, 0.01_f64), (1_000.0, 100.0), (2.5, 2.5)] {
            settings.room_metres_per_unit = stored;
            assert_eq!(
                settings.clone().validated().room_metres_per_unit.to_bits(),
                kept.to_bits()
            );
        }
    }

    #[test]
    fn defaults_and_layout_catalog_are_fixed() {
        let settings = OutputSettings::default();
        assert_eq!(settings.layout.label(), "7.1.4");
        assert!(settings.split_lfe);
        assert_eq!(
            SpeakerLayout::ALL.map(SpeakerLayout::label),
            ["7.1.4", "9.1.6", "22.2"]
        );
        assert!(serde_json::from_str::<SpeakerLayout>("\"5.1\"").is_err());
    }

    #[test]
    fn headphone_compensation_is_off_by_default_and_hot_swaps_without_a_rebuild() {
        // Settings written before this feature existed carry neither field, and
        // must come back as no compensation using the profile's own preamp.
        let before: OutputSettings = serde_json::from_str("{}").unwrap();
        assert!(before.hptf.is_empty());
        assert!(!before.hptf_auto_trim);

        let mut after = before.clone();
        after.hptf = "Sony MDR-MV1 ParametricEQ.txt".into();
        after.hptf_auto_trim = true;
        // The renderer blends a new profile into the running feed, so neither
        // choosing one nor changing the preamp policy may rebuild the output.
        assert!(!before.needs_rebuild(&after));
        assert!(!after.needs_rebuild(&before));

        let saved = serde_json::to_string(&after).unwrap();
        let reloaded: OutputSettings = serde_json::from_str(&saved).unwrap();
        assert_eq!(reloaded.hptf, after.hptf);
        assert!(reloaded.hptf_auto_trim);

        let mut broken = after.clone();
        broken.hptf.push('\0');
        assert!(broken.validated().hptf.is_empty());
    }

    #[test]
    fn only_the_renderer_owned_headphone_feed_can_be_compensated() {
        let mut settings = OutputSettings::default();
        for (mode, applicable) in [
            (SpatialBackendKind::SafBinaural, true),
            // The bed goes to the operating system, which forms the final two
            // channels where this player cannot filter them.
            (SpatialBackendKind::SystemSpatial, false),
            (SpatialBackendKind::WindowsSpatialAudio, false),
        ] {
            settings.mode = mode;
            assert_eq!(
                settings.hptf_applicable(),
                applicable,
                "{} should{} accept headphone compensation",
                mode.label(),
                if applicable { "" } else { " not" }
            );
        }
    }

    #[test]
    fn old_settings_enable_assist_and_toggle_does_not_rebuild_audio() {
        let before: OutputSettings = serde_json::from_str("{}").unwrap();
        assert!(before.atmos_label_assist);
        let mut after = before.clone();
        after.atmos_label_assist = false;
        assert!(!before.needs_rebuild(&after));
        let saved = serde_json::to_string(&after).unwrap();
        assert!(
            !serde_json::from_str::<OutputSettings>(&saved)
                .unwrap()
                .atmos_label_assist
        );
        after.mode = SpatialBackendKind::SystemSpatial;
        after.layout = SpeakerLayout::TwentyTwoTwo;
        assert!(!after.atmos_label_applicable());
        after.layout = SpeakerLayout::NineOneSix;
        assert!(!after.atmos_label_applicable());
        after.layout = SpeakerLayout::SevenOneFour;
        assert!(after.atmos_label_applicable());
        assert!(!after.atmos_label_assist);
    }
}
