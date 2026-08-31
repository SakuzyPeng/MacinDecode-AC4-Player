#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

#[cfg(target_arch = "wasm32")]
compile_error!(
    "MacinDecode AC-4 Player is a native desktop application; WebAssembly is unsupported"
);

mod app;
mod backend;
mod bitstream_ui;
pub mod decoder;
mod inspection;
mod model;
mod theme;

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_app_id("com.macinrender.macindecode-ac4-player")
            .with_inner_size([1_180.0, 760.0])
            .with_min_inner_size([920.0, 620.0])
            .with_title("MacinDecode AC-4 Player"),
        ..Default::default()
    };

    eframe::run_native(
        "MacinDecode AC-4 Player",
        native_options,
        Box::new(|creation_context| Ok(Box::new(app::PlayerApp::new(creation_context)))),
    )
}
