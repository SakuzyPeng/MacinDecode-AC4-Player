use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpatialBackendKind {
    #[default]
    Automatic,
    WindowsSpatialAudio,
    AppleAuSpatialMixer,
}

impl SpatialBackendKind {
    pub const ALL: [Self; 3] = [
        Self::Automatic,
        Self::WindowsSpatialAudio,
        Self::AppleAuSpatialMixer,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::WindowsSpatialAudio => "Windows Spatial Audio",
            Self::AppleAuSpatialMixer => "macOS AU Spatial Mixer",
        }
    }

    pub const fn availability(self) -> &'static str {
        match self {
            Self::Automatic => "Select a native backend when playback is implemented",
            Self::WindowsSpatialAudio => "Planned: dynamic objects plus one static LFE",
            Self::AppleAuSpatialMixer => "Planned: PointSource buses plus one LFE bus",
        }
    }
}

impl fmt::Display for SpatialBackendKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_labels_are_unique_and_non_empty() {
        for (index, backend) in SpatialBackendKind::ALL.iter().enumerate() {
            assert!(!backend.label().is_empty());
            assert!(
                SpatialBackendKind::ALL[..index]
                    .iter()
                    .all(|previous| previous.label() != backend.label())
            );
        }
    }
}
