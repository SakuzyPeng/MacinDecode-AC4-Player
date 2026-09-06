//! Playlist identities and navigation. This module never opens files or devices.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::media::FileStamp;
use crate::model::SelectedSource;

macro_rules! identity {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub i64);
    };
}
identity!(PlaylistId);
identity!(MediaId);
identity!(EntryId);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackMode {
    #[default]
    Sequential,
    RepeatOne,
    RepeatAll,
    Shuffle,
}
impl PlaybackMode {
    pub const ALL: [Self; 4] = [
        Self::Sequential,
        Self::RepeatOne,
        Self::RepeatAll,
        Self::Shuffle,
    ];
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sequential => "Sequential",
            Self::RepeatOne => "Repeat one",
            Self::RepeatAll => "Repeat all",
            Self::Shuffle => "Shuffle",
        }
    }
    pub const fn description(self) -> &'static str {
        match self {
            Self::Sequential => "Play the list once, then stop",
            Self::RepeatOne => "Repeat the current item",
            Self::RepeatAll => "Repeat the entire playlist",
            Self::Shuffle => "Choose a different item at random",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: EntryId,
    pub media: MediaId,
    pub source: SelectedSource,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PlaylistSummary {
    pub id: PlaylistId,
    pub name: String,
    pub count: usize,
    pub mode: PlaybackMode,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedBrowse {
    pub focus: Option<EntryId>,
    pub scroll_entry: Option<EntryId>,
    pub scroll_offset: f32,
}

#[derive(Debug)]
pub struct PlaylistSnapshot {
    pub summary: PlaylistSummary,
    pub entries: Vec<Entry>,
    pub positions: HashMap<EntryId, usize>,
    pub browse: SavedBrowse,
}
impl PlaylistSnapshot {
    pub fn new(summary: PlaylistSummary, entries: Vec<Entry>, browse: SavedBrowse) -> Self {
        let positions = entries.iter().enumerate().map(|(i, e)| (e.id, i)).collect();
        Self {
            summary,
            entries,
            positions,
            browse,
        }
    }
    pub fn entry(&self, id: EntryId) -> Option<&Entry> {
        self.entries.get(*self.positions.get(&id)?)
    }
}

#[derive(Debug, Default)]
pub struct BrowseState {
    pub playlist: Option<PlaylistId>,
    pub saved: SavedBrowse,
    pub selected: HashSet<EntryId>,
    pub range_anchor: Option<EntryId>,
    pub restore_scroll: bool,
}
impl BrowseState {
    pub fn install(&mut self, list: &PlaylistSnapshot) {
        if self.playlist != Some(list.summary.id) {
            self.playlist = Some(list.summary.id);
            self.saved = list.browse.clone();
            self.selected.clear();
            self.restore_scroll = true;
        }
        if self
            .saved
            .focus
            .is_none_or(|id| !list.positions.contains_key(&id))
        {
            self.saved.focus = list.entries.first().map(|e| e.id);
        }
        self.selected.retain(|id| list.positions.contains_key(id));
        if self.selected.is_empty() {
            self.selected.extend(self.saved.focus);
        }
    }
    pub fn select(&mut self, list: &PlaylistSnapshot, id: EntryId, toggle: bool, range: bool) {
        if range {
            let anchor = self.range_anchor.or(self.saved.focus).unwrap_or(id);
            if let (Some(&a), Some(&b)) = (list.positions.get(&anchor), list.positions.get(&id)) {
                if !toggle {
                    self.selected.clear();
                }
                self.selected
                    .extend(list.entries[a.min(b)..=a.max(b)].iter().map(|e| e.id));
            }
        } else {
            if !toggle {
                self.selected.clear();
            }
            if !toggle || !self.selected.remove(&id) {
                self.selected.insert(id);
            }
            self.range_anchor = Some(id);
        }
        self.saved.focus = Some(id);
    }
    pub fn ordered_selection(&self, list: &PlaylistSnapshot) -> Vec<EntryId> {
        list.entries
            .iter()
            .filter(|e| self.selected.contains(&e.id))
            .map(|e| e.id)
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackCursor {
    pub playlist: Option<PlaylistId>,
    pub entry: Entry,
    pub attached: bool,
    pub next_anchor: Option<EntryId>,
    #[serde(default)]
    pub previous_anchor: Option<EntryId>,
}
impl PlaybackCursor {
    pub fn new(playlist: PlaylistId, entry: Entry) -> Self {
        Self {
            playlist: Some(playlist),
            entry,
            attached: true,
            next_anchor: None,
            previous_anchor: None,
        }
    }
    /// Keep the open media alive when membership changes. Anchors follow stable
    /// identities, including when multiple pending transactions remove successors.
    pub fn reconcile(&mut self, old: Option<&PlaylistSnapshot>, new: Option<&PlaylistSnapshot>) {
        let Some(new) = new.filter(|p| Some(p.summary.id) == self.playlist) else {
            self.playlist = None;
            self.attached = false;
            self.next_anchor = None;
            self.previous_anchor = None;
            return;
        };
        if self.attached && new.entry(self.entry.id).is_some() {
            return;
        }
        let old_index = old.and_then(|p| p.positions.get(&self.entry.id)).copied();
        let next_index = if self.attached {
            old_index.map(|i| i + 1)
        } else {
            old.and_then(|p| self.next_anchor.and_then(|id| p.positions.get(&id)))
                .copied()
        };
        let previous_index = if self.attached {
            old_index
        } else {
            old.and_then(|p| self.previous_anchor.and_then(|id| p.positions.get(&id)))
                .map(|i| i + 1)
        };
        if self.attached
            || self
                .next_anchor
                .is_some_and(|id| !new.positions.contains_key(&id))
        {
            self.next_anchor = old.zip(next_index).and_then(|(p, i)| {
                p.entries[i..]
                    .iter()
                    .find(|e| new.positions.contains_key(&e.id))
                    .map(|e| e.id)
            });
        }
        if self.attached
            || self
                .previous_anchor
                .is_some_and(|id| !new.positions.contains_key(&id))
        {
            self.previous_anchor = old.zip(previous_index).and_then(|(p, i)| {
                p.entries[..i]
                    .iter()
                    .rev()
                    .find(|e| new.positions.contains_key(&e.id))
                    .map(|e| e.id)
            });
        }
        self.attached = false;
    }
    pub fn neighbor(&self, list: &PlaylistSnapshot, next: bool, wrap: bool) -> Option<EntryId> {
        if self.playlist != Some(list.summary.id) {
            return None;
        }
        if !self.attached {
            return (if next {
                self.next_anchor
            } else {
                self.previous_anchor
            })
            .filter(|id| list.positions.contains_key(id))
            .or_else(|| {
                wrap.then(|| {
                    if next {
                        list.entries.first()
                    } else {
                        list.entries.last()
                    }
                })
                .flatten()
                .map(|e| e.id)
            });
        }
        let index = *list.positions.get(&self.entry.id)?;
        let index = if next {
            index.checked_add(1).filter(|i| *i < list.entries.len())
        } else {
            index.checked_sub(1)
        }
        .or_else(|| {
            (wrap && list.entries.len() > 1).then(|| if next { 0 } else { list.entries.len() - 1 })
        })?;
        Some(list.entries[index].id)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionState {
    pub browse: Option<PlaylistId>,
    pub cursor: Option<PlaybackCursor>,
    pub frame: u64,
    pub sample_rate: u32,
    pub stamp: Option<FileStamp>,
}

/// A drop position refers to a gap in the original order. Removing selected
/// entries first must not change which gap the pointer named.
pub fn reordered(ids: &[EntryId], selected: &HashSet<EntryId>, gap: usize) -> Vec<EntryId> {
    let gap = gap.min(ids.len());
    let insert = ids[..gap]
        .iter()
        .filter(|id| !selected.contains(id))
        .count();
    let moving: Vec<_> = ids
        .iter()
        .copied()
        .filter(|id| selected.contains(id))
        .collect();
    let mut result: Vec<_> = ids
        .iter()
        .copied()
        .filter(|id| !selected.contains(id))
        .collect();
    result.splice(insert..insert, moving);
    result
}

#[cfg(unix)]
pub fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}
#[cfg(windows)]
pub fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}
#[cfg(unix)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "shared interface with fallible Windows UTF-16 decoding"
)]
pub fn decode_path(bytes: Vec<u8>) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStringExt;
    Ok(std::ffi::OsString::from_vec(bytes).into())
}
#[cfg(windows)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "shares the owned interface with zero-copy Unix path decoding"
)]
pub fn decode_path(bytes: Vec<u8>) -> Result<PathBuf, String> {
    use std::os::windows::ffi::OsStringExt;
    if !bytes.len().is_multiple_of(2) {
        return Err("Invalid Windows path encoding".into());
    }
    let wide: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .copied()
        .map(u16::from_le_bytes)
        .collect();
    Ok(std::ffi::OsString::from_wide(&wide).into())
}
pub mod native_path {
    use super::{Deserialize, Path, PathBuf, Serialize, decode_path, encode_path};
    pub fn serialize<S: serde::Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        encode_path(path).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<PathBuf, D::Error> {
        decode_path(Vec::<u8>::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_cursor_neighbors_obey_list_boundaries_and_repeat_all_wraps() {
        let list = list(&[1, 2, 3]);
        let middle = PlaybackCursor::new(list.summary.id, list.entries[1].clone());
        assert_eq!(middle.neighbor(&list, false, false), Some(EntryId(1)));
        assert_eq!(middle.neighbor(&list, true, false), Some(EntryId(3)));
        let first = PlaybackCursor::new(list.summary.id, list.entries[0].clone());
        let end_cursor = PlaybackCursor::new(list.summary.id, list.entries[2].clone());
        assert_eq!(first.neighbor(&list, false, false), None);
        assert_eq!(end_cursor.neighbor(&list, true, false), None);
        assert_eq!(first.neighbor(&list, false, true), Some(EntryId(3)));
        assert_eq!(end_cursor.neighbor(&list, true, true), Some(EntryId(1)));
    }
    #[cfg(windows)]
    #[test]
    fn windows_paths_preserve_unpaired_utf16_and_reject_incomplete_code_units() {
        use std::os::windows::ffi::OsStringExt;
        let path: PathBuf = std::ffi::OsString::from_wide(&[0xd800, 46, 97, 99, 52]).into();
        assert_eq!(decode_path(encode_path(&path)).unwrap(), path);
        assert!(decode_path(vec![0]).is_err());
    }
    fn list(ids: &[i64]) -> PlaylistSnapshot {
        PlaylistSnapshot::new(
            PlaylistSummary {
                id: PlaylistId(1),
                name: "A".into(),
                count: ids.len(),
                mode: PlaybackMode::Sequential,
            },
            ids.iter()
                .map(|id| Entry {
                    id: EntryId(*id),
                    media: MediaId(*id),
                    source: SelectedSource::from_path(format!("{id}.ac4").into()).unwrap(),
                    error: None,
                })
                .collect(),
            SavedBrowse::default(),
        )
    }
    #[test]
    fn deleting_current_and_successors_preserves_media_and_advances_anchor() {
        let a = list(&[1, 2, 3, 4]);
        let mut cursor = PlaybackCursor::new(PlaylistId(1), a.entries[1].clone());
        let b = list(&[1, 3, 4]);
        cursor.reconcile(Some(&a), Some(&b));
        assert!(!cursor.attached);
        assert_eq!(cursor.entry.id, EntryId(2));
        assert_eq!(cursor.neighbor(&b, true, false), Some(EntryId(3)));
        let c = list(&[4, 1]);
        cursor.reconcile(Some(&b), Some(&c));
        assert_eq!(cursor.neighbor(&c, true, false), Some(EntryId(4)));
        cursor.reconcile(Some(&c), None);
        assert_eq!(cursor.playlist, None);
    }
    #[test]
    fn multi_row_drop_keeps_relative_order_and_original_gap() {
        let ids: Vec<_> = (1..=6).map(EntryId).collect();
        let selected = HashSet::from([EntryId(2), EntryId(4)]);
        assert_eq!(
            reordered(&ids, &selected, 5),
            [1, 3, 5, 2, 4, 6].map(EntryId)
        );
    }
    #[test]
    fn selection_is_separate_from_playback_and_shift_uses_stable_anchor() {
        let a = list(&[1, 2, 3, 4]);
        let mut browse = BrowseState::default();
        browse.install(&a);
        browse.select(&a, EntryId(2), false, false);
        browse.select(&a, EntryId(4), false, true);
        assert_eq!(browse.ordered_selection(&a), [2, 3, 4].map(EntryId));
    }
}
