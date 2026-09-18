pub const EMBEDDED: bool = matches!(env!("MACINDECODE_HAS_LICENSES").as_bytes(), b"true");

pub struct License {
    pub name: String,
    pub heading: String,
    pub text: String,
    pub used_by: String,
}

/// An attribution that is not a crate, and so cannot come from cargo-about.
///
/// The demo track's notes were transcribed from a Mutopia typesetting. The
/// composition is public domain and the synthesis is ours, but the typesetting
/// is neither, so it is credited whether or not this build embedded the
/// dependency notices -- a development build shows an empty list otherwise,
/// and the obligation does not depend on how the binary was made.
fn demo_score() -> License {
    License {
        name: "CC BY 4.0".into(),
        heading: "CC BY 4.0 · Canon per 3 Violini e Basso (demo score)".into(),
        used_by: "Canon per 3 Violini e Basso, Mutopia-2015/09/02-2047".into(),
        text: "The built-in demo plays Johann Pachelbel's Canon in D \
               (Canon per 3 Violini e Basso, 1694), which is in the public domain. \
               It is synthesised in real time by this application; no recording is \
               included.\n\n\
               The notes were transcribed from the LilyPond typesetting published by \
               the Mutopia Project as Mutopia-2015/09/02-2047, maintained by \
               Michael Fischer v. Mollard, which is licensed under the Creative \
               Commons Attribution 4.0 International licence \
               (https://creativecommons.org/licenses/by/4.0/).\n\n\
               Source: https://www.mutopiaproject.org/"
            .into(),
    }
}

pub fn load() -> Vec<License> {
    let data: serde_json::Value =
        serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/licenses.json")))
            .expect("Embedded license report must be valid JSON");
    std::iter::once(demo_score())
        .chain(
            data["licenses"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|license| {
                    let first = license["used_by"][0]["crate"]["name"]
                        .as_str()
                        .unwrap_or("dependency");
                    let count = license["used_by"].as_array().map_or(0, Vec::len);
                    let name = license["name"].as_str().unwrap_or("License");
                    let heading = if count > 1 {
                        format!("{name} · {first} (+{})", count - 1)
                    } else {
                        format!("{name} · {first}")
                    };
                    License {
                        heading,
                        name: license["name"].as_str().unwrap_or("License").into(),
                        text: license["text"].as_str().unwrap_or_default().into(),
                        used_by: license["used_by"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|used| {
                                format!(
                                    "{} {}",
                                    used["crate"]["name"].as_str().unwrap_or_default(),
                                    used["crate"]["version"].as_str().unwrap_or_default()
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", "),
                    }
                }),
        )
        .collect()
}

#[derive(Default)]
pub struct Window {
    pub open: bool,
    filter: String,
    entries: Option<Vec<License>>,
}
impl Window {
    pub fn draw(&mut self, context: &eframe::egui::Context) {
        if !self.open {
            return;
        }
        let entries = self.entries.get_or_insert_with(load);
        eframe::egui::Window::new("About / third-party licenses")
            .open(&mut self.open)
            .default_width(700.0)
            .show(context, |ui| {
                ui.heading("MacinDecode AC-4 Player");
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                if !EMBEDDED {
                    ui.label("Development build: the installer build embeds the complete notices.");
                }
                ui.add(
                    eframe::egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("Search dependencies or licenses"),
                );
                let filter = self.filter.to_lowercase();
                eframe::egui::ScrollArea::vertical()
                    .max_height(520.0)
                    .show(ui, |ui| {
                        for (index, license) in entries.iter().enumerate() {
                            if !license.name.to_lowercase().contains(&filter)
                                && !license.used_by.to_lowercase().contains(&filter)
                            {
                                continue;
                            }
                            eframe::egui::CollapsingHeader::new(&license.heading)
                                .id_salt(index)
                                .show(ui, |ui| {
                                    ui.label(&license.used_by);
                                    ui.add(
                                        eframe::egui::Label::new(&license.text)
                                            .selectable(true)
                                            .wrap(),
                                    );
                                });
                        }
                    });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_score_is_credited_in_every_build() {
        // cargo-about only reports crates, and a development build embeds an
        // empty report, so nothing in the generated list would carry this. The
        // obligation does not depend on how the binary was made.
        let first = load().into_iter().next().expect("an About entry");
        assert_eq!(first.name, "CC BY 4.0");
        assert!(first.used_by.contains("Mutopia-2015/09/02-2047"));
        assert!(
            first.text.contains("Michael Fischer v. Mollard"),
            "the attribution must name the maintainer it credits"
        );
        assert!(
            first
                .text
                .contains("no recording is\n               included")
                || first.text.contains("no recording is included"),
            "and must say the audio is synthesised rather than recorded"
        );
    }
}
