use std::path::PathBuf;

use eframe::egui::{
    self, Color32, CornerRadius, Stroke,
    epaint::text::{FontInsert, FontPriority, InsertFontFamily},
};

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
    install_cjk_fallback(context);

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

fn install_cjk_fallback(context: &egui::Context) {
    let Some((font, index)) = system_cjk_font_candidates()
        .into_iter()
        .find_map(|(path, index)| std::fs::read(path).ok().map(|font| (font, index)))
    else {
        return;
    };

    context.add_font(FontInsert::new(
        "system-cjk-fallback",
        egui::FontData {
            font: font.into(),
            index,
            tweak: egui::FontTweak::default(),
        },
        vec![
            InsertFontFamily {
                family: egui::FontFamily::Proportional,
                priority: FontPriority::Lowest,
            },
            InsertFontFamily {
                family: egui::FontFamily::Monospace,
                priority: FontPriority::Lowest,
            },
        ],
    ));
}

#[cfg(target_os = "windows")]
fn system_cjk_font_candidates() -> Vec<(PathBuf, u32)> {
    let windows_dir =
        std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let fonts_dir = windows_dir.join("Fonts");
    [
        "msyh.ttc",
        "YuGothR.ttc",
        "YuGothM.ttc",
        "meiryo.ttc",
        "msgothic.ttc",
        "simsun.ttc",
        "msmincho.ttc",
    ]
    .into_iter()
    .map(|name| (fonts_dir.join(name), 0))
    .collect()
}

#[cfg(target_os = "macos")]
fn system_cjk_font_candidates() -> Vec<(PathBuf, u32)> {
    [
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
        "/System/Library/Fonts/ヒラギノ角ゴシック W4.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
    ]
    .into_iter()
    .map(|path| (PathBuf::from(path), 0))
    .collect()
}

#[cfg(target_os = "linux")]
fn system_cjk_font_candidates() -> Vec<(PathBuf, u32)> {
    [
        "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJKjp-Regular.otf",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/ipafont-gothic/ipag.ttf",
    ]
    .into_iter()
    .map(|path| (PathBuf::from(path), 0))
    .collect()
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn system_cjk_font_candidates() -> Vec<(PathBuf, u32)> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_system_cjk_fallback_supports_chinese_and_japanese() {
        if !system_cjk_font_candidates()
            .iter()
            .any(|(path, _)| path.is_file())
        {
            return;
        }

        let context = egui::Context::default();
        install_cjk_fallback(&context);
        context.begin_pass(egui::RawInput::default());
        let supports_cjk = context.fonts_mut(|fonts| {
            [egui::FontFamily::Proportional, egui::FontFamily::Monospace]
                .into_iter()
                .all(|family| {
                    fonts.has_glyphs(&egui::FontId::new(12.0, family), "中文日本語テスト")
                })
        });
        let mut output = context.end_pass();
        output.textures_delta.clear();

        assert!(supports_cjk);
    }
}
