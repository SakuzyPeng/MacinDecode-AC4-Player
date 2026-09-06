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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputSettings {
    #[cfg(test)]
    #[serde(skip)]
    pub null_output: bool,
    pub mode: SpatialBackendKind,
    pub layout: SpeakerLayout,
    pub split_lfe: bool,
    pub atmos_label_assist: bool,
    pub sofa: String,
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
            && matches!(
                self.layout,
                SpeakerLayout::SevenOneFour | SpeakerLayout::NineOneSix
            )
    }
    pub fn validated(mut self) -> Self {
        if !self.mode.supported() {
            self.mode = SpatialBackendKind::Automatic;
        }
        if self.sofa.contains('\0') {
            self.sofa.clear();
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
        assert!(after.atmos_label_applicable());
        assert!(!after.atmos_label_assist);
    }
}
