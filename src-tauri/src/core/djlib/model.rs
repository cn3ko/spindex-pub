//! The normalized model every endpoint reads into and writes from. It exists
//! only for the duration of a sync; nothing here is persisted.
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// One constant-tempo stretch of a 4/4 grid. A steady grid is one section.
#[derive(Debug, Clone, PartialEq)]
pub struct TempoSection {
    pub start_sec: f64,
    pub bpm: f64,
    /// 1..=4: which beat of the bar falls on `start_sec`.
    pub beat_in_bar: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CueKind {
    Hot { slot: u8 },
    Memory,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub kind: CueKind,
    pub pos_sec: f64,
    pub colour: Option<Rgb>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Loop {
    /// None = memory loop (never occupies a hot-cue slot).
    pub slot: Option<u8>,
    pub start_sec: f64,
    pub end_sec: f64,
    pub colour: Option<Rgb>,
    pub name: String,
    pub locked: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DjTrack {
    /// Path relative to the stick root with forward slashes ("MP3/x.mp3"):
    /// the identity across programs, immune to drive-letter changes.
    pub stick_rel_path: String,
    pub abs_path: PathBuf,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub label: Option<String>,
    pub year: Option<i64>,
    pub comment: Option<String>,
    pub duration_secs: Option<f64>,
    pub bitrate_kbps: Option<i64>,
    pub sample_rate: Option<i64>,
    pub file_size: i64,
    pub format: Option<String>,
    pub bpm: Option<f64>,
    pub key_camelot: Option<String>,
    pub colour: Option<Rgb>,
    pub grid_locked: bool,
    pub grid: Vec<TempoSection>,
    pub cues: Vec<Cue>,
    pub loops: Vec<Loop>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DjPlaylistNode {
    Folder { name: String, children: Vec<DjPlaylistNode> },
    /// `track_refs` are indices into `DjLibrary::tracks`, in playlist order.
    Playlist { name: String, track_refs: Vec<usize> },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DjLibrary {
    pub tracks: Vec<DjTrack>,
    pub playlists: Vec<DjPlaylistNode>,
}

impl DjLibrary {
    /// Number of Playlist nodes anywhere in the tree.
    pub fn playlist_count(&self) -> usize {
        fn count(nodes: &[DjPlaylistNode]) -> usize {
            nodes
                .iter()
                .map(|n| match n {
                    DjPlaylistNode::Folder { children, .. } => count(children),
                    DjPlaylistNode::Playlist { .. } => 1,
                })
                .sum()
        }
        count(&self.playlists)
    }
}
