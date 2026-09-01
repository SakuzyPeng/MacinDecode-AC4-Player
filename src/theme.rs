use eframe::egui::{
    self, Color32, CornerRadius, Stroke,
    epaint::text::{FontInsert, FontPriority, InsertFontFamily},
};

const NOTO_SANS_CJK_SC: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/NotoSansCJKsc-Regular.otf"));

// Warm, paper-like tokens adapted to the visual language of shannon-player.
pub const BACKGROUND: Color32 = Color32::from_rgb(251, 247, 240);
pub const SURFACE: Color32 = Color32::from_rgb(255, 254, 250);
pub const STAGE: Color32 = Color32::from_rgb(248, 243, 234);
pub const HOVER: Color32 = Color32::from_rgb(241, 233, 220);
pub const BORDER: Color32 = Color32::from_rgb(236, 227, 212);
pub const TEXT: Color32 = Color32::from_rgb(55, 44, 32);
pub const MUTED: Color32 = Color32::from_rgb(154, 139, 118);
pub const ACCENT: Color32 = Color32::from_rgb(206, 122, 59);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(244, 226, 209);
pub const INK: Color32 = Color32::from_rgb(51, 42, 31);
pub const SUCCESS: Color32 = Color32::from_rgb(102, 137, 105);
pub const WARNING: Color32 = Color32::from_rgb(176, 72, 58);

pub fn install(context: &egui::Context) {
    install_ui_font(context);

    let mut visuals = egui::Visuals::light();
    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = SURFACE;
    visuals.faint_bg_color = HOVER;
    visuals.extreme_bg_color = STAGE;
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.hyperlink_color = ACCENT;
    visuals.window_corner_radius = CornerRadius::ZERO;
    visuals.menu_corner_radius = CornerRadius::ZERO;
    visuals.widgets.noninteractive.bg_fill = SURFACE;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, MUTED);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::ZERO;
    visuals.widgets.inactive.weak_bg_fill = SURFACE;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.inactive.corner_radius = CornerRadius::ZERO;
    visuals.widgets.hovered.weak_bg_fill = HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.hovered.corner_radius = CornerRadius::ZERO;
    visuals.widgets.active.weak_bg_fill = ACCENT_SOFT;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, INK);
    visuals.widgets.active.corner_radius = CornerRadius::ZERO;
    context.set_visuals(visuals);

    context.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 9.0);
        style.spacing.button_padding = egui::vec2(14.0, 8.0);
        style.spacing.interact_size.y = 36.0;
        style.spacing.slider_width = 220.0;
    });
}

fn install_ui_font(context: &egui::Context) {
    context.add_font(FontInsert::new(
        "noto-sans-cjk-sc",
        egui::FontData::from_static(NOTO_SANS_CJK_SC),
        vec![
            InsertFontFamily {
                family: egui::FontFamily::Proportional,
                priority: FontPriority::Highest,
            },
            InsertFontFamily {
                family: egui::FontFamily::Monospace,
                priority: FontPriority::Lowest,
            },
        ],
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloaded_ui_font_supports_latin_chinese_and_japanese() {
        let context = egui::Context::default();
        install_ui_font(&context);
        context.begin_pass(egui::RawInput::default());
        let (is_primary_ui_font, supports_proportional_text, supports_monospace_cjk) = context
            .fonts_mut(|fonts| {
                let is_primary_ui_font = fonts
                    .definitions()
                    .families
                    .get(&egui::FontFamily::Proportional)
                    .and_then(|family| family.first())
                    .is_some_and(|name| name == "noto-sans-cjk-sc");
                let supports_proportional_text = fonts.has_glyphs(
                    &egui::FontId::proportional(12.0),
                    "MacinDecode 中文日本語テスト",
                );
                let supports_monospace_cjk =
                    fonts.has_glyphs(&egui::FontId::monospace(12.0), "中文日本語テスト");
                (
                    is_primary_ui_font,
                    supports_proportional_text,
                    supports_monospace_cjk,
                )
            });
        let mut output = context.end_pass();
        output.textures_delta.clear();

        assert!(is_primary_ui_font);
        assert!(supports_proportional_text);
        assert!(supports_monospace_cjk);
    }
}
