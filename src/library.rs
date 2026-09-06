//! A single database owner. UI messages contain identities and immutable snapshots,
//! never a SQLite connection or a media file handle.
mod store;

use std::path::PathBuf;
use std::sync::{
    Arc,
    mpsc::{self, Receiver, Sender},
};
use std::thread::{self, JoinHandle};

use crate::playlist::{
    EntryId, MediaId, PlaybackMode, PlaylistId, PlaylistSnapshot, PlaylistSummary, SavedBrowse,
    SessionState,
};
use crate::preferences::{AppPreferences, DataDirectory, PreferencesStore};

#[derive(Debug)]
pub enum Mutation {
    Create(String),
    Rename(PlaylistId, String),
    Delete(PlaylistId),
    OrderPlaylists(Vec<PlaylistId>),
    Mode(PlaylistId, PlaybackMode),
    Add(PlaylistId, Vec<PathBuf>),
    Remove(PlaylistId, Vec<EntryId>),
    Reorder(PlaylistId, Vec<EntryId>),
    Transfer {
        from: PlaylistId,
        to: PlaylistId,
        entries: Vec<EntryId>,
        remove: bool,
    },
    Relocate(MediaId, PathBuf),
    MediaError(MediaId, Option<String>),
}
enum Command {
    Watch {
        serial: u64,
        browse: Option<PlaylistId>,
        playing: Option<PlaylistId>,
    },
    Mutate(Mutation),
    Browse(PlaylistId, SavedBrowse),
    Session(SessionState),
    Preferences(Box<AppPreferences>),
    Retry,
    Shutdown,
}
struct Snapshot {
    serial: u64,
    revision: u64,
    summaries: Vec<PlaylistSummary>,
    browse: Option<Arc<PlaylistSnapshot>>,
    playing: Option<Arc<PlaylistSnapshot>>,
}
enum Event {
    Boot(Box<AppPreferences>, SessionState),
    Snapshot(Snapshot),
    Message(String),
    Error(String),
    MediaError {
        revision: u64,
        media: MediaId,
        error: Option<String>,
    },
    Recovered,
    Done,
}
pub enum Notice {
    Boot(Box<AppPreferences>, SessionState),
    PlayingChanged {
        old: Option<Arc<PlaylistSnapshot>>,
        new: Option<Arc<PlaylistSnapshot>>,
    },
}

pub struct LibraryController {
    sender: Sender<Command>,
    receiver: Receiver<Event>,
    worker: Option<JoinHandle<()>>,
    serial: u64,
    revision: u64,
    pending: usize,
    pub summaries: Vec<PlaylistSummary>,
    pub media_errors: std::collections::HashMap<MediaId, Option<String>>,
    pub browse: Option<Arc<PlaylistSnapshot>>,
    pub playing: Option<Arc<PlaylistSnapshot>>,
    pub desired_browse: Option<PlaylistId>,
    pub desired_playing: Option<PlaylistId>,
    pub ready: bool,
    pub error: Option<String>,
    pub message: String,
}
impl LibraryController {
    pub fn new(
        directory: Arc<DataDirectory>,
        legacy: AppPreferences,
        context: eframe::egui::Context,
    ) -> Self {
        let (sender, commands) = mpsc::channel();
        let (events, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("player-library".into())
            .spawn(move || {
                run(&directory, legacy, &commands, &events, &context);
            });
        let (worker, error) = match worker {
            Ok(worker) => (Some(worker), None),
            Err(e) => (None, Some(format!("Cannot start library worker: {e}"))),
        };
        Self {
            sender,
            receiver,
            worker,
            serial: 0,
            revision: 0,
            pending: 0,
            summaries: Vec::new(),
            media_errors: std::collections::HashMap::new(),
            browse: None,
            playing: None,
            desired_browse: None,
            desired_playing: None,
            ready: false,
            error,
            message: "Loading library…".into(),
        }
    }
    fn send(&mut self, command: Command) {
        match self.sender.send(command) {
            Ok(()) => self.pending += 1,
            Err(_) => {
                self.error = Some("Library worker is unavailable; changes were not saved".into());
            }
        }
    }
    pub fn busy(&self) -> bool {
        self.pending > 0 || (!self.ready && self.error.is_none())
    }
    pub fn mutate(&mut self, mutation: Mutation) {
        self.send(Command::Mutate(mutation));
    }
    pub fn save_browse(&mut self, id: PlaylistId, state: SavedBrowse) {
        self.send(Command::Browse(id, state));
    }
    pub fn save_session(&mut self, state: SessionState) {
        self.send(Command::Session(state));
    }
    pub fn save_preferences(&mut self, prefs: AppPreferences) {
        self.send(Command::Preferences(Box::new(prefs)));
    }
    pub fn retry(&mut self) {
        self.send(Command::Retry);
    }
    pub fn watch(&mut self, browse: Option<PlaylistId>, playing: Option<PlaylistId>) {
        if self.desired_browse == browse && self.desired_playing == playing {
            return;
        }
        self.serial = self.serial.wrapping_add(1);
        self.desired_browse = browse;
        self.desired_playing = playing;
        if self.browse.as_ref().map(|p| p.summary.id) != browse {
            self.browse = self
                .playing
                .as_ref()
                .filter(|p| Some(p.summary.id) == browse)
                .cloned();
        }
        if self.playing.as_ref().map(|p| p.summary.id) != playing {
            self.playing = self
                .browse
                .as_ref()
                .filter(|p| Some(p.summary.id) == playing)
                .cloned();
        }
        self.send(Command::Watch {
            serial: self.serial,
            browse,
            playing,
        });
    }
    pub fn poll(&mut self) -> Vec<Notice> {
        let mut notices = Vec::new();
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Event::Boot(prefs, session) => {
                    self.desired_browse = session.browse;
                    self.desired_playing = session.cursor.as_ref().and_then(|c| c.playlist);
                    notices.push(Notice::Boot(prefs, session));
                }
                Event::Snapshot(snapshot) => {
                    if snapshot.revision < self.revision {
                        continue;
                    }
                    self.revision = snapshot.revision;
                    self.summaries = snapshot.summaries;
                    if snapshot.serial != self.serial {
                        continue;
                    }
                    self.media_errors.clear();
                    let old = self.playing.take();
                    self.playing = snapshot.playing;
                    self.browse = snapshot.browse;
                    self.desired_browse = self.browse.as_ref().map(|p| p.summary.id);
                    self.desired_playing = self.playing.as_ref().map(|p| p.summary.id);
                    if !self.ready && self.message == "Loading library…" {
                        self.message = "Library ready".into();
                    }
                    self.ready = true;
                    notices.push(Notice::PlayingChanged {
                        old,
                        new: self.playing.clone(),
                    });
                }
                Event::Message(message) => {
                    if !message.is_empty() {
                        self.message = message;
                    }
                }
                Event::Error(error) => self.error = Some(error),
                Event::MediaError {
                    revision,
                    media,
                    error,
                } => {
                    if revision >= self.revision {
                        self.revision = revision;
                        self.media_errors.insert(media, error);
                    }
                }
                Event::Recovered => {
                    self.error = None;
                    self.message = "Storage available; repeat any failed playlist operation".into();
                }
                Event::Done => self.pending = self.pending.saturating_sub(1),
            }
        }
        notices
    }
    pub fn shutdown(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = self.sender.send(Command::Shutdown);
            let _ = worker.join();
            // Surface any failed final write instead of silently reporting a
            // clean shutdown. The native shutdown path can display this error.
            let _ = self.poll();
        }
    }
}
impl Drop for LibraryController {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Watch {
    serial: u64,
    revision: u64,
    browse: Option<PlaylistId>,
    playing: Option<PlaylistId>,
}
impl Watch {
    fn snapshot(&mut self, store: &store::Store) -> store::Result<Snapshot> {
        let summaries = store.summaries()?;
        if !summaries.iter().any(|p| Some(p.id) == self.browse) {
            self.browse = summaries.first().map(|p| p.id);
        }
        if !summaries.iter().any(|p| Some(p.id) == self.playing) {
            self.playing = None;
        }
        let browse = self.browse.map(|id| store.list(id)).transpose()?.flatten();
        let playing = if self.playing == self.browse {
            browse.clone()
        } else {
            self.playing.map(|id| store.list(id)).transpose()?.flatten()
        };
        Ok(Snapshot {
            serial: self.serial,
            revision: self.revision,
            summaries,
            browse,
            playing,
        })
    }
}
#[allow(
    clippy::too_many_lines,
    reason = "one worker owns startup recovery and the serialized command lifecycle"
)]
fn run(
    directory: &DataDirectory,
    legacy: AppPreferences,
    commands: &Receiver<Command>,
    events: &Sender<Event>,
    context: &eframe::egui::Context,
) {
    let emit = |event| {
        let _ = events.send(event);
        context.request_repaint();
    };
    let (mut preferences, prefs, warning) = PreferencesStore::load(&directory.path, legacy);
    if let Some(warning) = warning {
        emit(Event::Error(warning));
    }
    let path = directory.path.join("library.sqlite3");
    let mut store = match store::Store::open(&path) {
        Ok(s) => Some(s),
        Err(e) => {
            emit(Event::Error(format!("Cannot open library: {e}")));
            None
        }
    };
    let session = match store.as_ref().map(store::Store::session).transpose() {
        Ok(session) => session.unwrap_or_default(),
        Err(e) => {
            emit(Event::Error(format!("Cannot restore session: {e}")));
            SessionState::default()
        }
    };
    let mut watch = Watch {
        serial: 0,
        revision: 0,
        browse: session.browse,
        playing: session.cursor.as_ref().and_then(|c| c.playlist),
    };
    let mut requested_preferences = prefs.clone();
    let mut requested_session = session.clone();
    emit(Event::Boot(Box::new(prefs), session));
    if let Some(store) = &store {
        match watch.snapshot(store) {
            Ok(s) => emit(Event::Snapshot(s)),
            Err(e) => emit(Event::Error(e.to_string())),
        }
    }
    while let Ok(command) = commands.recv() {
        if matches!(command, Command::Shutdown) {
            break;
        }
        let result = (|| -> store::Result<bool> {
            match command {
                Command::Preferences(prefs) => {
                    requested_preferences = *prefs;
                    preferences.save(&requested_preferences)?;
                    Ok(false)
                }
                Command::Retry => {
                    let (replacement, _, warning) =
                        PreferencesStore::load(&directory.path, requested_preferences.clone());
                    preferences = replacement;
                    if let Some(warning) = warning {
                        return Err(warning.into());
                    }
                    preferences.save(&requested_preferences)?;
                    if let Some(store) = &store {
                        store.save_session(&requested_session)?;
                    } else {
                        let reopened = store::Store::open(&path)?;
                        requested_session = reopened.session()?;
                        watch.browse = requested_session.browse;
                        watch.playing = requested_session.cursor.as_ref().and_then(|c| c.playlist);
                        store = Some(reopened);
                        emit(Event::Boot(
                            Box::new(requested_preferences.clone()),
                            requested_session.clone(),
                        ));
                    }
                    emit(Event::Recovered);
                    Ok(true)
                }
                other => {
                    let store = store
                        .as_mut()
                        .ok_or("Library is unavailable; changes were not saved")?;
                    match other {
                        Command::Watch {
                            serial,
                            browse,
                            playing,
                        } => {
                            watch.serial = serial;
                            watch.browse = browse;
                            watch.playing = playing;
                            Ok(true)
                        }
                        Command::Mutate(mutation) => {
                            let change = store.mutate(mutation)?;
                            watch.revision = watch.revision.saturating_add(1);
                            if let Some(view) = change.view {
                                watch.browse = Some(view);
                            }
                            if let Some((media, error)) = change.media_error {
                                emit(Event::MediaError {
                                    revision: watch.revision,
                                    media,
                                    error,
                                });
                                Ok(false)
                            } else {
                                emit(Event::Message(change.message));
                                Ok(true)
                            }
                        }
                        Command::Browse(id, state) => {
                            store.save_browse(id, &state)?;
                            Ok(false)
                        }
                        Command::Session(state) => {
                            requested_session = state;
                            store.save_session(&requested_session)?;
                            Ok(false)
                        }
                        Command::Preferences(_) | Command::Retry | Command::Shutdown => {
                            unreachable!()
                        }
                    }
                }
            }
        })();
        match result {
            Ok(true) => {
                if let Some(store) = &store {
                    match watch.snapshot(store) {
                        Ok(s) => emit(Event::Snapshot(s)),
                        Err(e) => emit(Event::Error(e.to_string())),
                    }
                }
            }
            Ok(false) => {}
            Err(e) => emit(Event::Error(format!("Not saved: {e}"))),
        }
        emit(Event::Done);
    }
}
