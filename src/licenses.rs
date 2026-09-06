pub const EMBEDDED: bool = matches!(env!("MACINDECODE_HAS_LICENSES").as_bytes(), b"true");

pub struct License {
    pub name: String,
    pub heading: String,
    pub text: String,
    pub used_by: String,
}

pub fn load() -> Vec<License> {
    let data: serde_json::Value =
        serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/licenses.json")))
            .expect("Embedded license report must be valid JSON");
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
        })
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
