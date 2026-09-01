use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Align, Color32, Layout, RichText, Stroke};

use crate::backend::{
    OutputDeviceSelection, OutputPhase, OutputSnapshot, OutputStreamConfig, SpatialBackendKind,
    SpatialOutputController,
};
use crate::bitstream_ui::{self, BitstreamAction};
use crate::decoder::{
    DecodeMetrics, DecodePhase, DecoderController, DecoderSnapshot, PREBUFFER_MILLISECONDS,
};
use crate::inspection::InspectionController;
use crate::model::SelectedSource;
use crate::theme;

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent UI toggles and pointer interaction flags are not one state machine"
)]
pub struct PlayerApp {
    playlist: Vec<SelectedSource>,
    selected_source: Option<usize>,
    inspection: InspectionController,
    decoder: DecoderController,
    decoder_revision: u64,
    output: SpatialOutputController,
    output_revision: u64,
    backend: SpatialBackendKind,
    status: StatusLine,
    timeline_preview: f32,
    timeline_dragging: bool,
    playback_restore_pending: bool,
    playback_intent: bool,
    playback_mode: PlaybackMode,
    shuffle_history: Vec<usize>,
    shuffle_state: u64,
    automatic_reconfigure_guard: Option<(u64, u64)>,
    waiting_for_device: Option<DeviceWait>,
    volume: f32,
    muted: bool,
    bitstream_details_open: bool,
    diagnostics_open: bool,
}

const OUTPUT_DEVICE_STORAGE_KEY: &str = "preferred-output-device-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputSyncAction {
    Configure,
    Preserve,
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaylistStep {
    Previous,
    Next,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum PlaybackMode {
    #[default]
    Sequential,
    RepeatOne,
    RepeatAll,
    Shuffle,
}

impl PlaybackMode {
    const ALL: [Self; 4] = [
        Self::Sequential,
        Self::RepeatOne,
        Self::RepeatAll,
        Self::Shuffle,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Sequential => "Sequential",
            Self::RepeatOne => "Repeat one",
            Self::RepeatAll => "Repeat all",
            Self::Shuffle => "Shuffle",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Sequential => "Play the list once, then stop",
            Self::RepeatOne => "Repeat the current item",
            Self::RepeatAll => "Repeat the entire playlist",
            Self::Shuffle => "Choose a different item at random",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeviceWait {
    request_id: u64,
    playback_epoch: u64,
    frame: u64,
}

impl DeviceWait {
    const fn belongs_to(self, request_id: u64, playback_epoch: u64) -> bool {
        self.request_id == request_id && self.playback_epoch == playback_epoch
    }
}

const fn output_sync_action(
    decode_phase: DecodePhase,
    configured_for_playback: bool,
) -> OutputSyncAction {
    match (decode_phase, configured_for_playback) {
        (DecodePhase::Ready | DecodePhase::EndOfStream, false) => OutputSyncAction::Configure,
        (
            DecodePhase::Seeking
            | DecodePhase::Buffering
            | DecodePhase::Ready
            | DecodePhase::EndOfStream,
            true,
        )
        | (DecodePhase::Seeking | DecodePhase::Buffering, false) => OutputSyncAction::Preserve,
        _ => OutputSyncAction::Reset,
    }
}

fn take_playback_restore(pending: &mut bool, playback_intent: bool) -> Option<bool> {
    std::mem::take(pending).then_some(playback_intent)
}

fn neighboring_source_index(
    selected_source: Option<usize>,
    item_count: usize,
    step: PlaylistStep,
) -> Option<usize> {
    let selected = selected_source.filter(|index| *index < item_count)?;
    match step {
        PlaylistStep::Previous => selected.checked_sub(1),
        PlaylistStep::Next => selected.checked_add(1).filter(|next| *next < item_count),
    }
}

fn wrapped_source_index(
    selected_source: Option<usize>,
    item_count: usize,
    step: PlaylistStep,
) -> Option<usize> {
    if item_count < 2 {
        return None;
    }
    let selected = selected_source.filter(|index| *index < item_count)?;
    Some(match step {
        PlaylistStep::Previous => selected.checked_sub(1).unwrap_or(item_count - 1),
        PlaylistStep::Next => selected
            .checked_add(1)
            .filter(|next| *next < item_count)
            .unwrap_or(0),
    })
}

fn shuffle_seed() -> u64 {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_secs() ^ u64::from(duration.subsec_nanos())
        });
    if seed == 0 {
        0x9E37_79B9_7F4A_7C15
    } else {
        seed
    }
}

fn shuffled_source_index(
    selected_source: Option<usize>,
    item_count: usize,
    state: &mut u64,
) -> Option<usize> {
    let selected = selected_source.filter(|index| *index < item_count)?;
    let candidate_count = item_count.checked_sub(1)?;
    if candidate_count == 0 {
        return None;
    }

    let mut value = if *state == 0 {
        0x9E37_79B9_7F4A_7C15
    } else {
        *state
    };
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    *state = value;

    let candidate_count = u64::try_from(candidate_count).unwrap_or(u64::MAX);
    let candidate = usize::try_from(value % candidate_count).unwrap_or(0);
    Some(if candidate >= selected {
        candidate + 1
    } else {
        candidate
    })
}

fn should_handle_completed_item(
    output_phase: OutputPhase,
    playback_intent: bool,
    output_matches_decoder: bool,
) -> bool {
    output_phase == OutputPhase::Ended && playback_intent && output_matches_decoder
}

const fn should_replay_current_on_completion(mode: PlaybackMode, item_count: usize) -> bool {
    match mode {
        PlaybackMode::RepeatOne => true,
        PlaybackMode::RepeatAll => item_count == 1,
        PlaybackMode::Sequential | PlaybackMode::Shuffle => false,
    }
}

impl PlayerApp {
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        theme::install(&creation_context.egui_ctx);
        let preferred_device = creation_context
            .storage
            .and_then(|storage| eframe::get_value(storage, OUTPUT_DEVICE_STORAGE_KEY))
            .unwrap_or_default();
        let mut output = SpatialOutputController::new();
        output.set_preferred_device(preferred_device);
        Self {
            playlist: Vec::new(),
            selected_source: None,
            inspection: InspectionController::new(),
            decoder: DecoderController::new(),
            decoder_revision: 0,
            output,
            output_revision: 0,
            backend: SpatialBackendKind::Automatic,
            status: StatusLine::idle("Add or drop AC-4 media files"),
            timeline_preview: 0.0,
            timeline_dragging: false,
            playback_restore_pending: false,
            playback_intent: false,
            playback_mode: PlaybackMode::default(),
            shuffle_history: Vec::new(),
            shuffle_state: shuffle_seed(),
            automatic_reconfigure_guard: None,
            waiting_for_device: None,
            volume: 0.8,
            muted: false,
            bitstream_details_open: false,
            diagnostics_open: false,
        }
    }

    fn choose_sources(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .set_title("Add AC-4 media to playlist")
            .add_filter("AC-4 media", &["m4a", "mp4", "ac4"])
            .pick_files()
        {
            self.append_sources(paths);
        }
    }

    fn append_sources(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        let mut added = 0;
        let mut duplicates = 0;
        let mut rejected = 0;

        for path in paths {
            match SelectedSource::from_path(path) {
                Ok(source) => {
                    if self
                        .playlist
                        .iter()
                        .any(|item| item.path() == source.path())
                    {
                        duplicates += 1;
                    } else {
                        self.playlist.push(source);
                        added += 1;
                    }
                }
                Err(_) => rejected += 1,
            }
        }

        if self.selected_source.is_none() && !self.playlist.is_empty() {
            self.selected_source = Some(0);
        }

        if added > 0 {
            let noun = if added == 1 { "file" } else { "files" };
            self.status = StatusLine::ready(format!("Added {added} {noun} to the playlist"));
        } else if duplicates > 0 && rejected == 0 {
            self.status = StatusLine::idle("Selected files are already in the playlist");
        } else if rejected > 0 {
            self.status = StatusLine::warning("No supported AC-4 media was added");
        }
    }

    fn select_source(&mut self, index: usize) {
        if self.selected_source == Some(index) {
            return;
        }
        self.shuffle_history.clear();
        self.select_source_with_playback(index, false);
    }

    fn select_source_with_playback(&mut self, index: usize, playback_intent: bool) {
        if self.selected_source == Some(index) {
            return;
        }
        if let Some(source) = self.playlist.get(index) {
            let name = source.display_name().to_owned();
            self.output.pause();
            self.output.reset();
            self.selected_source = Some(index);
            self.timeline_preview = 0.0;
            self.playback_intent = playback_intent;
            self.playback_restore_pending = playback_intent;
            self.automatic_reconfigure_guard = None;
            self.waiting_for_device = None;
            self.status = if playback_intent {
                StatusLine::ready(format!(
                    "Advancing to {name}; opening MacinDecode Core for playback"
                ))
            } else {
                StatusLine::ready(format!("Selected {name}; opening MacinDecode Core"))
            };
        }
    }

    fn can_select_neighbor(&self, step: PlaylistStep) -> bool {
        match (self.playback_mode, step) {
            (PlaybackMode::RepeatAll, _) => {
                wrapped_source_index(self.selected_source, self.playlist.len(), step).is_some()
            }
            (PlaybackMode::Shuffle, PlaylistStep::Previous) => !self.shuffle_history.is_empty(),
            (PlaybackMode::Shuffle, PlaylistStep::Next) => {
                self.playlist.len() > 1 && self.has_selected_source()
            }
            (PlaybackMode::Sequential | PlaybackMode::RepeatOne, _) => {
                neighboring_source_index(self.selected_source, self.playlist.len(), step).is_some()
            }
        }
    }

    fn neighbor_for_playback_mode(&mut self, step: PlaylistStep) -> Option<usize> {
        match (self.playback_mode, step) {
            (PlaybackMode::RepeatAll, _) => {
                wrapped_source_index(self.selected_source, self.playlist.len(), step)
            }
            (PlaybackMode::Shuffle, PlaylistStep::Previous) => self.shuffle_history.pop(),
            (PlaybackMode::Shuffle, PlaylistStep::Next) => {
                let current = self.selected_source?;
                let next = shuffled_source_index(
                    self.selected_source,
                    self.playlist.len(),
                    &mut self.shuffle_state,
                )?;
                self.shuffle_history.push(current);
                Some(next)
            }
            (PlaybackMode::Sequential | PlaybackMode::RepeatOne, _) => {
                neighboring_source_index(self.selected_source, self.playlist.len(), step)
            }
        }
    }

    fn select_neighbor(&mut self, step: PlaylistStep) {
        if let Some(index) = self.neighbor_for_playback_mode(step) {
            self.select_source_with_playback(index, self.playback_intent);
        }
    }

    fn replay_current_source(&mut self) {
        self.playback_intent = true;
        self.automatic_reconfigure_guard = None;
        self.waiting_for_device = None;
        match self.decoder.seek(0) {
            Ok(()) => {
                self.timeline_preview = 0.0;
                self.playback_restore_pending = true;
                self.status = StatusLine::idle("Replaying from the beginning");
            }
            Err(error) => {
                self.playback_intent = false;
                self.playback_restore_pending = false;
                self.status = StatusLine::warning(error);
            }
        }
    }

    fn handle_completed_playlist_item(
        &mut self,
        context: &egui::Context,
        output_matches_decoder: bool,
    ) -> bool {
        if !should_handle_completed_item(
            self.output.snapshot().phase(),
            self.playback_intent,
            output_matches_decoder,
        ) {
            return false;
        }
        if should_replay_current_on_completion(self.playback_mode, self.playlist.len()) {
            self.replay_current_source();
            context.request_repaint();
            return true;
        }
        if let Some(index) = self.neighbor_for_playback_mode(PlaylistStep::Next) {
            self.select_source_with_playback(index, true);
            context.request_repaint();
            return true;
        }

        self.playback_intent = false;
        self.playback_restore_pending = false;
        false
    }

    fn remove_selected_source(&mut self) {
        let Some(index) = self.selected_source else {
            return;
        };
        let removed = self.playlist.remove(index);

        if self.playlist.is_empty() {
            self.selected_source = None;
            self.status = StatusLine::idle("Add or drop AC-4 media files");
        } else {
            self.selected_source = Some(index.min(self.playlist.len() - 1));
            self.status = StatusLine::idle(format!(
                "Removed {} from the playlist",
                removed.display_name()
            ));
        }
        self.timeline_preview = 0.0;
        self.playback_intent = false;
        self.playback_restore_pending = false;
        self.automatic_reconfigure_guard = None;
        self.waiting_for_device = None;
        self.output.pause();
        self.output.reset();
        self.shuffle_history.clear();
        self.inspection
            .retain_paths(self.playlist.iter().map(SelectedSource::path));
        if self.selected_source.is_none() {
            self.bitstream_details_open = false;
        }
    }

    fn has_selected_source(&self) -> bool {
        self.selected_source
            .is_some_and(|index| index < self.playlist.len())
    }

    fn selected_source(&self) -> Option<&SelectedSource> {
        self.selected_source
            .and_then(|index| self.playlist.get(index))
    }

    fn selected_path(&self) -> Option<&Path> {
        self.selected_source().map(SelectedSource::path)
    }

    fn sync_inspection(&mut self, context: &egui::Context) {
        self.inspection.poll();
        if let Some(path) = self.selected_path().map(Path::to_path_buf) {
            self.inspection.ensure_requested(&path);
        } else {
            self.bitstream_details_open = false;
        }
        if self.inspection.has_pending() {
            context.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn sync_decoder(&mut self, context: &egui::Context) {
        if let Some(path) = self.selected_path().map(Path::to_path_buf) {
            self.decoder.ensure_open(&path);
        } else {
            self.decoder.close();
        }
        self.decoder.poll();
        if self.decoder_revision != self.decoder.revision() {
            self.decoder_revision = self.decoder.revision();
            self.status = decoder_status_line(self.decoder.snapshot());
        }
        if self.decoder.is_working() {
            context.request_repaint_after(Duration::from_millis(50));
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "output synchronization keeps decode, device, renderer, and UI revisions atomic"
    )]
    fn sync_output(&mut self, context: &egui::Context) {
        let request_id = self.decoder.request_id();
        let playback_epoch = self.decoder.playback_epoch();
        let master_gain = if self.muted { 0.0 } else { self.volume };
        self.output.set_master_gain(master_gain);
        self.output.poll();

        let output_matches_decoder = self
            .output
            .is_configured_for_playback(request_id, playback_epoch);
        if self.handle_completed_playlist_item(context, output_matches_decoder) {
            return;
        }

        let ready_scene = matches!(
            self.decoder.snapshot().phase(),
            DecodePhase::Ready | DecodePhase::EndOfStream
        )
        .then(|| self.decoder.snapshot().metrics())
        .flatten()
        .map(|metrics| {
            (
                metrics.object_count(),
                metrics.duration_frames(),
                metrics.target_frame(),
            )
        });
        let desired_device = ready_scene
            .as_ref()
            .and_then(|(objects, _, _)| self.output.resolved_device(*objects));

        if self
            .waiting_for_device
            .is_some_and(|waiting| !waiting.belongs_to(request_id, playback_epoch))
        {
            self.waiting_for_device = None;
        }
        if let Some(waiting) = self.waiting_for_device {
            if desired_device.is_none() {
                self.output.pause();
                self.output.reset();
                self.status = StatusLine::warning(
                    "No active audio endpoint can host this Scene; waiting without closing the file",
                );
                context.request_repaint_after(Duration::from_secs(2));
                return;
            }
            match self.decoder.seek(waiting.frame) {
                Ok(()) => {
                    self.waiting_for_device = None;
                    self.playback_restore_pending = true;
                    self.status = StatusLine::idle("Compatible audio endpoint restored; resuming");
                    context.request_repaint_after(Duration::from_millis(20));
                    return;
                }
                Err(error) => {
                    self.status = StatusLine::warning(format!(
                        "Compatible endpoint restored, but playback cannot resume yet: {error}"
                    ));
                    context.request_repaint_after(Duration::from_millis(50));
                    return;
                }
            }
        }
        if let Some((_, duration, decoder_target)) = ready_scene
            && desired_device.is_none()
        {
            let frame = if self
                .output
                .is_configured_for_playback(request_id, playback_epoch)
            {
                self.output.snapshot().playhead_frames()
            } else {
                decoder_target
            }
            .min(duration.unwrap_or(u64::MAX));
            self.output.pause();
            self.output.reset();
            self.waiting_for_device = Some(DeviceWait {
                request_id,
                playback_epoch,
                frame,
            });
            self.status = StatusLine::warning(
                "No active audio endpoint can host this Scene; waiting without closing the file",
            );
            context.request_repaint_after(Duration::from_secs(2));
            return;
        }

        if let Some((guard_request, guard_frame)) = self.automatic_reconfigure_guard
            && guard_request == request_id
            && self.output.snapshot().playhead_frames() > guard_frame.saturating_add(2_048)
            && !matches!(self.output.snapshot().phase(), OutputPhase::Failed)
        {
            self.automatic_reconfigure_guard = None;
        }

        if matches!(self.output.snapshot().phase(), OutputPhase::Failed)
            && let Some(error) = self.output.snapshot().error()
            && is_reconfigurable_scene_error(error)
        {
            let target = self.output.snapshot().playhead_frames();
            if self.automatic_reconfigure_guard != Some((request_id, target)) {
                self.automatic_reconfigure_guard = Some((request_id, target));
                self.output.reset();
                match self.decoder.seek(target) {
                    Ok(()) => {
                        self.playback_restore_pending = true;
                        self.status = StatusLine::idle(
                            "Adapting Windows Spatial Audio to a Scene topology change",
                        );
                        context.request_repaint_after(Duration::from_millis(20));
                        return;
                    }
                    Err(seek_error) => self.status = StatusLine::warning(seek_error),
                }
            }
        }

        if matches!(
            self.decoder.snapshot().phase(),
            DecodePhase::Ready | DecodePhase::EndOfStream
        ) && self
            .output
            .is_configured_for_playback(request_id, playback_epoch)
            && let Some(metrics) = self.decoder.snapshot().metrics()
        {
            let desired_device = self.output.resolved_device(metrics.object_count());
            if desired_device.as_ref() != self.output.configured_device()
                && desired_device.is_some()
            {
                let target = self
                    .output
                    .snapshot()
                    .playhead_frames()
                    .min(metrics.duration_frames().unwrap_or(u64::MAX));
                self.output.pause();
                match self.decoder.seek(target) {
                    Ok(()) => {
                        self.playback_restore_pending = true;
                        self.status = StatusLine::idle("Switching Windows audio endpoint");
                        context.request_repaint_after(Duration::from_millis(20));
                        return;
                    }
                    Err(error) => self.status = StatusLine::warning(error),
                }
            }
        }

        let configured_for_playback = self
            .output
            .is_configured_for_playback(request_id, playback_epoch);
        match output_sync_action(self.decoder.snapshot().phase(), configured_for_playback) {
            OutputSyncAction::Configure => {
                let config = self
                    .decoder
                    .snapshot()
                    .metrics()
                    .ok_or_else(|| "Ready decoder state has no Scene metrics".to_owned())
                    .and_then(|metrics| {
                        let scene_signature =
                            metrics.scene_signature().cloned().ok_or_else(|| {
                                "Ready decoder state has no renderable Scene signature".to_owned()
                            })?;
                        let output_device = self
                            .output
                            .resolved_device(metrics.object_count())
                            .ok_or_else(|| {
                                "No active audio endpoint can host this Scene".to_owned()
                            })?;
                        OutputStreamConfig::new(
                            request_id,
                            playback_epoch,
                            metrics.target_frame(),
                            metrics.sample_rate(),
                            scene_signature,
                            output_device,
                        )
                    });
                match config {
                    Ok(config) => {
                        self.output
                            .ensure_configured(&config, self.decoder.scene_reader());
                        if self
                            .output
                            .is_configured_for_playback(request_id, playback_epoch)
                            && let Some(resume) = take_playback_restore(
                                &mut self.playback_restore_pending,
                                self.playback_intent,
                            )
                        {
                            if resume {
                                self.output.play();
                            } else {
                                self.output.pause();
                            }
                        }
                    }
                    Err(error) => {
                        self.output.reset();
                        self.status = StatusLine::warning(error);
                    }
                }
            }
            OutputSyncAction::Preserve => {}
            OutputSyncAction::Reset => self.output.reset(),
        }

        if self.output_revision != self.output.revision() {
            self.output_revision = self.output.revision();
            self.status = output_status_line(self.output.snapshot(), self.decoder.snapshot());
        }
        if !self.timeline_dragging
            && !matches!(self.decoder.snapshot().phase(), DecodePhase::Seeking)
            && self
                .output
                .is_configured_for_playback(request_id, playback_epoch)
            && let Some(metrics) = self.decoder.snapshot().metrics()
            && let Some(duration) = metrics.duration_frames()
            && duration > 0
        {
            const TIMELINE_STEPS: u64 = 10_000;
            let submitted = self.output.snapshot().playhead_frames().min(duration);
            let scaled = submitted.saturating_mul(TIMELINE_STEPS) / duration;
            let scaled = u16::try_from(scaled).unwrap_or(u16::MAX);
            self.timeline_preview = f32::from(scaled) / 10_000.0;
        }
        if matches!(
            self.output.snapshot().phase(),
            OutputPhase::Initializing | OutputPhase::Playing
        ) {
            context.request_repaint_after(Duration::from_millis(50));
        }
        #[cfg(target_os = "windows")]
        context.request_repaint_after(Duration::from_secs(2));
    }

    fn handle_bitstream_action(&mut self, action: Option<BitstreamAction>) {
        match action {
            Some(BitstreamAction::OpenDetails) => self.bitstream_details_open = true,
            Some(BitstreamAction::Retry) => {
                if let Some(path) = self.selected_path().map(Path::to_path_buf) {
                    self.inspection.retry(&path);
                }
            }
            None => {}
        }
    }

    fn accept_dropped_files(&mut self, context: &egui::Context) {
        let paths = context.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| {
                    let path = file.path();
                    (!path.as_os_str().is_empty()).then(|| path.to_path_buf())
                })
                .collect::<Vec<_>>()
        });
        if !paths.is_empty() {
            self.append_sources(paths);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the endpoint picker and header share one tightly coupled egui panel"
    )]
    fn draw_header(&mut self, root: &mut egui::Ui) {
        let output = self.output.snapshot();
        let devices = self.output.devices().to_vec();
        let preferred = self.output.preferred_device().clone();
        let preferred_label = match &preferred {
            OutputDeviceSelection::SystemDefault => match output.phase() {
                OutputPhase::Unavailable => "Spatial output unavailable".to_owned(),
                OutputPhase::Idle => "System default".to_owned(),
                _ => format!("System default · {}", output.device_label()),
            },
            OutputDeviceSelection::EndpointId(id) => {
                devices.iter().find(|device| device.id() == id).map_or_else(
                    || format!("Preferred unavailable · {}", output.device_label()),
                    |device| device.label().to_owned(),
                )
            }
        };
        let required_objects = self
            .decoder
            .snapshot()
            .metrics()
            .and_then(|metrics| u32::try_from(metrics.object_count()).ok())
            .unwrap_or(0);
        let default_device = devices.iter().find(|device| device.is_default());
        let default_eligible = !self.output.device_catalog_ready()
            || default_device.is_some_and(|device| {
                device
                    .max_dynamic_objects()
                    .is_some_and(|available| available >= required_objects)
            });
        let device_detail = if output.max_dynamic_objects() > 0 {
            format!(
                "{} dynamic-object slots available",
                output.max_dynamic_objects()
            )
        } else {
            output
                .error()
                .or_else(|| self.output.device_catalog_error())
                .unwrap_or("Select a decoded AC-4 scene to activate")
                .to_owned()
        };
        let mut selected = preferred.clone();
        egui::Panel::top("header")
            .exact_size(72.0)
            .frame(
                egui::Frame::NONE
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(20, 0)),
            )
            .show(root, |ui| {
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("MacinDecode AC-4 Player")
                                .size(21.0)
                                .strong()
                                .color(theme::TEXT),
                        );
                        ui.label(
                            RichText::new("Native spatial playback shell")
                                .size(12.0)
                                .color(theme::MUTED),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("output-device")
                            .selected_text(preferred_label)
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                let default_response = ui
                                    .add_enabled_ui(default_eligible, |ui| {
                                        ui.selectable_value(
                                            &mut selected,
                                            OutputDeviceSelection::SystemDefault,
                                            "System default",
                                        )
                                    })
                                    .inner;
                                if !default_eligible {
                                    let reason = default_device
                                        .and_then(|device| device.spatial_error())
                                        .map_or_else(
                                            || {
                                                format!(
                                                    "System default cannot provide {required_objects} dynamic objects"
                                                )
                                            },
                                            str::to_owned,
                                        );
                                    default_response.on_hover_text(reason);
                                }
                                ui.separator();
                                for device in &devices {
                                    let capacity = device.max_dynamic_objects();
                                    let eligible = capacity
                                        .is_some_and(|available| available >= required_objects);
                                    let label = if device.is_default() {
                                        format!("{} · default", device.label())
                                    } else {
                                        device.label().to_owned()
                                    };
                                    let response = ui
                                        .add_enabled_ui(eligible, |ui| {
                                            ui.selectable_value(
                                                &mut selected,
                                                OutputDeviceSelection::EndpointId(
                                                    device.id().to_owned(),
                                                ),
                                                label,
                                            )
                                        })
                                        .inner;
                                    if !eligible {
                                        let reason = device.spatial_error().map_or_else(
                                            || {
                                                format!(
                                                    "Needs {required_objects} dynamic objects; endpoint provides {}",
                                                    capacity.unwrap_or(0)
                                                )
                                            },
                                            str::to_owned,
                                        );
                                        response.on_hover_text(reason);
                                    }
                                }
                                ui.separator();
                                ui.add_enabled(false, egui::Label::new(device_detail));
                            });
                        ui.label(
                            RichText::new("OUTPUT DEVICE")
                                .size(10.0)
                                .strong()
                                .color(theme::MUTED),
                        );
                    });
                });
                let clip = ui.clip_rect();
                let bottom = ui.max_rect().bottom() - 0.5;
                ui.painter().line_segment(
                    [
                        egui::pos2(clip.left(), bottom),
                        egui::pos2(clip.right(), bottom),
                    ],
                    Stroke::new(1.0, theme::BORDER),
                );
            });
        if selected != preferred && self.output.set_preferred_device(selected) {
            self.status = StatusLine::idle("Changing Windows audio endpoint");
        }
    }

    fn draw_source_sidebar(&mut self, root: &mut egui::Ui) {
        egui::Panel::left("source-sidebar")
            .exact_size(310.0)
            .resizable(false)
            .frame(
                egui::Frame::NONE
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::same(18)),
            )
            .show(root, |ui| {
                // Noto Sans CJK makes the complete ready-state card about 249 points tall.
                const INFO_BLOCK_HEIGHT: f32 = 256.0;
                const BLOCK_GAP: f32 = 18.0;

                let available = ui.available_rect_before_wrap();
                let info_rect = egui::Rect::from_min_max(
                    egui::pos2(available.left(), available.bottom() - INFO_BLOCK_HEIGHT),
                    available.right_bottom(),
                );
                let source_rect = egui::Rect::from_min_max(
                    available.min,
                    egui::pos2(available.right(), info_rect.top() - BLOCK_GAP),
                );

                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(source_rect)
                        .layout(Layout::top_down(Align::Min)),
                    |ui| {
                        section_title(ui, "SOURCE");
                        let playlist_height = (ui.available_height() - 124.0).max(72.0);
                        card(ui, |ui| self.draw_source_card(ui, playlist_height));
                    },
                );
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(info_rect)
                        .layout(Layout::top_down(Align::Min)),
                    |ui| self.draw_bitstream_info(ui),
                );
            });
    }

    fn draw_bitstream_info(&mut self, ui: &mut egui::Ui) {
        section_title(ui, "BITSTREAM INFO");
        let source = self.selected_source();
        let state = source.and_then(|source| self.inspection.state(source.path()));
        let action = card(ui, |ui| bitstream_ui::draw_card(ui, source, state));
        self.handle_bitstream_action(action);
    }

    fn draw_source_card(&mut self, ui: &mut egui::Ui, playlist_height: f32) {
        playlist_header(ui, self.playlist.len());
        ui.separator();

        let content_width = ui.available_width();
        let action_height = 36.0;
        let action_gap = 11.0;
        let (body_rect, _) = ui.allocate_exact_size(
            egui::vec2(content_width, playlist_height + action_gap + action_height),
            egui::Sense::hover(),
        );
        let actions_rect = egui::Rect::from_min_max(
            egui::pos2(body_rect.left(), body_rect.bottom() - action_height),
            body_rect.right_bottom(),
        );
        let list_rect = egui::Rect::from_min_max(
            body_rect.min,
            egui::pos2(body_rect.right(), actions_rect.top() - action_gap),
        );
        if let Some(index) = playlist_contents(ui, list_rect, &self.playlist, self.selected_source)
        {
            self.select_source(index);
        }

        match playlist_actions(ui, actions_rect, self.has_selected_source()) {
            Some(PlaylistAction::Add) => self.choose_sources(),
            Some(PlaylistAction::Remove) => self.remove_selected_source(),
            None => {}
        }
    }

    fn draw_scene(&mut self, root: &mut egui::Ui) {
        let decoder = self.decoder.snapshot().clone();
        let output = self.output.snapshot().clone();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::same(20)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(RichText::new("Object scene").color(theme::TEXT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_sized([36.0, 30.0], egui::Button::new("..."))
                            .on_hover_text("Open diagnostics")
                            .clicked()
                        {
                            self.diagnostics_open = true;
                        }
                        ui.add_space(4.0);
                        let format = decoder.metrics().map_or_else(
                            || "48 kHz · planar f32".to_owned(),
                            |metrics| format!("{} Hz · planar f32", metrics.sample_rate()),
                        );
                        ui.label(RichText::new(format).size(11.0).color(theme::MUTED));
                    });
                });
                ui.add_space(10.0);

                metric_strip(ui, &decoder);

                ui.add_space(16.0);
                scene_placeholder(ui, self.has_selected_source(), &decoder, &output);
            });
    }

    fn draw_diagnostics_window(&mut self, context: &egui::Context) {
        if !self.diagnostics_open {
            return;
        }

        let decoder = self.decoder.snapshot().clone();
        let output = self.output.snapshot().clone();
        let remains_open = context.show_viewport_immediate(
            egui::ViewportId::from_hash_of("playback-diagnostics"),
            egui::ViewportBuilder::default()
                .with_title("MacinDecode AC-4 Diagnostics")
                .with_inner_size([460.0, 390.0])
                .with_min_inner_size([400.0, 300.0]),
            |root, _class| {
                let close_requested = root.ctx().input(|input| input.viewport().close_requested());
                draw_diagnostics_content(root, self.backend, &decoder, &output);
                !close_requested
            },
        );
        self.diagnostics_open = remains_open;
    }

    fn draw_bitstream_details_window(&mut self, context: &egui::Context) {
        if !self.bitstream_details_open {
            return;
        }
        let Some(source) = self.selected_source() else {
            self.bitstream_details_open = false;
            return;
        };
        let state = self.inspection.state(source.path());
        let mut requested_action = None;
        let remains_open = context.show_viewport_immediate(
            egui::ViewportId::from_hash_of("bitstream-details"),
            egui::ViewportBuilder::default()
                .with_title("MacinDecode AC-4 Bitstream Details")
                .with_inner_size([760.0, 680.0])
                .with_min_inner_size([560.0, 420.0]),
            |root, _class| {
                let close_requested = root.ctx().input(|input| input.viewport().close_requested());
                requested_action = bitstream_ui::draw_details(root, source, state);
                !close_requested
            },
        );
        self.bitstream_details_open = remains_open;
        self.handle_bitstream_action(requested_action);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "transport layout and its immediate seek actions are intentionally colocated"
    )]
    fn draw_transport(&mut self, root: &mut egui::Ui) {
        let output_phase = self.output.snapshot().phase();
        let can_toggle = self.output.snapshot().can_play()
            || (output_phase == OutputPhase::Ended && self.has_selected_source());
        let playing = self.output.snapshot().is_playing();
        let can_previous = self.can_select_neighbor(PlaylistStep::Previous);
        let can_next = self.can_select_neighbor(PlaylistStep::Next);
        let mut selected_mode = self.playback_mode;
        let (duration_frames, sample_rate, seekable) =
            self.decoder
                .snapshot()
                .metrics()
                .map_or((None, 0, false), |metrics| {
                    (
                        metrics.duration_frames(),
                        metrics.sample_rate(),
                        metrics.seekable_from_frame().is_some(),
                    )
                });
        let (action, timeline) = egui::Panel::bottom("transport")
            .exact_size(136.0)
            .frame(
                egui::Frame::NONE
                    .fill(theme::SURFACE)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .inner_margin(egui::Margin::symmetric(22, 14)),
            )
            .show(root, |ui| {
                let mut action = None;
                let mut timeline = TimelineInteraction::default();
                let (content, _) =
                    ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
                let status_rect =
                    egui::Rect::from_min_size(content.min, egui::vec2(content.width(), 20.0));
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(status_rect)
                        .layout(Layout::left_to_right(Align::Center)),
                    |ui| {
                        ui.colored_label(self.status.color(), "●");
                        ui.label(RichText::new(&self.status.text).color(theme::MUTED));
                    },
                );

                let control_height = 34.0 + ui.spacing().item_spacing.y + 20.0;
                let control_rect = egui::Rect::from_center_size(
                    content.center(),
                    egui::vec2(content.width(), control_height),
                );
                let volume_width = if content.width() >= 700.0 {
                    150.0
                } else {
                    120.0
                };
                let side_reserve = volume_width + 20.0;
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(control_rect)
                        .layout(Layout::top_down(Align::Center)),
                    |ui| {
                        action =
                            transport_buttons(ui, [can_previous, can_toggle, can_next], playing);
                        timeline = transport_progress(
                            ui,
                            &mut self.timeline_preview,
                            side_reserve,
                            duration_frames,
                            sample_rate,
                            seekable,
                        );
                    },
                );

                let volume_rect = egui::Rect::from_center_size(
                    egui::pos2(
                        content.right() - volume_width / 2.0,
                        control_rect.bottom() - 10.0,
                    ),
                    egui::vec2(volume_width, 28.0),
                );
                volume_control(ui, volume_rect, &mut self.volume, &mut self.muted);

                let mode_rect = egui::Rect::from_center_size(
                    egui::pos2(
                        content.left() + volume_width / 2.0,
                        control_rect.bottom() - 10.0,
                    ),
                    egui::vec2(volume_width, 28.0),
                );
                playback_mode_control(ui, mode_rect, &mut selected_mode);
                (action, timeline)
            })
            .inner;

        if self.playback_mode != selected_mode {
            self.playback_mode = selected_mode;
            self.shuffle_history.clear();
            self.status = StatusLine::idle(format!(
                "Playback mode: {}",
                self.playback_mode.description()
            ));
        }

        self.timeline_dragging = timeline.dragging;
        if let Some(target_frame) = timeline.seek_target {
            self.output.pause();
            match self.decoder.seek(target_frame) {
                Ok(()) => {
                    self.playback_restore_pending = true;
                    self.status = StatusLine::idle(format!(
                        "Seeking to {}",
                        format_timestamp(target_frame, sample_rate)
                    ));
                }
                Err(error) => {
                    self.playback_restore_pending = false;
                    if self.playback_intent {
                        self.output.play();
                    }
                    self.status = StatusLine::warning(error);
                }
            }
        }

        match action {
            Some(TransportAction::Previous) => {
                self.select_neighbor(PlaylistStep::Previous);
            }
            Some(TransportAction::Toggle) if output_phase == OutputPhase::Ended => {
                self.replay_current_source();
            }
            Some(TransportAction::Toggle) if playing => {
                self.playback_intent = false;
                self.output.pause();
            }
            Some(TransportAction::Toggle) => {
                self.playback_intent = true;
                self.output.play();
            }
            Some(TransportAction::Next) => {
                self.select_neighbor(PlaylistStep::Next);
            }
            None => {}
        }
    }
}

enum PlaylistAction {
    Add,
    Remove,
}

fn playlist_header(ui: &mut egui::Ui, item_count: usize) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("PLAYLIST")
                .size(10.0)
                .strong()
                .color(theme::MUTED),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let noun = if item_count == 1 { "ITEM" } else { "ITEMS" };
            ui.label(
                RichText::new(format!("{item_count} {noun}"))
                    .size(10.0)
                    .color(theme::MUTED),
            );
        });
    });
}

fn playlist_contents(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    playlist: &[SelectedSource],
    selected_source: Option<usize>,
) -> Option<usize> {
    let mut requested_selection = None;
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::top_down(Align::Min)),
        |ui| {
            if playlist.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(((rect.height() - 48.0) / 2.0).max(16.0));
                    ui.label(RichText::new("Playlist is empty").color(theme::TEXT));
                    ui.label(
                        RichText::new("Drop .m4a, .mp4, or .ac4 files here")
                            .size(11.0)
                            .color(theme::MUTED),
                    );
                });
                return;
            }

            egui::ScrollArea::vertical()
                .id_salt("source-playlist")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (index, source) in playlist.iter().enumerate() {
                        let label = format!("{:02}  {}", index + 1, source.display_name());
                        let selected = selected_source == Some(index);
                        if ui
                            .add_sized(
                                [ui.available_width(), 34.0],
                                egui::Button::selectable(selected, RichText::new(label).size(12.0))
                                    .truncate(),
                            )
                            .on_hover_text(source.path().display().to_string())
                            .clicked()
                        {
                            requested_selection = Some(index);
                        }
                    }
                });
        },
    );
    requested_selection
}

fn playlist_actions(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    can_remove: bool,
) -> Option<PlaylistAction> {
    let remove_width = 76.0;
    let add_width = (rect.width() - remove_width - ui.spacing().item_spacing.x).max(100.0);
    let mut action = None;
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            if ui
                .add_sized(
                    [add_width, rect.height()],
                    egui::Button::new(RichText::new("Add files").strong().color(Color32::WHITE))
                        .fill(theme::ACCENT)
                        .stroke(Stroke::NONE),
                )
                .clicked()
            {
                action = Some(PlaylistAction::Add);
            }
            if ui
                .add_enabled(
                    can_remove,
                    egui::Button::new("Remove").min_size(egui::vec2(remove_width, rect.height())),
                )
                .clicked()
            {
                action = Some(PlaylistAction::Remove);
            }
        },
    );
    action
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportAction {
    Previous,
    Toggle,
    Next,
}

fn transport_buttons(
    ui: &mut egui::Ui,
    enabled: [bool; 3],
    playing: bool,
) -> Option<TransportAction> {
    let [can_previous, can_toggle, can_next] = enabled;
    let button_size = egui::vec2(44.0, 34.0);
    let group_width = button_size.x * 3.0 + ui.spacing().item_spacing.x * 2.0;
    let (row, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), button_size.y),
        egui::Sense::hover(),
    );
    let group = egui::Rect::from_center_size(row.center(), egui::vec2(group_width, button_size.y));
    let mut action = None;
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(group)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            if ui
                .add_enabled(
                    can_previous,
                    egui::Button::new("◀◀")
                        .min_size(button_size)
                        .fill(theme::HOVER),
                )
                .on_hover_text("Previous playlist item")
                .clicked()
            {
                action = Some(TransportAction::Previous);
            }
            let toggle_response = ui
                .add_enabled(
                    can_toggle,
                    egui::Button::new(if playing { "" } else { "▶" })
                        .min_size(button_size)
                        .fill(theme::HOVER),
                )
                .on_hover_text(if playing { "Pause" } else { "Play" });
            if playing {
                paint_pause_icon(ui, toggle_response.rect, can_toggle);
            }
            if toggle_response.clicked() {
                action = Some(TransportAction::Toggle);
            }
            if ui
                .add_enabled(
                    can_next,
                    egui::Button::new("▶▶")
                        .min_size(button_size)
                        .fill(theme::HOVER),
                )
                .on_hover_text("Next playlist item")
                .clicked()
            {
                action = Some(TransportAction::Next);
            }
        },
    );
    action
}

fn playback_mode_control(ui: &mut egui::Ui, rect: egui::Rect, mode: &mut PlaybackMode) {
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().interact_size.y = rect.height();
            egui::ComboBox::from_id_salt("playback-mode")
                .selected_text(mode.label())
                .width(rect.width())
                .show_ui(ui, |ui| {
                    for option in PlaybackMode::ALL {
                        ui.selectable_value(mode, option, option.label())
                            .on_hover_text(option.description());
                    }
                });
        },
    );
}

fn paint_pause_icon(ui: &egui::Ui, button_rect: egui::Rect, enabled: bool) {
    let color = if enabled { theme::TEXT } else { theme::MUTED };
    let center = button_rect.center();
    let bar_size = egui::vec2(3.0, 14.0);
    for offset in [-3.5, 3.5] {
        let bar_center = egui::pos2(center.x + offset, center.y);
        ui.painter().rect_filled(
            egui::Rect::from_center_size(bar_center, bar_size),
            0.0,
            color,
        );
    }
}

#[derive(Default)]
struct TimelineInteraction {
    seek_target: Option<u64>,
    dragging: bool,
}

fn transport_progress(
    ui: &mut egui::Ui,
    timeline_preview: &mut f32,
    side_reserve: f32,
    duration_frames: Option<u64>,
    sample_rate: u32,
    seekable: bool,
) -> TimelineInteraction {
    let row_width = ui.available_width();
    let time_width = 50.0;
    let spacing = ui.spacing().item_spacing.x;
    let max_group_width = (row_width - side_reserve * 2.0).max(260.0);
    let max_progress_width = (max_group_width - time_width * 2.0 - spacing * 2.0).max(140.0);
    let progress_width = (row_width * 0.45)
        .clamp(160.0, 420.0)
        .min(max_progress_width);
    let group_width = progress_width + time_width * 2.0 + ui.spacing().item_spacing.x * 2.0;
    let (row, _) = ui.allocate_exact_size(egui::vec2(row_width, 20.0), egui::Sense::hover());
    let group = egui::Rect::from_center_size(row.center(), egui::vec2(group_width, row.height()));

    let mut interaction = TimelineInteraction::default();
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(group)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            let duration = duration_frames.unwrap_or(0);
            let preview_frame = normalized_frame(*timeline_preview, duration);
            ui.add_sized(
                [time_width, 18.0],
                egui::Label::new(
                    RichText::new(format_timestamp(preview_frame, sample_rate))
                        .monospace()
                        .color(theme::MUTED),
                )
                .halign(Align::RIGHT),
            );
            ui.add_enabled_ui(seekable && duration > 0, |ui| {
                ui.spacing_mut().interact_size.y = 18.0;
                ui.spacing_mut().slider_width = progress_width;
                let response =
                    ui.add(egui::Slider::new(timeline_preview, 0.0..=1.0).show_value(false));
                interaction.dragging = response.dragged() || response.is_pointer_button_down_on();
                if response.drag_stopped() || response.clicked() {
                    interaction.seek_target = Some(normalized_frame(*timeline_preview, duration));
                }
            });
            ui.add_sized(
                [time_width, 18.0],
                egui::Label::new(
                    RichText::new(if duration > 0 {
                        format_timestamp(duration, sample_rate)
                    } else {
                        "--:--".to_owned()
                    })
                    .monospace()
                    .color(theme::MUTED),
                )
                .halign(Align::LEFT),
            );
        },
    );
    interaction
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "the normalized UI slider is clamped to a finite 0..=1 range"
)]
fn normalized_frame(value: f32, duration_frames: u64) -> u64 {
    if !value.is_finite() || duration_frames == 0 {
        return 0;
    }
    (f64::from(value.clamp(0.0, 1.0)) * duration_frames as f64).round() as u64
}

fn format_timestamp(frames: u64, sample_rate: u32) -> String {
    if sample_rate == 0 {
        return "--:--".to_owned();
    }
    let total_seconds = frames / u64::from(sample_rate);
    let seconds = total_seconds % 60;
    let minutes = (total_seconds / 60) % 60;
    let hours = total_seconds / 3_600;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

fn volume_control(ui: &mut egui::Ui, rect: egui::Rect, volume: &mut f32, muted: &mut bool) {
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            if speaker_button(ui, *muted, *volume).clicked() {
                if *muted {
                    if *volume <= f32::EPSILON {
                        *volume = 0.5;
                    }
                    *muted = false;
                } else {
                    *muted = true;
                }
            }

            let slider_width = (rect.width() - 28.0 - ui.spacing().item_spacing.x).max(56.0);
            ui.spacing_mut().interact_size.y = 18.0;
            ui.spacing_mut().slider_width = slider_width;
            let response = ui.add(egui::Slider::new(volume, 0.0..=1.0).show_value(false));
            if response.changed() {
                *muted = *volume <= f32::EPSILON;
            }
            response.on_hover_text(format!("Volume: {:.0}%", *volume * 100.0));
        },
    );
}

fn speaker_button(ui: &mut egui::Ui, muted: bool, volume: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
    let response = response.on_hover_text(if muted { "Unmute" } else { "Mute" });
    let fill = if response.hovered() {
        theme::HOVER
    } else {
        theme::SURFACE
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    ui.painter().rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, theme::BORDER),
        egui::StrokeKind::Inside,
    );

    let center = rect.center();
    let color = if muted { theme::WARNING } else { theme::MUTED };
    ui.painter().rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(center.x - 9.0, center.y - 3.0),
            egui::pos2(center.x - 5.0, center.y + 3.0),
        ),
        0.0,
        color,
    );
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(center.x - 5.0, center.y - 4.0),
            egui::pos2(center.x + 1.0, center.y - 8.0),
            egui::pos2(center.x + 1.0, center.y + 8.0),
            egui::pos2(center.x - 5.0, center.y + 4.0),
        ],
        color,
        Stroke::NONE,
    ));

    if muted {
        ui.painter().line_segment(
            [
                egui::pos2(center.x + 5.0, center.y - 4.0),
                egui::pos2(center.x + 11.0, center.y + 4.0),
            ],
            Stroke::new(1.5, color),
        );
        ui.painter().line_segment(
            [
                egui::pos2(center.x + 11.0, center.y - 4.0),
                egui::pos2(center.x + 5.0, center.y + 4.0),
            ],
            Stroke::new(1.5, color),
        );
    } else if volume > f32::EPSILON {
        ui.painter().line_segment(
            [
                egui::pos2(center.x + 5.0, center.y - 4.0),
                egui::pos2(center.x + 8.0, center.y),
            ],
            Stroke::new(1.5, color),
        );
        ui.painter().line_segment(
            [
                egui::pos2(center.x + 8.0, center.y),
                egui::pos2(center.x + 5.0, center.y + 4.0),
            ],
            Stroke::new(1.5, color),
        );
        if volume > 0.5 {
            ui.painter().line_segment(
                [
                    egui::pos2(center.x + 8.0, center.y - 6.0),
                    egui::pos2(center.x + 12.0, center.y),
                ],
                Stroke::new(1.5, color),
            );
            ui.painter().line_segment(
                [
                    egui::pos2(center.x + 12.0, center.y),
                    egui::pos2(center.x + 8.0, center.y + 6.0),
                ],
                Stroke::new(1.5, color),
            );
        }
    }

    response
}

impl eframe::App for PlayerApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = root.ctx().clone();
        self.accept_dropped_files(&context);
        self.sync_inspection(&context);
        self.sync_decoder(&context);
        self.sync_output(&context);
        self.draw_header(root);
        self.draw_source_sidebar(root);
        self.draw_transport(root);
        self.draw_scene(root);
        self.draw_bitstream_details_window(&context);
        self.draw_diagnostics_window(&context);

        if context.input(|input| !input.raw.hovered_files.is_empty()) {
            draw_drop_overlay(&context);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(
            storage,
            OUTPUT_DEVICE_STORAGE_KEY,
            self.output.preferred_device(),
        );
    }
}

struct StatusLine {
    kind: StatusKind,
    text: String,
}

impl StatusLine {
    fn idle(text: impl Into<String>) -> Self {
        Self {
            kind: StatusKind::Idle,
            text: text.into(),
        }
    }

    fn ready(text: impl Into<String>) -> Self {
        Self {
            kind: StatusKind::Ready,
            text: text.into(),
        }
    }

    fn warning(text: impl Into<String>) -> Self {
        Self {
            kind: StatusKind::Warning,
            text: text.into(),
        }
    }

    const fn color(&self) -> Color32 {
        match self.kind {
            StatusKind::Idle => theme::MUTED,
            StatusKind::Ready => theme::SUCCESS,
            StatusKind::Warning => theme::WARNING,
        }
    }
}

enum StatusKind {
    Idle,
    Ready,
    Warning,
}

fn is_reconfigurable_scene_error(error: &str) -> bool {
    [
        "Scene dynamic-object count changed",
        "Scene LFE layout changed",
        "Scene configuration generation changed",
        "Selected Scene presentation changed",
        "Scene dynamic-object element IDs changed",
        "Scene LFE element ID changed",
    ]
    .iter()
    .any(|prefix| error.starts_with(prefix))
}

fn decoder_status_line(decoder: &DecoderSnapshot) -> StatusLine {
    let source = decoder
        .path()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("AC-4 source");
    match decoder.phase() {
        DecodePhase::Unavailable => StatusLine::idle(
            decoder
                .detail()
                .unwrap_or("The Windows decode worker is unavailable"),
        ),
        DecodePhase::Idle => StatusLine::idle("Add or select an AC-4 media file"),
        DecodePhase::Opening => StatusLine::idle(format!("Opening {source} with MacinDecode Core")),
        DecodePhase::Seeking => {
            let target = decoder.metrics().map_or(0, DecodeMetrics::target_frame);
            StatusLine::idle(format!("Seeking {source} to frame {target}"))
        }
        DecodePhase::Buffering => {
            let buffered = decoder
                .metrics()
                .map_or(0, DecodeMetrics::buffered_milliseconds);
            StatusLine::idle(format!(
                "MacinDecode Core buffered {buffered} / {PREBUFFER_MILLISECONDS} ms"
            ))
        }
        DecodePhase::Ready => {
            let metrics = decoder.metrics().expect("ready decode state has metrics");
            StatusLine::ready(format!(
                "MacinDecode Core ready: {} objects + {} LFE, {} ms buffered{}",
                metrics.object_count(),
                u8::from(metrics.has_lfe()),
                metrics.buffered_milliseconds(),
                seek_index_suffix(decoder)
            ))
        }
        DecodePhase::EndOfStream => {
            let metrics = decoder
                .metrics()
                .expect("end-of-stream decode state has metrics");
            StatusLine::ready(format!(
                "Decoded {source} to end: {} AUs, {} ms of scene PCM",
                metrics.decoded_access_units(),
                metrics.decoded_milliseconds()
            ))
        }
        DecodePhase::Failed => StatusLine::warning(format!(
            "MacinDecode Core failed for {source}: {}",
            decoder.detail().unwrap_or("unknown decode error")
        )),
    }
}

fn output_status_line(output: &OutputSnapshot, decoder: &DecoderSnapshot) -> StatusLine {
    match output.phase() {
        OutputPhase::Unavailable | OutputPhase::Idle => decoder_status_line(decoder),
        OutputPhase::Initializing => StatusLine::idle(format!(
            "Opening Windows Spatial Audio for {} dynamic objects",
            output.reserved_dynamic_objects()
        )),
        OutputPhase::Ready => StatusLine::ready(format!(
            "Windows Spatial Audio ready: {} of {} dynamic slots reserved{}",
            output.reserved_dynamic_objects(),
            output.max_dynamic_objects(),
            seek_index_suffix(decoder)
        )),
        OutputPhase::Playing => StatusLine::ready(format!(
            "Spatial playback at frame {}: {} object buffers, {} underruns{}",
            output.playhead_frames(),
            output.object_buffer_submissions(),
            output.underruns(),
            seek_index_suffix(decoder)
        )),
        OutputPhase::Paused => StatusLine::idle(format!(
            "Spatial playback paused at {} frames",
            output.playhead_frames()
        )),
        OutputPhase::Ended => StatusLine::ready(format!(
            "Spatial playback ended at frame {}",
            output.playhead_frames()
        )),
        OutputPhase::Failed => StatusLine::warning(format!(
            "Windows Spatial Audio failed: {}",
            output.error().unwrap_or("unknown native output error")
        )),
    }
}

fn seek_index_suffix(decoder: &DecoderSnapshot) -> String {
    let Some(metrics) = decoder.metrics() else {
        return String::new();
    };
    if metrics.is_indexing() {
        " · indexing seek map".to_owned()
    } else {
        metrics
            .index_error()
            .map_or_else(String::new, |error| format!(" · seek unavailable: {error}"))
    }
}

const fn decode_phase_label(phase: DecodePhase) -> &'static str {
    match phase {
        DecodePhase::Unavailable => "Unavailable",
        DecodePhase::Idle => "Idle",
        DecodePhase::Opening => "Opening",
        DecodePhase::Seeking => "Seeking",
        DecodePhase::Buffering => "Buffering",
        DecodePhase::Ready => "Ready",
        DecodePhase::EndOfStream => "End of stream",
        DecodePhase::Failed => "Failed",
    }
}

const fn output_phase_label(phase: OutputPhase) -> &'static str {
    match phase {
        OutputPhase::Unavailable => "Unavailable",
        OutputPhase::Idle => "Idle",
        OutputPhase::Initializing => "Initializing",
        OutputPhase::Ready => "Ready",
        OutputPhase::Playing => "Playing",
        OutputPhase::Paused => "Paused",
        OutputPhase::Ended => "End of stream",
        OutputPhase::Failed => "Failed",
    }
}

fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).size(11.0).strong().color(theme::MUTED));
    ui.add_space(4.0);
}

fn card<R>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::NONE
        .fill(theme::SURFACE)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .inner_margin(egui::Margin::same(14))
        .show(ui, contents)
        .inner
}

fn key_value(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).size(12.0).color(theme::MUTED));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).size(12.0).color(theme::TEXT));
        });
    });
}

fn decode_metric_values(decoder: &DecoderSnapshot) -> [(&'static str, String, String); 4] {
    let metrics = decoder.metrics();
    let phase = decode_phase_label(decoder.phase());
    [
        (
            "OBJECTS",
            metrics.map_or_else(|| "—".to_owned(), |value| value.object_count().to_string()),
            metrics.map_or_else(
                || phase.to_owned(),
                |value| format!("presentation {}", value.presentation_index()),
            ),
        ),
        (
            "LFE",
            metrics.map_or_else(
                || "—".to_owned(),
                |value| if value.has_lfe() { "1" } else { "0" }.to_owned(),
            ),
            "Native bed component".to_owned(),
        ),
        (
            "POSITION",
            metrics.map_or_else(
                || "—".to_owned(),
                |value| {
                    if value.state_complete() {
                        "READY"
                    } else {
                        "WAIT"
                    }
                    .to_owned()
                },
            ),
            metrics.map_or_else(
                || "OAMD pending".to_owned(),
                |value| format!("{} in-frame updates", value.metadata_updates()),
            ),
        ),
        (
            "BUFFER",
            metrics.map_or_else(
                || "—".to_owned(),
                |value| format!("{} ms", value.buffered_milliseconds()),
            ),
            metrics.map_or_else(
                || "Scene FIFO offline".to_owned(),
                |value| {
                    format!(
                        "{} / {} frames",
                        value.buffered_frames(),
                        value.buffer_capacity_frames()
                    )
                },
            ),
        ),
    ]
}

fn metric_strip(ui: &mut egui::Ui, decoder: &DecoderSnapshot) {
    let values = decode_metric_values(decoder);

    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 96.0), egui::Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, theme::SURFACE);
    let cell_width = rect.width() / 4.0;
    let first_cell = egui::Rect::from_min_size(rect.min, egui::vec2(cell_width, rect.height()));
    painter.rect_filled(first_cell, 0.0, theme::STAGE);
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, theme::BORDER),
        egui::StrokeKind::Inside,
    );

    let mut left = rect.left();
    let value_count = values.len();
    for (index, (title, value, detail)) in values.into_iter().enumerate() {
        let right = if index + 1 == value_count {
            rect.right()
        } else {
            left + cell_width
        };
        if index > 0 {
            painter.line_segment(
                [
                    egui::pos2(left, rect.top()),
                    egui::pos2(left, rect.bottom()),
                ],
                Stroke::new(1.0, theme::BORDER),
            );
        }
        let cell = egui::Rect::from_min_max(
            egui::pos2(left + 16.0, rect.top() + 12.0),
            egui::pos2(right - 12.0, rect.bottom() - 10.0),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(cell)
                .layout(Layout::top_down(Align::Min)),
            |ui| {
                ui.label(RichText::new(title).size(10.0).strong().color(theme::MUTED));
                ui.label(RichText::new(&value).size(21.0).strong().color(theme::TEXT));
                ui.label(RichText::new(&detail).size(10.0).color(theme::MUTED));
            },
        );
        left = right;
    }
}

fn scene_placeholder(
    ui: &mut egui::Ui,
    has_source: bool,
    decoder: &DecoderSnapshot,
    output: &OutputSnapshot,
) {
    let available_height = ui.available_height();
    let frame = egui::Frame::NONE
        .fill(theme::STAGE)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .inner_margin(egui::Margin::same(18));
    let margins = frame.total_margin();
    let content_height = (available_height - margins.top - margins.bottom).max(180.0);

    frame.show(ui, |ui| {
        ui.set_min_height(content_height);
        ui.vertical_centered(|ui| {
            ui.add_space(((content_height - 62.0) / 2.0).max(24.0));
            let (headline, detail) = match decoder.phase() {
                DecodePhase::Opening => (
                    "Opening AC-4 source",
                    "Reading the bounded access-unit timeline for MacinDecode Core.",
                ),
                DecodePhase::Seeking => (
                    "Seeking object scene",
                    "Core is rebuilding Scene state from the nearest complete random-access point.",
                ),
                DecodePhase::Buffering => (
                    "Decoding object scene",
                    "Core is producing normalized object/LFE PCM into the bounded Scene FIFO.",
                ),
                DecodePhase::Ready | DecodePhase::EndOfStream => match output.phase() {
                    OutputPhase::Initializing => (
                        "Opening spatial output",
                        "Activating the default endpoint and reserving Windows dynamic objects.",
                    ),
                    OutputPhase::Playing => (
                        "Spatial playback active",
                        "Scene PCM, object positions, gains, and LFE are being submitted to Windows.",
                    ),
                    OutputPhase::Paused => (
                        "Spatial playback paused",
                        "The native stream remains active and submits silence until playback resumes.",
                    ),
                    OutputPhase::Ended => (
                        "Spatial playback ended",
                        "The Scene FIFO reached its end and all native objects were finalized.",
                    ),
                    OutputPhase::Failed => (
                        "Spatial output failed",
                        output.error().unwrap_or("Windows Spatial Audio reported an error."),
                    ),
                    OutputPhase::Ready => (
                        "Spatial scene ready",
                        "Windows Spatial Audio is active; press Play to consume the Scene FIFO.",
                    ),
                    OutputPhase::Unavailable | OutputPhase::Idle => (
                        "Decoded scene buffered",
                        "Full A-JOC PCM and OAMD are ready for native spatial submission.",
                    ),
                },
                DecodePhase::Failed => (
                    "Scene decode failed",
                    decoder.detail().unwrap_or("MacinDecode Core reported an error."),
                ),
                DecodePhase::Unavailable => (
                    "Windows decoder path",
                    decoder
                        .detail()
                        .unwrap_or("Audio decoding is unavailable on this platform."),
                ),
                DecodePhase::Idle if has_source => (
                    "Waiting for decoder",
                    "The selected source has not entered the decode worker yet.",
                ),
                DecodePhase::Idle => ("No object scene", "Add an AC-4 source to begin inspection."),
            };
            ui.label(
                RichText::new(headline)
                    .size(19.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(detail).color(theme::MUTED),
            );
        });
    });
}

#[allow(
    clippy::too_many_lines,
    reason = "the diagnostics viewport deliberately keeps its ordered key/value layout together"
)]
fn draw_diagnostics_content(
    root: &mut egui::Ui,
    backend: SpatialBackendKind,
    decoder: &DecoderSnapshot,
    output: &OutputSnapshot,
) {
    egui::CentralPanel::default()
        .frame(
            egui::Frame::NONE
                .fill(theme::BACKGROUND)
                .inner_margin(egui::Margin::same(22)),
        )
        .show(root, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.heading(RichText::new("Playback diagnostics").color(theme::TEXT));
                    ui.label(
                        RichText::new(
                            "Live status for the decoder and native spatial output path.",
                        )
                        .size(11.0)
                        .color(theme::MUTED),
                    );
                    ui.add_space(16.0);
                    section_title(ui, "SESSION");
                    card(ui, |ui| {
                        let metrics = decoder.metrics();
                        key_value(
                            ui,
                            "Container",
                            metrics.map_or("Not connected", |value| value.container().label()),
                        );
                        ui.separator();
                        key_value(ui, "Decoder session", decode_phase_label(decoder.phase()));
                        ui.separator();
                        key_value(
                            ui,
                            "Scene elements",
                            &metrics.map_or_else(
                                || "—".to_owned(),
                                |value| {
                                    format!(
                                        "{} objects + {} LFE",
                                        value.object_count(),
                                        u8::from(value.has_lfe())
                                    )
                                },
                            ),
                        );
                        ui.separator();
                        key_value(
                            ui,
                            "Decoded AUs / frames",
                            &metrics.map_or_else(
                                || "0 / 0".to_owned(),
                                |value| {
                                    format!(
                                        "{} / {}",
                                        value.decoded_access_units(),
                                        value.decoded_scene_frames()
                                    )
                                },
                            ),
                        );
                        ui.separator();
                        key_value(
                            ui,
                            "Scene buffer",
                            &metrics.map_or_else(
                                || "0 ms".to_owned(),
                                |value| format!("{} ms", value.buffered_milliseconds()),
                            ),
                        );
                        ui.separator();
                        key_value(ui, "Backend policy", backend.label());
                        ui.separator();
                        key_value(
                            ui,
                            "Native adapters",
                            &format!("1 active / {} declared", SpatialBackendKind::ALL.len() - 1),
                        );
                        ui.separator();
                        key_value(ui, "Spatial stream", output_phase_label(output.phase()));
                        ui.separator();
                        key_value(ui, "Endpoint", output.device_label());
                        ui.separator();
                        key_value(
                            ui,
                            "Dynamic objects",
                            &format!(
                                "{} active / {} reserved / {} max",
                                output.active_dynamic_objects(),
                                output.reserved_dynamic_objects(),
                                output.max_dynamic_objects()
                            ),
                        );
                        ui.separator();
                        key_value(
                            ui,
                            "Render updates / frames",
                            &format!(
                                "{} / {}",
                                output.render_updates(),
                                output.submitted_frames()
                            ),
                        );
                        ui.separator();
                        key_value(
                            ui,
                            "Object buffers / positions",
                            &format!(
                                "{} / {}",
                                output.object_buffer_submissions(),
                                output.position_updates()
                            ),
                        );
                        ui.separator();
                        key_value(ui, "Underruns", &output.underruns().to_string());
                    });
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(format!(
                            "{} Decoder ownership remains independent of the native output stream.{}",
                            backend.availability(),
                            output
                                .error()
                                .map_or_else(String::new, |error| format!(" Error: {error}"))
                        ))
                        .size(10.0)
                        .color(theme::MUTED),
                    );
                });
        });
}

fn draw_drop_overlay(context: &egui::Context) {
    let painter = context.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("drop-overlay"),
    ));
    let rect = context.content_rect().shrink(24.0);
    painter.rect_filled(
        rect,
        0.0,
        Color32::from_rgba_unmultiplied(255, 253, 247, 244),
    );
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(2.0, theme::ACCENT),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "Drop AC-4 media files",
        egui::FontId::proportional(22.0),
        theme::TEXT,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_sync_preserves_same_request_during_buffering() {
        assert_eq!(
            output_sync_action(DecodePhase::Buffering, true),
            OutputSyncAction::Preserve
        );
    }

    #[test]
    fn output_sync_preserves_renderer_while_a_new_epoch_buffers() {
        assert_eq!(
            output_sync_action(DecodePhase::Buffering, false),
            OutputSyncAction::Preserve
        );
    }

    #[test]
    fn output_sync_preserves_renderer_while_seeking() {
        assert_eq!(
            output_sync_action(DecodePhase::Seeking, false),
            OutputSyncAction::Preserve
        );
    }

    #[test]
    fn output_sync_configures_an_unconfigured_ready_decoder() {
        assert_eq!(
            output_sync_action(DecodePhase::Ready, false),
            OutputSyncAction::Configure
        );
    }

    #[test]
    fn output_sync_resets_during_a_new_open_request() {
        assert_eq!(
            output_sync_action(DecodePhase::Opening, true),
            OutputSyncAction::Reset
        );
    }

    #[test]
    fn playback_restore_uses_the_latest_transport_intent() {
        let mut pending = true;
        assert_eq!(take_playback_restore(&mut pending, true), Some(true));
        assert!(!pending);

        let mut pending = true;
        assert_eq!(take_playback_restore(&mut pending, false), Some(false));
    }

    #[test]
    fn playlist_neighbors_stop_at_the_list_boundaries() {
        assert_eq!(
            neighboring_source_index(Some(1), 3, PlaylistStep::Previous),
            Some(0)
        );
        assert_eq!(
            neighboring_source_index(Some(1), 3, PlaylistStep::Next),
            Some(2)
        );
        assert_eq!(
            neighboring_source_index(Some(0), 3, PlaylistStep::Previous),
            None
        );
        assert_eq!(
            neighboring_source_index(Some(2), 3, PlaylistStep::Next),
            None
        );
    }

    #[test]
    fn repeat_all_neighbors_wrap_at_both_boundaries() {
        assert_eq!(
            wrapped_source_index(Some(0), 3, PlaylistStep::Previous),
            Some(2)
        );
        assert_eq!(
            wrapped_source_index(Some(2), 3, PlaylistStep::Next),
            Some(0)
        );
        assert_eq!(wrapped_source_index(Some(0), 1, PlaylistStep::Next), None);
    }

    #[test]
    fn shuffle_never_immediately_repeats_the_current_item() {
        let mut state = 7;
        for current in 0..4 {
            for _ in 0..32 {
                let next = shuffled_source_index(Some(current), 4, &mut state)
                    .expect("a four-item playlist has another shuffle target");
                assert!(next < 4);
                assert_ne!(next, current);
            }
        }
        assert_eq!(shuffled_source_index(Some(0), 1, &mut state), None);
    }

    #[test]
    fn sequential_playback_is_the_default_mode() {
        assert_eq!(PlaybackMode::default(), PlaybackMode::Sequential);
    }

    #[test]
    fn repeat_modes_replay_a_single_item_playlist() {
        assert!(should_replay_current_on_completion(
            PlaybackMode::RepeatOne,
            3
        ));
        assert!(should_replay_current_on_completion(
            PlaybackMode::RepeatAll,
            1
        ));
        assert!(!should_replay_current_on_completion(
            PlaybackMode::RepeatAll,
            3
        ));
        assert!(!should_replay_current_on_completion(
            PlaybackMode::Sequential,
            1
        ));
    }

    #[test]
    fn completion_is_handled_only_for_the_current_playback_epoch() {
        assert!(should_handle_completed_item(OutputPhase::Ended, true, true));
        assert!(!should_handle_completed_item(
            OutputPhase::Ended,
            true,
            false
        ));
        assert!(!should_handle_completed_item(
            OutputPhase::Playing,
            true,
            true
        ));
        assert!(!should_handle_completed_item(
            OutputPhase::Ended,
            false,
            true
        ));
    }

    #[test]
    fn device_wait_is_scoped_to_the_playback_epoch() {
        let waiting = DeviceWait {
            request_id: 7,
            playback_epoch: 3,
            frame: 96_000,
        };
        assert!(waiting.belongs_to(7, 3));
        assert!(!waiting.belongs_to(7, 4));
        assert!(!waiting.belongs_to(8, 3));
    }

    #[test]
    fn normalized_timeline_maps_to_absolute_frames() {
        assert_eq!(normalized_frame(0.0, 480_000), 0);
        assert_eq!(normalized_frame(0.25, 480_000), 120_000);
        assert_eq!(normalized_frame(1.0, 480_000), 480_000);
    }

    #[test]
    fn timestamp_uses_hours_only_when_needed() {
        assert_eq!(format_timestamp(48_000 * 65, 48_000), "01:05");
        assert_eq!(format_timestamp(48_000 * 3_661, 48_000), "01:01:01");
        assert_eq!(format_timestamp(0, 0), "--:--");
    }

    #[test]
    fn only_scene_signature_failures_trigger_automatic_reconfiguration() {
        assert!(is_reconfigurable_scene_error(
            "Scene configuration generation changed from 1 to 2"
        ));
        assert!(!is_reconfigurable_scene_error(
            "Windows returned an invalid Spatial Audio object buffer"
        ));
    }
}
