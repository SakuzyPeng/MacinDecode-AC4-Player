//! Content reference frames, independent of the sensor and output-device policy.
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
pub enum TrackingIssue {
    ReservedOperationMode(u8),
    ReservedObjectMode(u8),
    MissingGlobalControl,
    UnboundSource,
    ConflictingGroups,
    Unknown,
}

impl fmt::Display for TrackingIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReservedOperationMode(value) => {
                write!(f, "Reserved headphone operation mode {value}")
            }
            Self::ReservedObjectMode(value) => write!(f, "Reserved object headphone mode {value}"),
            Self::MissingGlobalControl => f.write_str("Missing global tracking control"),
            Self::UnboundSource => f.write_str("No associated audio group"),
            Self::ConflictingGroups => {
                f.write_str("Audio groups specify conflicting content policies")
            }
            Self::Unknown => f.write_str("Unrecognized content tracking policy"),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "decode"), allow(dead_code))]
pub enum ContentHeadTracking {
    #[default]
    Unspecified,
    SceneRelative,
    HeadRelative,
    Unsupported(TrackingIssue),
}

impl ContentHeadTracking {
    /// Missing and unsupported declarations both use the world-space default.
    pub const fn head_locked(self) -> bool {
        matches!(self, Self::HeadRelative)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TrackingSummary {
    pub scene_relative: usize,
    pub head_relative: usize,
    pub unspecified: usize,
    pub unsupported: usize,
    pub first_issue: Option<(u64, TrackingIssue)>,
}

impl TrackingSummary {
    pub fn observe(&mut self, element_id: u64, tracking: ContentHeadTracking) {
        match tracking {
            ContentHeadTracking::SceneRelative => self.scene_relative += 1,
            ContentHeadTracking::HeadRelative => self.head_relative += 1,
            ContentHeadTracking::Unspecified => self.unspecified += 1,
            ContentHeadTracking::Unsupported(issue) => {
                self.unsupported += 1;
                self.first_issue.get_or_insert((element_id, issue));
            }
        }
    }
}
