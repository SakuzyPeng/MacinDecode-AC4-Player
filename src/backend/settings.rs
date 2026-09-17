use super::{OutputDeviceSelection, SpatialBackendKind};
use crate::head_tracking::HeadSource;
use serde::{Deserialize, Serialize};

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
    pub layout: SpeakerLayout,
    pub split_lfe: bool,
    pub atmos_label_assist: bool,
    pub sofa: String,
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
}
impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            #[cfg(test)]
            null_output: false,
            mode: SpatialBackendKind::Automatic,
            layout: SpeakerLayout::default(),
            split_lfe: true,
            atmos_label_assist: true,
            sofa: String::new(),
            hptf: String::new(),
            hptf_auto_trim: false,
            hptf_bass_db: 0.0,
            hptf_tilt_db: 0.0,
            native_device: OutputDeviceSelection::SystemDefault,
            stereo_device: OutputDeviceSelection::SystemDefault,
            head_source: HeadSource::Automatic,
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
    pub fn validated(mut self) -> Self {
        if !self.mode.supported() {
            self.mode = SpatialBackendKind::Automatic;
        }
        if self.sofa.contains('\0') {
            self.sofa.clear();
        }
        if self.hptf.contains('\0') {
            self.hptf.clear();
        }
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
                && self.layout != other.layout)
            || self.native_device != other.native_device
            || self.stereo_device != other.stereo_device
    }
    #[cfg(macinrender_output)]
    pub fn renderer(&self) -> macindecode_macinrender::RendererSettings {
        macindecode_macinrender::RendererSettings {
            binaural: self.mode.resolved() == SpatialBackendKind::SafBinaural,
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
