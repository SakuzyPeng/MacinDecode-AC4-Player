use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::library::{LibraryController, Notice};
use crate::preferences::{AppPreferences, DataDirectory};

#[derive(Default)]
pub struct Options {
    pub directory: Option<PathBuf>,
    pub check: bool,
    pub smoke: bool,
}
impl Options {
    pub fn parse() -> Result<Self, String> {
        let mut options = Self::default();
        let mut arguments = std::env::args_os().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some("--data-dir") => {
                    options.directory =
                        Some(arguments.next().ok_or("--data-dir requires a path")?.into());
                }
                Some("--check-install") => options.check = true,
                Some("--smoke-test") => options.smoke = true,
                _ => return Err(format!("Unknown argument: {}", argument.to_string_lossy())),
            }
        }
        if (options.check || options.smoke) && options.directory.is_none() {
            return Err("Installation checks require an isolated --data-dir".into());
        }
        if options.check && options.smoke {
            return Err("Run installation and window checks separately".into());
        }
        Ok(options)
    }
}

fn load(directory: Arc<DataDirectory>) -> Result<(LibraryController, AppPreferences), String> {
    let context = eframe::egui::Context::default();
    let mut library = LibraryController::new(directory, AppPreferences::default(), context);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut preferences = None;
    loop {
        for notice in library.poll() {
            if let Notice::Boot(saved, _) = notice {
                preferences = Some(*saved);
            }
        }
        if let Some(error) = &library.error {
            return Err(error.clone());
        }
        if library.ready {
            return Ok((library, preferences.ok_or("Missing restored preferences")?));
        }
        if Instant::now() >= deadline {
            return Err("Library initialization timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn check(directory: &Arc<DataDirectory>) -> Result<(), String> {
    let (mut library, original) = load(directory.clone())?;
    let mut changed = original.clone();
    changed.volume = 0.375;
    changed.muted = !original.muted;
    library.save_preferences(changed.clone());
    library.shutdown();
    if let Some(error) = library.error.take() {
        return Err(error);
    }
    drop(library);
    let (mut reopened, saved) = load(directory.clone())?;
    if changed != saved {
        return Err("Preferences did not survive reopening the library".into());
    }
    reopened.save_preferences(original);
    reopened.shutdown();
    if let Some(error) = reopened.error.take() {
        return Err(error);
    }
    #[cfg(macinrender_output)]
    {
        use macindecode_macinrender::{Config, OutputKind, RendererSettings, Session};
        let config = Config {
            renderer: RendererSettings {
                binaural: false,
                layout: "4+7+0".into(),
                sofa: String::new(),
                split_lfe: true,
            },
            output: OutputKind::Null,
            device_id: String::new(),
            input_rate: 48_000,
        };
        let _session = Session::new(&config)?;
    }
    let report = serde_json::json!({"ok":true, "version":env!("CARGO_PKG_VERSION"), "embedded_licenses":crate::licenses::EMBEDDED,
        "decode":cfg!(feature="decode"), "macinrender":cfg!(macinrender_output), "sqlite":rusqlite::version()});
    std::fs::write(
        directory.path.join("install-check.json"),
        report.to_string(),
    )
    .map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(())
}

pub struct WindowSmoke {
    root: PathBuf,
    started: Instant,
    reported: Option<Instant>,
    frames: u64,
}
impl WindowSmoke {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            started: Instant::now(),
            reported: None,
            frames: 0,
        }
    }
    pub fn frame(
        &mut self,
        context: &eframe::egui::Context,
        ready: bool,
        error: Option<&str>,
        renderer: bool,
    ) {
        self.frames += 1;
        if self.reported.is_none()
            && self.started.elapsed() >= Duration::from_secs(3)
            && self.frames >= 2
        {
            let report = serde_json::json!({"ok":ready && error.is_none() && renderer, "rendered_frames":self.frames,
                "scene_renderer_ready":renderer, "embedded_licenses":crate::licenses::EMBEDDED, "storage_warning":error});
            if let Err(error) =
                std::fs::write(self.root.join("smoke-report.json"), report.to_string())
            {
                eprintln!("Smoke report: {error}");
            }
            self.reported = Some(Instant::now());
        }
        if self
            .reported
            .is_some_and(|at| at.elapsed() > Duration::from_secs(1))
        {
            context.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
        }
        context.request_repaint_after(Duration::from_millis(100));
    }
}
