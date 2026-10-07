//! APAC container input and the qualified Core Audio discrete channel orders.
//! Speaker angles follow the Apple geometry used by the output renderer.
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::sync::Arc;

use apac_core::model::ChannelLayout;

use crate::media::{MediaCursor, OpenedMedia};

pub(crate) struct Source {
    reader: BufReader<MediaCursor>,
    bytes: u64,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl Source {
    pub(crate) fn new(
        media: &Arc<OpenedMedia>,
        cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            reader: media.reader(),
            bytes: media.file_len(),
            cancelled,
        }
    }

    fn check(&self) -> io::Result<()> {
        if (self.cancelled)() {
            // Interrupted is retried by read_exact, so use a terminal error.
            Err(io::Error::other("APAC operation cancelled"))
        } else {
            Ok(())
        }
    }
}

impl Read for Source {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        self.reader.read(out)
    }
}
impl Seek for Source {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.check()?;
        self.reader.seek(position)
    }
}
impl apac_container::Source for Source {
    fn length(&mut self) -> io::Result<u64> {
        self.check()?;
        Ok(self.bytes)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Destination {
    Speaker {
        azimuth: f32,
        elevation: f32,
    },
    /// Semantic order, not Core Audio's numeric label: `CICP_13`'s LFE2/LFE3
    /// are the renderer's LFE1/LFE2 respectively.
    Lfe,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Channel {
    pub name: &'static str,
    #[cfg_attr(not(feature = "decode"), allow(dead_code))]
    pub destination: Destination,
}
const fn speaker(name: &'static str, azimuth: f32, elevation: f32) -> Channel {
    Channel {
        name,
        destination: Destination::Speaker { azimuth, elevation },
    }
}
const fn lfe(name: &'static str) -> Channel {
    Channel {
        name,
        destination: Destination::Lfe,
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub name: &'static str,
    pub channels: &'static [Channel],
}
impl Layout {
    #[allow(
        clippy::too_many_lines,
        reason = "qualified discrete channel orders are kept together for auditing"
    )]
    pub(crate) fn from_core(layout: &ChannelLayout, count: u32) -> Result<Self, String> {
        if layout.ambisonic_order.is_some() || matches!(layout.tag >> 16, 190 | 191) {
            return Err("APAC HOA playback needs an ambisonic renderer; this player currently supports discrete channels".into());
        }
        let expected = ChannelLayout::discrete(count)
            .ok_or_else(|| format!("Unsupported APAC channel layout {:#010x}", layout.tag))?;
        if !layout.equivalent(&expected) {
            return Err(format!(
                "Unsupported APAC channel layout {:#010x}; channel count alone does not define speaker positions",
                layout.tag
            ));
        }
        let (name, channels): (_, &[_]) = match count {
            1 => const { ("Mono", &[speaker("Mono", 0.0, 0.0)] as &[Channel]) },
            2 => {
                const {
                    (
                        "Stereo",
                        &[speaker("L", 30.0, 0.0), speaker("R", -30.0, 0.0)] as &[Channel],
                    )
                }
            }
            6 => {
                const {
                    (
                        "5.1",
                        &[
                            speaker("L", 30.0, 0.0),
                            speaker("R", -30.0, 0.0),
                            speaker("C", 0.0, 0.0),
                            lfe("LFE"),
                            speaker("Ls", 110.0, 0.0),
                            speaker("Rs", -110.0, 0.0),
                        ] as &[Channel],
                    )
                }
            }
            8 => {
                const {
                    (
                        "7.1",
                        &[
                            speaker("L", 30.0, 0.0),
                            speaker("R", -30.0, 0.0),
                            speaker("C", 0.0, 0.0),
                            lfe("LFE"),
                            speaker("Ls", 110.0, 0.0),
                            speaker("Rs", -110.0, 0.0),
                            speaker("Rls", 150.0, 0.0),
                            speaker("Rrs", -150.0, 0.0),
                        ] as &[Channel],
                    )
                }
            }
            12 => {
                const {
                    (
                        "7.1.4",
                        &[
                            speaker("L", 30.0, 0.0),
                            speaker("R", -30.0, 0.0),
                            speaker("C", 0.0, 0.0),
                            lfe("LFE"),
                            speaker("Ls", 110.0, 0.0),
                            speaker("Rs", -110.0, 0.0),
                            speaker("Rls", 150.0, 0.0),
                            speaker("Rrs", -150.0, 0.0),
                            speaker("Vhl", 45.0, 45.0),
                            speaker("Vhr", -45.0, 45.0),
                            speaker("Ltr", 135.0, 45.0),
                            speaker("Rtr", -135.0, 45.0),
                        ] as &[Channel],
                    )
                }
            }
            16 => {
                const {
                    (
                        "9.1.6",
                        &[
                            speaker("L", 30.0, 0.0),
                            speaker("R", -30.0, 0.0),
                            speaker("C", 0.0, 0.0),
                            lfe("LFE"),
                            speaker("Ls", 110.0, 0.0),
                            speaker("Rs", -110.0, 0.0),
                            speaker("Rls", 150.0, 0.0),
                            speaker("Rrs", -150.0, 0.0),
                            speaker("Lw", 60.0, 0.0),
                            speaker("Rw", -60.0, 0.0),
                            speaker("Vhl", 45.0, 45.0),
                            speaker("Vhr", -45.0, 45.0),
                            speaker("Ltm", 90.0, 45.0),
                            speaker("Rtm", -90.0, 45.0),
                            speaker("Ltr", 135.0, 45.0),
                            speaker("Rtr", -135.0, 45.0),
                        ] as &[Channel],
                    )
                }
            }
            24 => {
                const {
                    (
                        "22.2",
                        &[
                            speaker("Lw", 60.0, 0.0),
                            speaker("Rw", -60.0, 0.0),
                            speaker("C", 0.0, 0.0),
                            lfe("LFE2"),
                            speaker("Rls", 150.0, 0.0),
                            speaker("Rrs", -150.0, 0.0),
                            speaker("L", 30.0, 0.0),
                            speaker("R", -30.0, 0.0),
                            speaker("Cs", 180.0, 0.0),
                            lfe("LFE3"),
                            speaker("Lss", 90.0, 0.0),
                            speaker("Rss", -90.0, 0.0),
                            speaker("Vhl", 45.0, 45.0),
                            speaker("Vhr", -45.0, 45.0),
                            speaker("Vhc", 0.0, 30.0),
                            speaker("Ts", 0.0, 90.0),
                            speaker("Ltr", 135.0, 45.0),
                            speaker("Rtr", -135.0, 45.0),
                            speaker("Ltm", 90.0, 45.0),
                            speaker("Rtm", -90.0, 45.0),
                            speaker("Ctr", 180.0, 45.0),
                            speaker("Cb", 0.0, -15.0),
                            speaker("Lb", 45.0, -15.0),
                            speaker("Rb", -45.0, -15.0),
                        ] as &[Channel],
                    )
                }
            }
            _ => return Err("Unsupported APAC discrete layout".into()),
        };
        Ok(Self { name, channels })
    }
}

#[derive(Debug)]
pub(crate) struct Report {
    pub summary: [String; 3],
    pub fields: Vec<(String, String)>,
}
impl Report {
    pub(crate) fn read(source: Source) -> Result<Self, String> {
        let media = apac_container::Media::open(source).map_err(|error| error.to_string())?;
        let track = media.track();
        let layout = Layout::from_core(&track.layout, track.channels);
        let layout_name = layout.as_ref().map_or_else(
            |_| {
                track
                    .layout
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("{:#010x}", track.layout.tag))
            },
            |layout| layout.name.to_owned(),
        );
        let channels = layout.as_ref().map_or_else(
            |_| "See layout metadata".into(),
            |layout| {
                layout
                    .channels
                    .iter()
                    .map(|ch| ch.name)
                    .collect::<Vec<_>>()
                    .join(" ")
            },
        );
        let playback = layout.map_or_else(|error| error, |_| "Discrete channel playback".into());
        let fields = vec![
            ("Codec".into(), "Apple Positional Audio Codec (APAC)".into()),
            ("Container".into(), media.container().to_uppercase()),
            ("Sample rate".into(), format!("{} Hz", track.sample_rate)),
            ("Layout".into(), format!("{layout_name} · {} channels", track.channels)),
            ("Channel order".into(), channels),
            ("Layout tag".into(), format!("{:#010x}", track.layout.tag)),
            ("Valid frames".into(), track.table.valid_frames.to_string()),
            ("Priming / remainder".into(), format!("{} / {} frames", track.table.priming_frames, track.table.remainder_frames)),
            ("Packets".into(), track.packet_count.to_string()),
            ("Playback".into(), playback),
            ("22.2 LFE".into(), "CICP_13 LFE2 / LFE3 → renderer LFE1 / LFE2; 22.2 Direct preserves both; equal-power copy normalizes the sum when both have signal".into()),
            ("Metadata processing".into(), "DRC, loudness and scene/renderer metadata are not applied".into()),
        ];
        Ok(Self {
            summary: [
                format!("APAC · {} Hz", track.sample_rate),
                format!("{layout_name} · {} ch", track.channels),
                "Not applied".into(),
            ],
            fields,
        })
    }

    pub(crate) fn render_text(&self) -> String {
        use std::fmt::Write as _;
        let mut text = String::new();
        for (key, value) in &self.fields {
            writeln!(text, "{key}: {value}").expect("writing a String");
        }
        text
    }
}
