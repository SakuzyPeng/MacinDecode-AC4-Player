use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};

use super::Mutation;
use crate::model::SelectedSource;
use crate::playlist::{
    Entry, EntryId, MediaId, PlaylistId, PlaylistSnapshot, PlaylistSummary, SavedBrowse,
    SessionState, decode_path, encode_path,
};

pub(super) type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub(super) struct Store {
    connection: Connection,
}
pub(super) struct Change {
    pub view: Option<PlaylistId>,
    pub message: String,
    pub media_error: Option<(MediaId, Option<String>)>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let application: u32 =
            connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
        if version > 1 || (application != 0 && application != 0x4d41_4334) {
            return Err(
                "Unsupported library version or application; original database preserved".into(),
            );
        }
        if version == 0 {
            let tables: i64 = connection.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )?;
            if tables != 0 {
                return Err("Unrecognized library schema; original database preserved".into());
            }
            connection.execute_batch("BEGIN IMMEDIATE;
                CREATE TABLE playlists (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
                    position INTEGER NOT NULL, mode TEXT NOT NULL DEFAULT '\"Sequential\"', browse TEXT NOT NULL DEFAULT '{}');
                CREATE TABLE media (id INTEGER PRIMARY KEY AUTOINCREMENT, path BLOB NOT NULL UNIQUE,
                    name TEXT NOT NULL, error TEXT);
                CREATE TABLE entries (id INTEGER PRIMARY KEY AUTOINCREMENT,
                    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
                    media_id INTEGER NOT NULL REFERENCES media(id), position INTEGER NOT NULL,
                    UNIQUE(playlist_id, media_id));
                CREATE INDEX entries_order ON entries(playlist_id, position, id);
                CREATE INDEX entries_media ON entries(media_id);
                CREATE TABLE session (id INTEGER PRIMARY KEY CHECK(id=1), state TEXT NOT NULL);
                CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                INSERT INTO playlists(name,position) VALUES ('Default',0);
                PRAGMA application_id=1296122676;
                PRAGMA user_version=1;
                COMMIT;")?;
            connection.execute(
                "INSERT INTO metadata(key,value) VALUES ('platform',?1)",
                [std::env::consts::OS],
            )?;
        }
        let platform: String =
            connection.query_row("SELECT value FROM metadata WHERE key='platform'", [], |r| {
                r.get(0)
            })?;
        if platform != std::env::consts::OS {
            return Err("This library contains paths from a different operating system".into());
        }
        connection.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )?;
        // Journal recovery belongs to SQLite. A malformed database must never
        // be replaced with a fresh empty library.
        let check: String = connection.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(format!("Library integrity check failed: {check}").into());
        }
        let store = Self { connection };
        store.session()?;
        Ok(store)
    }
    pub fn summaries(&self) -> Result<Vec<PlaylistSummary>> {
        let mut query = self.connection.prepare("SELECT p.id,p.name,p.mode,(SELECT count(*) FROM entries e WHERE e.playlist_id=p.id) FROM playlists p ORDER BY p.position,p.id")?;
        let rows = query.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (id, name, mode, count) = row?;
            Ok(PlaylistSummary {
                id: PlaylistId(id),
                name,
                count: usize::try_from(count)?,
                mode: serde_json::from_str(&mode)?,
            })
        })
        .collect()
    }
    pub fn list(&self, id: PlaylistId) -> Result<Option<Arc<PlaylistSnapshot>>> {
        read_list(&self.connection, id)
    }
    pub fn session(&self) -> Result<SessionState> {
        self.connection
            .query_row("SELECT state FROM session WHERE id=1", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .map_or_else(
                || Ok(SessionState::default()),
                |s| Ok(serde_json::from_str(&s)?),
            )
    }
    pub fn save_session(&self, state: &SessionState) -> Result<()> {
        let mut state = state.clone();
        if let Some(cursor) = &mut state.cursor {
            let membership = self
                .connection
                .query_row(
                    "SELECT playlist_id FROM entries WHERE id=?1",
                    [cursor.entry.id.0],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
                .map(PlaylistId);
            // A UI checkpoint may have been queued before the UI received a
            // deletion acknowledgment. Keep the cursor repaired by that transaction.
            if membership != cursor.playlist || !cursor.attached {
                if let Some(repaired) = self
                    .session()?
                    .cursor
                    .filter(|old| old.entry.id == cursor.entry.id && !old.attached)
                {
                    *cursor = repaired;
                } else if cursor.attached {
                    let list = cursor
                        .playlist
                        .map(|id| self.list(id))
                        .transpose()?
                        .flatten();
                    cursor.reconcile(None, list.as_deref());
                }
            }
        }
        self.connection.execute("INSERT INTO session(id,state) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET state=excluded.state", [serde_json::to_string(&state)?])?;
        Ok(())
    }
    pub fn save_browse(&self, id: PlaylistId, state: &SavedBrowse) -> Result<()> {
        self.connection.execute(
            "UPDATE playlists SET browse=?1 WHERE id=?2",
            params![serde_json::to_string(state)?, id.0],
        )?;
        Ok(())
    }
    #[allow(
        clippy::too_many_lines,
        reason = "each command shares one transaction with the repaired playback cursor"
    )]
    pub fn mutate(&mut self, mutation: Mutation) -> Result<Change> {
        if let Mutation::MediaError(id, error) = mutation {
            self.connection.execute(
                "UPDATE media SET error=?1 WHERE id=?2",
                params![error, id.0],
            )?;
            return Ok(Change {
                view: None,
                message: String::new(),
                media_error: Some((id, error)),
            });
        }
        let mut session = self.session()?;
        let old_playing = session
            .cursor
            .as_ref()
            .and_then(|cursor| cursor.playlist)
            .map(|id| self.list(id))
            .transpose()?
            .flatten();
        let tx = self.connection.transaction()?;
        let mut view = None;
        let message = match mutation {
            Mutation::Create(name) => {
                let name = checked_name(&name)?;
                tx.execute("INSERT INTO playlists(name,position) VALUES(?1,(SELECT COALESCE(max(position),-1)+1 FROM playlists))", [name])?;
                view = Some(PlaylistId(tx.last_insert_rowid()));
                "Playlist created".into()
            }
            Mutation::Rename(id, name) => {
                tx.execute(
                    "UPDATE playlists SET name=?1 WHERE id=?2",
                    params![checked_name(&name)?, id.0],
                )?;
                "Playlist renamed".into()
            }
            Mutation::Delete(id) => {
                tx.execute("DELETE FROM playlists WHERE id=?1", [id.0])?;
                tx.execute("INSERT INTO playlists(name,position) SELECT 'Default',0 WHERE NOT EXISTS(SELECT 1 FROM playlists)", [])?;
                "Playlist deleted; media files were kept".into()
            }
            Mutation::OrderPlaylists(order) => {
                let existing = tx
                    .prepare("SELECT id FROM playlists ORDER BY position,id")?
                    .query_map([], |r| r.get::<_, i64>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                validate_order(&existing, &order.iter().map(|id| id.0).collect::<Vec<_>>())?;
                let mut update = tx.prepare("UPDATE playlists SET position=?1 WHERE id=?2")?;
                for (position, id) in order.iter().enumerate() {
                    update.execute(params![i64::try_from(position)?, id.0])?;
                }
                "Playlist order saved".into()
            }
            Mutation::Mode(id, mode) => {
                tx.execute(
                    "UPDATE playlists SET mode=?1 WHERE id=?2",
                    params![serde_json::to_string(&mode)?, id.0],
                )?;
                format!("Playback mode: {}", mode.label())
            }
            Mutation::Add(id, paths) => {
                require_playlist(&tx, id)?;
                let mut next = next_position(&tx, id)?;
                let (mut added, mut duplicates, mut failed) = (0, 0, 0);
                let mut media_insert = tx.prepare(
                    "INSERT INTO media(path,name) VALUES(?1,?2) ON CONFLICT(path) DO NOTHING",
                )?;
                let mut media_id = tx.prepare("SELECT id FROM media WHERE path=?1")?;
                let mut insert = tx.prepare(
                    "INSERT OR IGNORE INTO entries(playlist_id,media_id,position) VALUES(?1,?2,?3)",
                )?;
                for path in paths {
                    let Ok(source) = normalized_source(&path) else {
                        failed += 1;
                        continue;
                    };
                    let encoded = encode_path(source.path());
                    media_insert.execute(params![encoded, source.display_name()])?;
                    let media: i64 = media_id.query_row([encoded], |r| r.get(0))?;
                    if insert.execute(params![id.0, media, next])? == 0 {
                        duplicates += 1;
                    } else {
                        added += 1;
                        next += 1;
                    }
                }
                format!("Added {added} · duplicates {duplicates} · rejected {failed}")
            }
            Mutation::Remove(id, entries) => {
                let mut remove =
                    tx.prepare("DELETE FROM entries WHERE playlist_id=?1 AND id=?2")?;
                let mut count = 0;
                for entry in entries {
                    count += remove.execute(params![id.0, entry.0])?;
                }
                format!("Removed {count} entries; media files were kept")
            }
            Mutation::Reorder(id, order) => {
                let existing = entry_order(&tx, id)?;
                validate_order(&existing, &order.iter().map(|id| id.0).collect::<Vec<_>>())?;
                let mut update =
                    tx.prepare("UPDATE entries SET position=?1 WHERE id=?2 AND playlist_id=?3")?;
                for (position, entry) in order.iter().enumerate() {
                    update.execute(params![i64::try_from(position)?, entry.0, id.0])?;
                }
                "Song order saved".into()
            }
            Mutation::Transfer {
                from,
                to,
                entries,
                remove,
            } => {
                if from == to {
                    return Err("Choose a different destination playlist".into());
                }
                require_playlist(&tx, from)?;
                require_playlist(&tx, to)?;
                let selected: HashSet<_> = entries.into_iter().collect();
                let mut query = tx.prepare(
                    "SELECT id,media_id FROM entries WHERE playlist_id=?1 ORDER BY position,id",
                )?;
                let rows = query
                    .query_map([from.0], |r| Ok((EntryId(r.get(0)?), r.get::<_, i64>(1)?)))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let mut position = next_position(&tx, to)?;
                let (mut added, mut merged) = (0, 0);
                let mut insert = tx.prepare(
                    "INSERT OR IGNORE INTO entries(playlist_id,media_id,position) VALUES(?1,?2,?3)",
                )?;
                let mut delete =
                    tx.prepare("DELETE FROM entries WHERE playlist_id=?1 AND id=?2")?;
                for (entry, media) in rows.into_iter().filter(|(id, _)| selected.contains(id)) {
                    if insert.execute(params![to.0, media, position])? == 0 {
                        merged += 1;
                    } else {
                        added += 1;
                        position += 1;
                    }
                    if remove {
                        delete.execute(params![from.0, entry.0])?;
                    }
                }
                format!(
                    "{} {added} · merged {merged}",
                    if remove { "Moved" } else { "Copied" }
                )
            }
            Mutation::Relocate(id, path) => {
                let source = normalized_source(&path)?;
                let key = encode_path(source.path());
                let collision = tx
                    .query_row(
                        "SELECT id FROM media WHERE path=?1 AND id<>?2",
                        params![key, id.0],
                        |r| r.get::<_, i64>(0),
                    )
                    .optional()?;
                if collision.is_some() {
                    return Err("That file is already a different library item; copy the existing item into the desired lists instead".into());
                }
                tx.execute(
                    "UPDATE media SET path=?1,name=?2,error=NULL WHERE id=?3",
                    params![key, source.display_name(), id.0],
                )?;
                "Media location updated in every playlist".into()
            }
            Mutation::MediaError(id, error) => {
                tx.execute(
                    "UPDATE media SET error=?1 WHERE id=?2",
                    params![error, id.0],
                )?;
                String::new()
            }
        };
        if let Some(cursor) = &mut session.cursor {
            let new_playing = cursor
                .playlist
                .map(|id| read_list(&tx, id))
                .transpose()?
                .flatten();
            cursor.reconcile(old_playing.as_deref(), new_playing.as_deref());
            if cursor.attached
                && let Some(entry) = new_playing.as_ref().and_then(|p| p.entry(cursor.entry.id))
            {
                cursor.entry.source.clone_from(&entry.source);
            }
        }
        if let Some(browse) = session.browse
            && tx
                .query_row(
                    "SELECT 1 FROM playlists WHERE id=?1",
                    [browse.0],
                    |_| Ok(()),
                )
                .optional()?
                .is_none()
        {
            session.browse = tx
                .query_row(
                    "SELECT id FROM playlists ORDER BY position,id LIMIT 1",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
                .map(PlaylistId);
        }
        tx.execute("INSERT INTO session(id,state) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET state=excluded.state", [serde_json::to_string(&session)?])?;
        tx.commit()?;
        Ok(Change {
            view,
            message,
            media_error: None,
        })
    }
}

fn read_list(connection: &Connection, id: PlaylistId) -> Result<Option<Arc<PlaylistSnapshot>>> {
    let row = connection
        .query_row(
            "SELECT name,mode,browse FROM playlists WHERE id=?1",
            [id.0],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((name, mode, browse)) = row else {
        return Ok(None);
    };
    let mut query = connection.prepare("SELECT e.id,m.id,m.path,m.error FROM entries e JOIN media m ON e.media_id=m.id WHERE e.playlist_id=?1 ORDER BY e.position,e.id")?;
    let rows = query.query_map([id.0], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Vec<u8>>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    let entries: Vec<Entry> = rows
        .map(|row| {
            let (entry, media, path, error) = row?;
            let path = decode_path(path)?;
            let source = SelectedSource::from_path(path).map_err(|e| e.to_string())?;
            Ok(Entry {
                id: EntryId(entry),
                media: MediaId(media),
                source,
                error,
            })
        })
        .collect::<Result<_>>()?;
    Ok(Some(Arc::new(PlaylistSnapshot::new(
        PlaylistSummary {
            id,
            name,
            mode: serde_json::from_str(&mode)?,
            count: entries.len(),
        },
        entries,
        serde_json::from_str(&browse)?,
    ))))
}

fn require_playlist(connection: &Connection, id: PlaylistId) -> Result<()> {
    if connection
        .query_row("SELECT 1 FROM playlists WHERE id=?1", [id.0], |_| Ok(()))
        .optional()?
        .is_none()
    {
        return Err("The playlist no longer exists".into());
    }
    Ok(())
}
fn next_position(connection: &Connection, id: PlaylistId) -> Result<i64> {
    Ok(connection.query_row(
        "SELECT COALESCE(max(position),-1)+1 FROM entries WHERE playlist_id=?1",
        [id.0],
        |r| r.get(0),
    )?)
}
fn entry_order(connection: &Connection, id: PlaylistId) -> Result<Vec<i64>> {
    Ok(connection
        .prepare("SELECT id FROM entries WHERE playlist_id=?1 ORDER BY position,id")?
        .query_map([id.0], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?)
}
fn validate_order(existing: &[i64], order: &[i64]) -> Result<()> {
    let expected: HashSet<_> = existing.iter().copied().collect();
    let actual: HashSet<_> = order.iter().copied().collect();
    if order.len() != existing.len() || actual.len() != order.len() || actual != expected {
        return Err("The list changed while reordering; try the operation again".into());
    }
    Ok(())
}
fn checked_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() || name.contains('\0') {
        return Err("Enter a non-empty playlist name".into());
    }
    Ok(name)
}
fn normalized_source(path: &Path) -> Result<SelectedSource> {
    let absolute = std::path::absolute(path)?;
    let path = absolute.canonicalize().unwrap_or_else(|_| {
        let mut normalized = PathBuf::new();
        for part in absolute.components() {
            match part {
                Component::CurDir => {}
                Component::ParentDir => {
                    normalized.pop();
                }
                _ => normalized.push(part),
            }
        }
        normalized
    });
    Ok(SelectedSource::from_path(path).map_err(|e| e.to_string())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deletion_repairs_resume_in_the_transaction_and_rejects_stale_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = store.summaries().unwrap()[0].id;
        store
            .mutate(Mutation::Add(
                id,
                (0..3)
                    .map(|i| dir.path().join(format!("{i}.ac4")))
                    .collect(),
            ))
            .unwrap();
        let list = store.list(id).unwrap().unwrap();
        let state = SessionState {
            browse: Some(id),
            cursor: Some(crate::playlist::PlaybackCursor::new(
                id,
                list.entries[0].clone(),
            )),
            frame: 480_000,
            sample_rate: 48_000,
            stamp: None,
        };
        store.save_session(&state).unwrap();
        store
            .mutate(Mutation::Remove(id, vec![list.entries[0].id]))
            .unwrap();
        store.save_session(&state).unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let restored = store.session().unwrap();
        assert_eq!(restored.frame, 480_000);
        let cursor = restored.cursor.unwrap();
        assert!(!cursor.attached);
        assert_eq!(cursor.next_anchor, Some(list.entries[1].id));
    }
    #[test]
    fn failure_halfway_through_move_rolls_back_both_lists() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("db")).unwrap();
        let a = store.summaries().unwrap()[0].id;
        let b = store
            .mutate(Mutation::Create("B".into()))
            .unwrap()
            .view
            .unwrap();
        store
            .mutate(Mutation::Add(
                a,
                (0..2)
                    .map(|i| dir.path().join(format!("{i}.ac4")))
                    .collect(),
            ))
            .unwrap();
        let list = store.list(a).unwrap().unwrap();
        store.connection.execute_batch(&format!("CREATE TRIGGER fail_transfer BEFORE INSERT ON entries WHEN NEW.playlist_id={} AND NEW.media_id={} BEGIN SELECT RAISE(ABORT,'injected failure'); END;",b.0,list.entries[1].media.0)).unwrap();
        assert!(
            store
                .mutate(Mutation::Transfer {
                    from: a,
                    to: b,
                    entries: list.entries.iter().map(|e| e.id).collect(),
                    remove: true
                })
                .is_err()
        );
        assert_eq!(store.list(a).unwrap().unwrap().entries.len(), 2);
        assert!(store.list(b).unwrap().unwrap().entries.is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_survive_database_and_session_json() {
        use std::os::unix::ffi::OsStringExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join(std::ffi::OsString::from_vec(b"song-\xff.ac4".to_vec()));
        let mut store = Store::open(&dir.path().join("db")).unwrap();
        let id = store.summaries().unwrap()[0].id;
        store.mutate(Mutation::Add(id, vec![path.clone()])).unwrap();
        let list = store.list(id).unwrap().unwrap();
        assert_eq!(list.entries[0].source.path(), path);
        store
            .save_session(&SessionState {
                cursor: Some(crate::playlist::PlaybackCursor::new(
                    id,
                    list.entries[0].clone(),
                )),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            store.session().unwrap().cursor.unwrap().entry.source.path(),
            path
        );
    }
    #[test]
    fn transactions_deduplicate_move_and_preserve_identity() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("library.sqlite3")).unwrap();
        let a = store.summaries().unwrap()[0].id;
        let b = store
            .mutate(Mutation::Create("B".into()))
            .unwrap()
            .view
            .unwrap();
        let paths = vec![dir.path().join("中文 1.ac4"), dir.path().join("2.ac4")];
        store.mutate(Mutation::Add(a, paths.clone())).unwrap();
        store.mutate(Mutation::Add(a, paths.clone())).unwrap();
        let source = store.list(a).unwrap().unwrap();
        assert_eq!(source.entries.len(), 2);
        let ids = source.entries.iter().map(|e| e.id).collect::<Vec<_>>();
        store
            .mutate(Mutation::Transfer {
                from: a,
                to: b,
                entries: ids.clone(),
                remove: false,
            })
            .unwrap();
        store
            .mutate(Mutation::Transfer {
                from: a,
                to: b,
                entries: ids,
                remove: true,
            })
            .unwrap();
        assert!(store.list(a).unwrap().unwrap().entries.is_empty());
        let target = store.list(b).unwrap().unwrap();
        assert_eq!(target.entries.len(), 2);
        assert_eq!(target.entries[0].source.display_name(), "中文 1.ac4");
        assert!(
            store
                .mutate(Mutation::Reorder(b, vec![target.entries[0].id; 2]))
                .is_err()
        );
        assert_eq!(
            store.list(b).unwrap().unwrap().entries[1].id,
            target.entries[1].id
        );
        drop(store);
        assert_eq!(
            Store::open(&dir.path().join("library.sqlite3"))
                .unwrap()
                .summaries()
                .unwrap()
                .len(),
            2
        );
    }
    #[test]
    fn deleting_last_list_creates_a_fresh_identity_and_rolls_back_failed_moves() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("db")).unwrap();
        let id = store.summaries().unwrap()[0].id;
        store
            .mutate(Mutation::Add(id, vec![dir.path().join("a.ac4")]))
            .unwrap();
        let entry = store.list(id).unwrap().unwrap().entries[0].id;
        assert!(
            store
                .mutate(Mutation::Transfer {
                    from: id,
                    to: PlaylistId(999),
                    entries: vec![entry],
                    remove: true
                })
                .is_err()
        );
        assert!(store.list(id).unwrap().unwrap().entry(entry).is_some());
        store.mutate(Mutation::Delete(id)).unwrap();
        assert_ne!(store.summaries().unwrap()[0].id, id);
    }
    #[test]
    #[ignore = "synthetic 100,000-entry capacity and IO benchmark"]
    fn capacity_one_hundred_thousand_entries() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("db")).unwrap();
        let a = store.summaries().unwrap()[0].id;
        let b = store
            .mutate(Mutation::Create("B".into()))
            .unwrap()
            .view
            .unwrap();
        let start = std::time::Instant::now();
        store
            .mutate(Mutation::Add(
                a,
                (0..50_000)
                    .map(|i| dir.path().join(format!("{i}.ac4")))
                    .collect(),
            ))
            .unwrap();
        let inserted = start.elapsed();
        let load = std::time::Instant::now();
        let list = store.list(a).unwrap().unwrap();
        let loaded = load.elapsed();
        let ids: Vec<_> = list.entries.iter().map(|e| e.id).collect();
        let transfer = std::time::Instant::now();
        store
            .mutate(Mutation::Transfer {
                from: a,
                to: b,
                entries: ids.clone(),
                remove: false,
            })
            .unwrap();
        let transferred = transfer.elapsed();
        let reorder = std::time::Instant::now();
        store
            .mutate(Mutation::Reorder(a, ids.into_iter().rev().collect()))
            .unwrap();
        assert_eq!(
            store
                .summaries()
                .unwrap()
                .iter()
                .map(|p| p.count)
                .sum::<usize>(),
            100_000
        );
        assert_eq!(
            store.list(a).unwrap().unwrap().entries[0].id,
            list.entries[49_999].id
        );
        eprintln!(
            "capacity: add 50k={inserted:?}, load 50k={loaded:?}, copy 50k={transferred:?}, reorder 50k={:?}",
            reorder.elapsed()
        );
    }
}
