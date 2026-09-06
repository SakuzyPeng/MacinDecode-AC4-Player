//! Connect persisted library identities to the existing decoder/output lifecycle.
use super::{
    AppPreferences, Arc, Context, DecodePhase, DialogWake, Duration, EntryId, Future, Instant,
    MediaSource, Mutation, Path, PathBuf, Pin, PlaybackCursor, PlaybackMode, PlayerApp, PlaylistId,
    PlaylistStep, Poll, SelectedSource, SessionState, StatusLine, Waker, decoder_status_line, egui,
    scene3d, should_handle_completed_item, should_replay_current_on_completion,
    shuffled_source_index,
};
use crate::library::Notice;
use crate::playlist::{Entry, reordered};
use crate::playlist_ui::Action;
#[cfg(test)]
mod tests;

pub(super) struct FilePick {
    target: PickTarget,
    future: Pin<Box<dyn Future<Output = Vec<PathBuf>>>>,
}
enum PickTarget {
    Add(PlaylistId),
    Relocate(crate::playlist::MediaId),
}

impl PlayerApp {
    pub(super) fn tick(&mut self, context: &egui::Context, showing: bool) {
        self.poll_library(context);
        self.poll_sofa_picker(context);
        self.sync_inspection(context);
        self.sync_decoder(context);
        if !self.restore_session_seek(context) {
            self.sync_output(context, showing);
        }
        self.persist_state(context, false);
    }
    pub(super) fn retry_playback(&mut self) {
        self.inspection_media = None;
        if let Some(cursor) = self.cursor.clone() {
            if let Some(list) = cursor.playlist {
                self.activate_entry(list, cursor.entry, true, false);
            } else {
                self.resume = None;
                self.decoder.close();
                self.media_source = None;
                self.output.pause();
                self.output.reset();
                self.playback_intent = true;
                self.playback_restore_pending = true;
                self.marked_failure_key = None;
            }
        }
    }

    pub(super) fn selected_source(&self) -> Option<&SelectedSource> {
        self.library
            .browse
            .as_ref()?
            .entry(self.browse.saved.focus?)
            .map(|e| &e.source)
    }
    pub(super) fn selected_path(&self) -> Option<&Path> {
        self.selected_source().map(SelectedSource::path)
    }
    pub(super) fn playback_media(&mut self) -> Option<MediaSource> {
        let path = self.cursor.as_ref().map(|c| c.entry.source.path());
        if self.media_source.as_ref().map(MediaSource::path) != path {
            self.media_source = self
                .inspection_media
                .as_ref()
                .filter(|source| Some(source.path()) == path && !source.cached_open_failed())
                .cloned()
                .or_else(|| path.map(MediaSource::new));
        }
        self.media_source.clone()
    }
    pub(super) fn browsed_media(&mut self) -> Option<MediaSource> {
        let path = self.selected_path().map(Path::to_path_buf);
        // The inspection-only build has no playback consumer. Let its worker
        // release the open file after publishing the report, as before.
        if !cfg!(feature = "decode") {
            return path.as_deref().map(MediaSource::new);
        }
        if path.as_deref() == self.cursor.as_ref().map(|c| c.entry.source.path()) {
            return self.playback_media();
        }
        if self.inspection_media.as_ref().map(MediaSource::path) != path.as_deref() {
            self.inspection_media = path.as_deref().map(MediaSource::new);
        }
        self.inspection_media.clone()
    }
    pub(super) fn poll_library(&mut self, context: &egui::Context) {
        let notices = self.library.poll();
        let refreshed = notices
            .iter()
            .any(|notice| matches!(notice, Notice::PlayingChanged { .. }));
        for notice in notices {
            match notice {
                Notice::Boot(prefs, session) => {
                    self.preferences = *prefs;
                    self.output.manual_head(self.preferences.manual_head);
                    self.output
                        .install_settings(self.preferences.output.clone());
                    self.volume = self.preferences.volume;
                    self.muted = self.preferences.muted;
                    self.camera = scene3d::camera::Camera::from_state(self.preferences.camera);
                    self.object_numbers_visible = self.preferences.object_numbers;
                    self.preferences_observed = self.preferences.clone();
                    self.cursor.clone_from(&session.cursor);
                    self.checkpoint = session.clone();
                    self.resume = self.cursor.as_ref().map(|_| session);
                    self.playback_intent = false;
                    self.playback_restore_pending = false;
                }
                Notice::PlayingChanged { old, new } => {
                    if let Some(cursor) = &mut self.cursor {
                        cursor.reconcile(old.as_deref(), new.as_deref());
                        if cursor.playlist.is_none() {
                            self.automatic_candidate = false;
                        }
                    }
                    self.shuffle_history
                        .retain(|id| new.as_ref().is_some_and(|p| p.positions.contains_key(id)));
                }
            }
        }
        if refreshed && let Some(list) = &self.library.browse {
            self.browse.install(list);
        }
        self.playback_mode = self.effective_playback_mode();
        if let Some(mut picker) = self.file_picker.take() {
            let waker = Waker::from(Arc::new(DialogWake(context.clone())));
            let mut task = Context::from_waker(&waker);
            match picker.future.as_mut().poll(&mut task) {
                Poll::Pending => self.file_picker = Some(picker),
                Poll::Ready(paths) => {
                    if let Some(path) = paths.first() {
                        if let Some(parent) = path.parent() {
                            self.preferences.last_directory = parent.to_path_buf();
                        }
                        match picker.target {
                            PickTarget::Add(id) => self.library.mutate(Mutation::Add(id, paths)),
                            PickTarget::Relocate(id) => {
                                self.library.mutate(Mutation::Relocate(id, path.clone()));
                            }
                        }
                    }
                }
            }
        }
        if self.library.busy() {
            context.request_repaint_after(Duration::from_millis(50));
        }
    }
    fn save_browse_now(&mut self) {
        if let Some(id) = self.browse.playlist {
            self.library.save_browse(id, self.browse.saved.clone());
            self.browse_observed = self.browse.saved.clone();
            self.browse_dirty_at = None;
        }
    }
    pub(super) fn switch_playlist(&mut self, id: PlaylistId) {
        self.save_browse_now();
        self.library
            .watch(Some(id), self.cursor.as_ref().and_then(|c| c.playlist));
    }
    pub(super) fn choose_sources(&mut self) {
        let Some(id) = self.library.desired_browse else {
            return;
        };
        if self.file_picker.is_some() {
            return;
        }
        let mut dialog = rfd::AsyncFileDialog::new()
            .set_title("Add AC-4 media to playlist")
            .add_filter("AC-4 media", &["m4a", "mp4", "ac4"]);
        if !self.preferences.last_directory.as_os_str().is_empty() {
            dialog = dialog.set_directory(&self.preferences.last_directory);
        }
        self.file_picker = Some(FilePick {
            target: PickTarget::Add(id),
            future: Box::pin(async move {
                dialog
                    .pick_files()
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|f| f.path().to_path_buf())
                    .collect()
            }),
        });
    }
    pub(super) fn append_sources(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        if let Some(id) = self.library.desired_browse {
            self.library
                .mutate(Mutation::Add(id, paths.into_iter().collect()));
        }
    }
    pub(super) fn handle_playlist_action(&mut self, action: Action) {
        match action {
            Action::Switch(id) => self.switch_playlist(id),
            Action::Add => self.choose_sources(),
            Action::Play(id) => self.play_browsed_entry(id),
            Action::Retry(id) => {
                if let Some(entry) = self
                    .library
                    .browse
                    .as_ref()
                    .and_then(|p| p.entry(id))
                    .cloned()
                {
                    self.inspection.retry(entry.source.path());
                    self.library.mutate(Mutation::MediaError(entry.media, None));
                    if self
                        .cursor
                        .as_ref()
                        .is_some_and(|c| c.entry.media == entry.media)
                    {
                        self.play_browsed_entry(id);
                    }
                }
            }
            Action::Relocate(id) => {
                if self.file_picker.is_none() {
                    self.file_picker = Some(FilePick {
                        target: PickTarget::Relocate(id),
                        future: Box::pin(async move {
                            rfd::AsyncFileDialog::new()
                                .set_title("Locate AC-4 media")
                                .add_filter("AC-4 media", &["m4a", "mp4", "ac4"])
                                .pick_file()
                                .await
                                .into_iter()
                                .map(|f| f.path().to_path_buf())
                                .collect()
                        }),
                    });
                }
            }
            Action::Mutate(mutation) => {
                self.save_browse_now();
                self.library.mutate(mutation);
            }
            action => {
                let Some(list) = self.library.browse.clone() else {
                    return;
                };
                let id = list.summary.id;
                let selection = self.browse.ordered_selection(&list);
                let mutation = match action {
                    Action::Remove => Mutation::Remove(id, selection),
                    Action::Transfer { to, remove } => Mutation::Transfer {
                        from: id,
                        to,
                        entries: selection,
                        remove,
                    },
                    Action::Drop { entries, gap } => Mutation::Reorder(
                        id,
                        reordered(
                            &list.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
                            &entries,
                            gap,
                        ),
                    ),
                    Action::MoveSelection(up) => {
                        let mut order: Vec<_> = list.entries.iter().map(|e| e.id).collect();
                        if up {
                            for index in 1..order.len() {
                                if self.browse.selected.contains(&order[index])
                                    && !self.browse.selected.contains(&order[index - 1])
                                {
                                    order.swap(index, index - 1);
                                }
                            }
                        } else {
                            for index in (0..order.len().saturating_sub(1)).rev() {
                                if self.browse.selected.contains(&order[index])
                                    && !self.browse.selected.contains(&order[index + 1])
                                {
                                    order.swap(index, index + 1);
                                }
                            }
                        }
                        Mutation::Reorder(id, order)
                    }
                    _ => unreachable!(),
                };
                self.save_browse_now();
                self.library.mutate(mutation);
            }
        }
    }
    pub(super) fn play_browsed_entry(&mut self, id: EntryId) {
        if let Some(list) = self.library.browse.clone()
            && let Some(entry) = list.entry(id).cloned()
        {
            self.activate_entry(list.summary.id, entry, true, false);
        }
    }
    fn activate_entry(&mut self, list: PlaylistId, entry: Entry, playing: bool, automatic: bool) {
        self.save_checkpoint();
        self.output.pause();
        self.output.reset();
        if let Some(previous) = self.pending_output_change.take() {
            self.output.install_settings(previous);
        }
        self.decoder.close();
        self.media_source = None;
        self.resume = None;
        self.pending_output_change = None;
        self.audio_settings_error = None;
        self.timeline_preview = 0.0;
        self.timeline_dragging = false;
        self.playback_intent = playing;
        self.playback_restore_pending = true;
        self.automatic_reconfigure_guard = None;
        self.waiting_for_device = None;
        self.marked_failure_key = None;
        self.automatic_candidate = automatic;
        if !automatic {
            self.failed_candidates.clear();
            self.shuffle_history.clear();
        }
        self.status = StatusLine::idle(format!("Opening {}", entry.source.display_name()));
        self.cursor = Some(PlaybackCursor::new(list, entry));
        self.checkpoint = SessionState {
            browse: self.library.desired_browse,
            cursor: self.cursor.clone(),
            ..Default::default()
        };
        self.library.watch(self.library.desired_browse, Some(list));
        if let Some(source) = self.playback_media()
            && matches!(
                self.inspection.state(source.path()),
                Some(crate::inspection::InspectionState::Failed(_))
            )
        {
            self.inspection.retry_source(source);
        }
        self.save_checkpoint();
    }
    pub(super) fn effective_playback_mode(&self) -> PlaybackMode {
        let id = self.cursor.as_ref().and_then(|c| c.playlist).or_else(|| {
            self.cursor
                .is_none()
                .then_some(self.browse.playlist)
                .flatten()
        });
        self.library
            .summaries
            .iter()
            .find(|p| Some(p.id) == id)
            .map_or(PlaybackMode::Sequential, |p| p.mode)
    }
    pub(super) fn can_select_neighbor(&self, step: PlaylistStep) -> bool {
        let (Some(cursor), Some(list)) = (&self.cursor, &self.library.playing) else {
            return false;
        };
        match (self.effective_playback_mode(), step) {
            (PlaybackMode::Shuffle, PlaylistStep::Previous) => !self.shuffle_history.is_empty(),
            (PlaybackMode::Shuffle, PlaylistStep::Next) => {
                list.entries.len() > usize::from(cursor.attached)
            }
            (mode, step) => cursor
                .neighbor(
                    list,
                    step == PlaylistStep::Next,
                    mode == PlaybackMode::RepeatAll,
                )
                .is_some(),
        }
    }
    fn neighbor_entry(&mut self, step: PlaylistStep) -> Option<(PlaylistId, Entry)> {
        let list = self.library.playing.clone()?;
        let cursor = self.cursor.as_ref()?;
        let mode = self.effective_playback_mode();
        let id = match (mode, step) {
            (PlaybackMode::Shuffle, PlaylistStep::Previous) => self.shuffle_history.pop()?,
            (PlaybackMode::Shuffle, PlaylistStep::Next) => {
                let candidates: Vec<_> = list
                    .entries
                    .iter()
                    .filter(|e| {
                        (!cursor.attached || e.id != cursor.entry.id)
                            && !self.failed_candidates.contains(&e.id)
                    })
                    .collect();
                if candidates.is_empty() {
                    return None;
                }
                let choice =
                    shuffled_source_index(Some(0), candidates.len() + 1, &mut self.shuffle_state)?
                        - 1;
                if cursor.attached {
                    self.shuffle_history.push(cursor.entry.id);
                }
                candidates[choice].id
            }
            (mode, step) => cursor.neighbor(
                &list,
                step == PlaylistStep::Next,
                mode == PlaybackMode::RepeatAll,
            )?,
        };
        Some((list.summary.id, list.entry(id)?.clone()))
    }
    pub(super) fn select_neighbor(&mut self, step: PlaylistStep) {
        if let Some((list, entry)) = self.neighbor_entry(step) {
            let history = std::mem::take(&mut self.shuffle_history);
            self.activate_entry(list, entry, self.playback_intent, false);
            self.shuffle_history = history;
        }
    }
    pub(super) fn handle_completed_playlist_item(
        &mut self,
        context: &egui::Context,
        matches: bool,
    ) -> bool {
        if !should_handle_completed_item(
            self.output.snapshot().phase(),
            self.playback_intent,
            matches,
        ) {
            return false;
        }
        if let Some(list) = &self.library.playing
            && self.cursor.as_ref().is_some_and(|c| c.attached)
            && should_replay_current_on_completion(
                self.effective_playback_mode(),
                list.entries.len(),
            )
        {
            self.replay_current_source();
            context.request_repaint();
            return true;
        }
        if let Some((list, entry)) = self.neighbor_entry(PlaylistStep::Next) {
            self.failed_candidates.clear();
            self.activate_entry(list, entry, true, true);
            context.request_repaint();
            return true;
        }
        self.playback_intent = false;
        self.playback_restore_pending = false;
        false
    }
    pub(super) fn handle_media_failure(&mut self, context: &egui::Context) -> bool {
        let key = (self.decoder.request_id(), self.decoder.playback_epoch());
        if self.marked_failure_key != Some(key) {
            self.marked_failure_key = Some(key);
            if let Some(cursor) = &self.cursor {
                let error = decoder_status_line(self.decoder.snapshot()).text;
                self.library
                    .mutate(Mutation::MediaError(cursor.entry.media, Some(error)));
                self.failed_candidates.insert(cursor.entry.id);
            }
        }
        if self.automatic_candidate && self.playback_intent {
            if let Some((list, entry)) = self.neighbor_entry(PlaylistStep::Next)
                && !self.failed_candidates.contains(&entry.id)
            {
                self.activate_entry(list, entry, true, true);
                context.request_repaint();
                return true;
            }
            self.automatic_candidate = false;
        }
        false
    }
    pub(super) fn clear_successful_media_error(&mut self) {
        if self.output.snapshot().is_playing()
            && let Some(cursor) = &mut self.cursor
        {
            if cursor.entry.error.take().is_some() || self.marked_failure_key.take().is_some() {
                self.library
                    .mutate(Mutation::MediaError(cursor.entry.media, None));
            }
            self.failed_candidates.clear();
        }
    }
    /// True holds output setup until the restore seek has either been submitted
    /// or explicitly abandoned. No initial frame can become audible meanwhile.
    pub(super) fn restore_session_seek(&mut self, context: &egui::Context) -> bool {
        let Some(saved) = self.resume.clone() else {
            return false;
        };
        if self.cursor.as_ref().map(|c| c.entry.id) != saved.cursor.as_ref().map(|c| c.entry.id) {
            self.resume = None;
            return false;
        }
        if self.decoder.snapshot().phase() == DecodePhase::Failed {
            self.resume = None;
            return false;
        }
        let Some(metrics) = self.decoder.snapshot().metrics() else {
            context.request_repaint_after(Duration::from_millis(50));
            return true;
        };
        if metrics.is_indexing() {
            context.request_repaint_after(Duration::from_millis(50));
            return true;
        }
        let stamp = self
            .media_source
            .as_ref()
            .and_then(MediaSource::cached_stamp);
        let changed = saved.stamp.is_some() && saved.stamp != stamp;
        let ended = metrics
            .duration_frames()
            .is_some_and(|duration| saved.frame >= duration);
        let invalid_rate = saved.sample_rate != 0 && saved.sample_rate != metrics.sample_rate();
        let can_seek = saved.frame == 0 || metrics.can_seek_to(saved.frame);
        self.resume = None;
        if changed || ended || invalid_rate || !can_seek {
            self.checkpoint.frame = 0;
            self.checkpoint.stamp = stamp;
            self.status = StatusLine::warning(if changed || invalid_rate {
                "Media changed; prepared from the beginning"
            } else if ended {
                "Finished item prepared from the beginning"
            } else {
                "Saved position cannot be restored; prepared from the beginning"
            });
            self.library.message.clone_from(&self.status.text);
            self.playback_intent = false;
        } else if saved.frame > 0 {
            match self.decoder.seek(saved.frame) {
                Ok(()) => self.status = StatusLine::idle("Restored saved position"),
                Err(error) => {
                    self.status = StatusLine::warning(error);
                    self.checkpoint.frame = 0;
                    self.library.message.clone_from(&self.status.text);
                    self.playback_intent = false;
                }
            }
        }
        self.playback_restore_pending = true;
        false
    }
    fn current_preferences(&self) -> AppPreferences {
        let mut prefs = self.preferences.clone();
        if !self.output.settings_pending()
            && self.pending_output_change.is_none()
            && self.audio_settings_error.is_none()
        {
            prefs.output = self.output.settings().clone();
        }
        prefs.volume = self.volume;
        prefs.muted = self.muted;
        prefs.camera = self.camera.state();
        prefs.object_numbers = self.object_numbers_visible;
        prefs
    }
    fn save_checkpoint(&mut self) {
        if !self.library.ready {
            return;
        }
        self.checkpoint.browse = self.library.desired_browse;
        self.checkpoint.cursor = self.cursor.clone();
        if let Some(cursor) = &mut self.checkpoint.cursor
            && cursor.attached
            && let Some(entry) = self
                .library
                .playing
                .as_ref()
                .and_then(|p| p.entry(cursor.entry.id))
        {
            cursor.entry.source = entry.source.clone();
        }
        if self.resume.is_none()
            && self.output.is_configured_for_playback(
                self.decoder.request_id(),
                self.decoder.playback_epoch(),
            )
            && !matches!(
                self.decoder.snapshot().phase(),
                DecodePhase::Seeking | DecodePhase::Failed
            )
            && let Some(metrics) = self.decoder.snapshot().metrics()
        {
            self.checkpoint.frame = self
                .output
                .snapshot()
                .playhead_frames()
                .min(metrics.duration_frames().unwrap_or(u64::MAX));
            self.checkpoint.sample_rate = metrics.sample_rate();
            self.checkpoint.stamp = self
                .media_source
                .as_ref()
                .and_then(MediaSource::cached_stamp);
        }
        self.library.save_session(self.checkpoint.clone());
        self.last_session_save = Instant::now();
        self.last_saved_intent = self.playback_intent;
    }
    pub(super) fn persist_state(&mut self, context: &egui::Context, force: bool) {
        if !self.library.ready {
            return;
        }
        let now = Instant::now();
        let prefs = self.current_preferences();
        // Keep the last successful output choice across failed preparation and
        // device initialization; never snapshot the pending candidate.
        self.preferences.output = prefs.output.clone();
        if prefs != self.preferences_observed {
            self.preferences_observed = prefs.clone();
            self.preferences_dirty_at = Some(now);
        }
        if self.browse.saved != self.browse_observed {
            self.browse_observed = self.browse.saved.clone();
            self.browse_dirty_at = Some(now);
        }
        if force
            || self
                .preferences_dirty_at
                .is_some_and(|at| now.duration_since(at) >= Duration::from_millis(500))
        {
            self.library.save_preferences(prefs);
            self.preferences_dirty_at = None;
        }
        if force
            || self
                .browse_dirty_at
                .is_some_and(|at| now.duration_since(at) >= Duration::from_millis(500))
        {
            self.save_browse_now();
        }
        if force
            || self.last_saved_intent != self.playback_intent
            || now.duration_since(self.last_session_save) >= Duration::from_secs(5)
        {
            self.save_checkpoint();
        }
        if self.preferences_dirty_at.is_some() || self.browse_dirty_at.is_some() {
            context.request_repaint_after(Duration::from_millis(100));
        }
        if self.cursor.is_some() {
            context.request_repaint_after(Duration::from_secs(5));
        }
    }
    pub(super) fn flush_persistence(&mut self) {
        if !self.library.ready {
            return;
        }
        self.library.save_preferences(self.current_preferences());
        self.save_browse_now();
        self.save_checkpoint();
    }
}
