use std::sync::{Arc, OnceLock};

use eframe::egui::IconData;

#[cfg(target_os = "macos")]
const PNG: &[u8] = include_bytes!("../assets/icons/app-macos.png");
#[cfg(not(target_os = "macos"))]
const PNG: &[u8] = include_bytes!("../assets/icons/app-windows.png");

/// Decode once: secondary viewport builders are recreated on every frame.
pub fn load() -> Arc<IconData> {
    static ICON: OnceLock<Arc<IconData>> = OnceLock::new();
    Arc::clone(ICON.get_or_init(|| {
        Arc::new(
            eframe::icon_data::from_png_bytes(PNG).expect("embedded app icon must be valid PNG"),
        )
    }))
}
