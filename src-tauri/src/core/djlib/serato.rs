//! Serato adapter: tracks from the database, grid/cues/colour from each file,
//! playlists from the stick's `_Serato_/Subcrates`.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Result};
use rusqlite::Connection;

use super::capabilities::{Capabilities, SERATO};
use super::convert::{grid_from_serato, markers_to_model, FileDj};
use super::endpoint::{DjEndpoint, ReadOutcome, WriteOutcome, CANCELLED};
use super::model::{DjLibrary, DjPlaylistNode, DjTrack, TempoSection};
use crate::core::{db, metadata, serato, stick_path};

/// Crate files in a Subcrates folder as (file name, stick-relative entry
/// paths), sorted by file name. A missing folder yields an empty list.
pub fn read_crate_files(subcrates: &Path) -> Vec<(String, Vec<String>)> {
    let Ok(entries) = std::fs::read_dir(subcrates) else { return Vec::new() };
    let mut out: Vec<(String, Vec<String>)> = entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("crate"))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let bytes = std::fs::read(e.path()).ok()?;
            Some((name, serato::parse_crate(&bytes)))
        })
        .collect();
    out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    out
}

/// All crates, or only those whose file name is listed (case-insensitive).
pub fn select_crates(
    all: Vec<(String, Vec<String>)>,
    only: Option<&[String]>,
) -> Vec<(String, Vec<String>)> {
    match only {
        None => all,
        Some(names) => all
            .into_iter()
            .filter(|(file, _)| names.iter().any(|n| n.eq_ignore_ascii_case(file)))
            .collect(),
    }
}

/// Every track id the given resolved crates reference.
pub fn crate_track_ids<S>(crates: &[(S, Vec<i64>)]) -> std::collections::HashSet<i64> {
    crates.iter().flat_map(|(_, ids)| ids.iter().copied()).collect()
}

/// Crates (name segments split on `%%`, track indices in crate order) →
/// playlist tree. A crate that also has sub-crates becomes a folder holding a
/// playlist of its own tracks first, then its children.
pub fn build_playlist_tree(crates: &[(Vec<String>, Vec<usize>)]) -> Vec<DjPlaylistNode> {
    fn level(items: &[(Vec<String>, Vec<usize>)], depth: usize) -> Vec<DjPlaylistNode> {
        let mut names: Vec<String> = Vec::new();
        for (segs, _) in items {
            if let Some(s) = segs.get(depth) {
                if !names.contains(s) {
                    names.push(s.clone());
                }
            }
        }
        names
            .into_iter()
            .map(|name| {
                let own = items
                    .iter()
                    .find(|(s, _)| s.len() == depth + 1 && s[depth] == name)
                    .map(|(_, t)| t.clone());
                let children: Vec<(Vec<String>, Vec<usize>)> = items
                    .iter()
                    .filter(|(s, _)| s.len() > depth + 1 && s[depth] == name)
                    .cloned()
                    .collect();
                if children.is_empty() {
                    return DjPlaylistNode::Playlist { name, track_refs: own.unwrap_or_default() };
                }
                let mut kids = Vec::new();
                if let Some(t) = own {
                    kids.push(DjPlaylistNode::Playlist { name: name.clone(), track_refs: t });
                }
                kids.extend(level(&children, depth + 1));
                DjPlaylistNode::Folder { name, children: kids }
            })
            .collect()
    }
    level(crates, 0)
}

/// Keeps whichever half read successfully: a broken markers frame must not
/// throw away a good grid, nor the reverse. Only both failing is an error.
pub(crate) fn combine_dj(
    grid: Result<Vec<TempoSection>>,
    markers: Result<FileDj>,
) -> Result<(Vec<TempoSection>, FileDj)> {
    match (grid, markers) {
        (Err(g), Err(_)) => Err(g),
        (g, m) => Ok((g.unwrap_or_default(), m.unwrap_or_default())),
    }
}

/// Grid and markers of one file. An `Err` means neither could be read
/// (missing, empty, broken tag); the caller counts it and keeps the track.
pub fn file_dj_data(path: &Path) -> Result<(Vec<TempoSection>, FileDj)> {
    let grid = metadata::read_serato_beatgrid(path)
        .map(|g| g.map(|g| grid_from_serato(&g)).unwrap_or_default());
    let dj = metadata::read_serato_markers(path)
        .map(|m| m.map(|m| markers_to_model(&m)).unwrap_or_default());
    combine_dj(grid, dj)
}

/// Runs a file read with lofty panics turned into errors: one malformed file
/// must cost one track's DJ data, never the whole export.
pub(crate) fn read_dj_guarded(
    path: &Path,
    read: impl FnOnce() -> Result<(Vec<TempoSection>, FileDj)>,
) -> Result<(Vec<TempoSection>, FileDj)> {
    metadata::guard_tag_panic(path, read)
}

fn track_from_row(r: &db::StickTrack) -> DjTrack {
    DjTrack {
        stick_rel_path: r.stick_rel_path.clone(),
        abs_path: r.abs_path.clone(),
        title: r.title.clone(),
        artist: r.artist.clone(),
        album: r.album.clone(),
        genre: r.genre.clone(),
        label: r.label.clone(),
        year: r.year,
        comment: r.comment.clone(),
        duration_secs: r.duration_secs,
        bitrate_kbps: r.bitrate_kbps,
        sample_rate: r.sample_rate,
        file_size: r.file_size,
        format: r.format.clone(),
        bpm: r.bpm,
        key_camelot: r.key_camelot.clone(),
        ..Default::default()
    }
}

pub struct SeratoEndpoint<'a> {
    pub conn: &'a Connection,
    /// Drive letter with colon, e.g. "D:".
    pub letter: String,
    /// Crate file names to export; None = every crate and every track.
    pub only_crates: Option<Vec<String>>,
}

impl DjEndpoint for SeratoEndpoint<'_> {
    fn capabilities(&self) -> &'static Capabilities {
        &SERATO
    }

    fn read(&self, progress: &dyn Fn(usize, usize), cancel: &AtomicBool) -> Result<ReadOutcome> {
        // Crates first: a per-crate export limits the collection to their tracks.
        let subcrates = PathBuf::from(format!("{}\\_Serato_\\Subcrates", self.letter));
        let mut dangling = 0usize;
        let mut resolved: Vec<(String, Vec<i64>)> = Vec::new();
        for (file, entries) in select_crates(read_crate_files(&subcrates), self.only_crates.as_deref()) {
            let (_, ids, missing) =
                stick_path::resolve_crate_entries(self.conn, &self.letter, entries.into_iter().map(Some));
            dangling += missing as usize;
            resolved.push((file, ids));
        }
        let keep = self.only_crates.as_ref().map(|_| crate_track_ids(&resolved));
        let rows: Vec<db::StickTrack> = db::stick_tracks(self.conn, &self.letter)?
            .into_iter()
            .filter(|r| keep.as_ref().is_none_or(|k| k.contains(&r.id)))
            .collect();
        let mut lib = DjLibrary::default();
        let mut index: HashMap<i64, usize> = HashMap::new();
        let mut unreadable = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            if cancel.load(Ordering::SeqCst) {
                bail!(CANCELLED);
            }
            let mut t = track_from_row(row);
            match read_dj_guarded(&row.abs_path, || file_dj_data(&row.abs_path)) {
                Ok((grid, dj)) => {
                    t.grid = grid;
                    t.cues = dj.cues;
                    t.loops = dj.loops;
                    t.colour = dj.colour;
                    t.grid_locked = dj.grid_locked;
                }
                Err(e) => {
                    tracing::warn!("dj sync: reading {}: {e:#}", row.abs_path.display());
                    unreadable.push(
                        row.abs_path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default(),
                    );
                }
            }
            index.insert(row.id, lib.tracks.len());
            lib.tracks.push(t);
            progress(i + 1, rows.len());
        }

        let mut crates = Vec::new();
        for (file, ids) in resolved {
            let refs: Vec<usize> = ids.iter().filter_map(|id| index.get(id).copied()).collect();
            let base = file.strip_suffix(".crate").unwrap_or(&file).to_string();
            crates.push((base.split("%%").map(str::to_string).collect(), refs));
        }
        lib.playlists = build_playlist_tree(&crates);
        Ok(ReadOutcome { library: lib, unreadable, dangling })
    }

    fn write(&self, _lib: &DjLibrary, _cancel: &AtomicBool) -> Result<WriteOutcome> {
        bail!("writing to Serato through the sync hub is sub-project 2")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(s: &str) -> Vec<String> {
        s.split("%%").map(str::to_string).collect()
    }

    #[test]
    fn flat_crates_become_playlists_in_order() {
        let t = build_playlist_tree(&[(segs("Deep House"), vec![2, 0]), (segs("Summer"), vec![1])]);
        assert_eq!(
            t,
            vec![
                DjPlaylistNode::Playlist { name: "Deep House".into(), track_refs: vec![2, 0] },
                DjPlaylistNode::Playlist { name: "Summer".into(), track_refs: vec![1] },
            ]
        );
    }

    #[test]
    fn crate_with_children_keeps_its_own_tracks() {
        let t = build_playlist_tree(&[(segs("A"), vec![0]), (segs("A%%B"), vec![1]), (segs("A%%C%%D"), vec![2])]);
        assert_eq!(
            t,
            vec![DjPlaylistNode::Folder {
                name: "A".into(),
                children: vec![
                    DjPlaylistNode::Playlist { name: "A".into(), track_refs: vec![0] },
                    DjPlaylistNode::Playlist { name: "B".into(), track_refs: vec![1] },
                    DjPlaylistNode::Folder {
                        name: "C".into(),
                        children: vec![DjPlaylistNode::Playlist { name: "D".into(), track_refs: vec![2] }],
                    },
                ],
            }]
        );
    }

    #[test]
    fn select_crates_keeps_everything_or_only_the_named_ones() {
        let all = vec![
            ("Deep House.crate".to_string(), vec!["a".to_string()]),
            ("Summer.crate".to_string(), vec!["b".to_string()]),
        ];
        assert_eq!(select_crates(all.clone(), None).len(), 2);
        let only = vec!["deep house.crate".to_string()];
        let picked = select_crates(all, Some(&only));
        assert_eq!(picked.len(), 1, "matched case-insensitively");
        assert_eq!(picked[0].0, "Deep House.crate");
    }

    #[test]
    fn a_crate_export_keeps_only_that_crates_tracks() {
        let crates = vec![("A.crate".to_string(), vec![3, 1]), ("B.crate".to_string(), vec![1, 7])];
        let keep = crate_track_ids(&crates);
        let mut ids: Vec<i64> = keep.into_iter().collect();
        ids.sort();
        assert_eq!(ids, vec![1, 3, 7]);
    }

    #[test]
    fn missing_subcrates_dir_yields_no_crates() {
        let dir = std::env::temp_dir().join(format!("spindex-no-subcrates-{}", std::process::id()));
        assert!(read_crate_files(&dir.join("does-not-exist")).is_empty());
    }

    #[test]
    fn a_good_grid_survives_broken_markers_and_vice_versa() {
        let grid = vec![TempoSection { start_sec: 0.1, bpm: 128.0, beat_in_bar: 1 }];
        let (g, dj) = combine_dj(Ok(grid.clone()), Err(anyhow::anyhow!("bad markers"))).unwrap();
        assert_eq!(g, grid);
        assert_eq!(dj, FileDj::default());
        let only_markers = FileDj { grid_locked: true, ..Default::default() };
        let (g, dj) = combine_dj(Err(anyhow::anyhow!("bad grid")), Ok(only_markers.clone())).unwrap();
        assert!(g.is_empty());
        assert_eq!(dj, only_markers);
        assert!(combine_dj(Err(anyhow::anyhow!("a")), Err(anyhow::anyhow!("b"))).is_err());
    }

    #[test]
    fn a_panicking_file_read_is_an_error_not_a_crash() {
        let r = read_dj_guarded(Path::new("x.mp3"), || -> Result<(Vec<TempoSection>, FileDj)> {
            panic!("lofty blew up")
        });
        assert!(r.is_err());
    }

    #[test]
    fn empty_file_is_unreadable_not_fatal() {
        let dir = std::env::temp_dir().join(format!("spindex-empty-dj-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("empty.aiff");
        std::fs::write(&p, b"").unwrap();
        assert!(file_dj_data(&p).is_err(), "the caller counts it and moves on");
    }
}
