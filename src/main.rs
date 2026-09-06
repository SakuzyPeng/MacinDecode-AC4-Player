#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

#[cfg(target_arch = "wasm32")]
compile_error!(
    "MacinDecode AC-4 Player is a native desktop application; WebAssembly is unsupported"
);

mod app;
mod app_icon;
mod backend;
mod bitstream_ui;
pub mod decoder;
mod head_tracking;
mod inspection;
mod media;
mod model;
mod scene3d;
mod scene_view;
mod theme;

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        // Only the object scene needs MSAA and depth. It owns scene-sized
        // attachments; egui composites the resolved image in its normal pass.
        viewport: eframe::egui::ViewportBuilder::default()
            .with_app_id("com.macinrender.macindecode-ac4-player")
            .with_icon(app_icon::load())
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
