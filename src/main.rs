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
mod scene3d;
mod scene_view;
mod theme;

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        // The object scene renders into egui's own render pass, so the depth and
        // MSAA attachments have to come from eframe. Both values are read from
        // `scene3d::gpu` rather than written out here: the pipelines declare the
        // matching format and sample count, and a mismatch is a validation panic
        // at pipeline creation rather than a runtime downgrade.
        depth_buffer: scene3d::gpu::DEPTH_BUFFER_BITS,
        multisampling: scene3d::gpu::MSAA_SAMPLES,
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
