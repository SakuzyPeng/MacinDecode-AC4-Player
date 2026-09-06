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
mod install_check;
mod library;
mod licenses;
mod media;
mod model;
mod playlist;
mod playlist_ui;
mod preferences;
mod scene3d;
mod scene_view;
mod sofa_catalog;
mod theme;

fn main() {
    if let Err(error) = run() {
        eprintln!("MacinDecode AC-4 Player: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let options = install_check::Options::parse()?;
    let directory = match options.directory.clone().map_or_else(
        preferences::DataDirectory::acquire,
        preferences::DataDirectory::at,
    ) {
        Ok(directory) => directory,
        Err(error) => {
            if options.check || options.smoke {
                return Err(error.into());
            }
            rfd::MessageDialog::new()
                .set_title("MacinDecode AC-4 Player")
                .set_description(error)
                .set_level(rfd::MessageLevel::Error)
                .show();
            return Ok(());
        }
    };
    if options.check {
        return install_check::check(&directory).map_err(Into::into);
    }
    #[allow(unused_mut)]
    let mut native_options = eframe::NativeOptions {
        // Only the object scene needs MSAA and depth. It owns scene-sized
        // attachments; egui composites the resolved image in its normal pass.
        viewport: eframe::egui::ViewportBuilder::default()
            .with_app_id(preferences::APP_ID)
            .with_icon(app_icon::load())
            .with_inner_size([1_180.0, 760.0])
            .with_min_inner_size([920.0, 620.0])
            .with_title("MacinDecode AC-4 Player"),
        persistence_path: Some(directory.path.join("app.ron")),
        ..Default::default()
    };
    #[cfg(windows)]
    {
        use eframe::egui_wgpu::{WgpuSetup, WgpuSetupCreateNew, wgpu};
        let mut setup = WgpuSetupCreateNew::without_display_handle();
        setup
            .instance_descriptor
            .backend_options
            .dx12
            .shader_compiler = wgpu::Dx12Compiler::Fxc;
        native_options.wgpu_options.wgpu_setup = WgpuSetup::CreateNew(setup);
    }

    eframe::run_native(
        "MacinDecode AC-4 Player",
        native_options,
        Box::new(move |creation_context| {
            let mut app = app::PlayerApp::new(creation_context, directory.clone());
            if options.smoke {
                app.smoke = Some(install_check::WindowSmoke::new(directory.path.clone()));
            }
            Ok(Box::new(app))
        }),
    )?;
    Ok(())
}
