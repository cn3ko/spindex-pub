//! Rekordbox → Serato import (DJ sync hub SP2). Pure: deciding what to write
//! never touches a file, so the preview and the import work from one plan.
//! The rule throughout is "only fill gaps": nothing Serato already has is
//! changed. Spec: docs/superpowers/specs/2026-09-30-rekordbox-to-serato-design.md

use super::model::{CueKind, DjLibrary, DjPlaylistNode, DjTrack, Rgb, TempoSection};
use crate::core::serato_beatgrid::{Beatgrid, Marker};
pub use crate::core::serato_markers::Entry;
use crate::core::serato_markers::{colour_body, cue_body, loop_body, SeratoCue, SeratoLoop};

/// Serato's hot cue red, used when a Rekordbox hot cue carries no colour.
const DEFAULT_CUE_RGB: [u8; 3] = [0xcc, 0x00, 0x00];
/// Serato's saved-loop blue, used when a Rekordbox loop carries no colour.
const DEFAULT_LOOP_RGB: [u8; 3] = [0x27, 0xaa, 0xe1];
const SLOTS: u8 = 8;

/// What filling one file's gaps adds, and what it had to leave out.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct FillCounts {
    pub cues: usize,
    pub loops: usize,
    pub colour: bool,
    /// Rekordbox hot cues / loops whose Serato slot was already taken.
    pub cues_skipped: usize,
    pub loops_skipped: usize,
    /// Rekordbox memory cues: Serato has no memory cues.
    pub memory_cues_dropped: usize,
}

impl FillCounts {
    /// Whether filling adds anything at all (only then is the file written).
    pub fn anything(&self) -> bool {
        self.cues > 0 || self.loops > 0 || self.colour
    }
}

fn ms(sec: f64) -> u32 {
    (sec * 1000.0).round().clamp(0.0, u32::MAX as f64) as u32
}

fn rgb_or(c: Option<Rgb>, fallback: [u8; 3]) -> [u8; 3] {
    c.map(|c| [c.r, c.g, c.b]).unwrap_or(fallback)
}

/// A CUE or LOOP body's slot, or None when the body is too short to tell.
fn slot_of(body: &[u8]) -> Option<u8> {
    body.get(1).copied()
}

/// The slots `kind` entries occupy. An entry too short to show its slot could
/// be holding any of them, so then every slot counts as taken: nothing new is
/// added beside a cue or loop we cannot read.
fn taken_slots(entries: &[Entry], kind: &str) -> Vec<u8> {
    let bodies: Vec<&Vec<u8>> = entries.iter().filter(|(n, _)| n == kind).map(|(_, b)| b).collect();
    if bodies.iter().any(|b| slot_of(b).is_none()) {
        return (0..SLOTS).collect();
    }
    bodies.iter().filter_map(|b| slot_of(b)).collect()
}

/// Fills the gaps of one file's Markers2 entries with a Rekordbox track's hot
/// cues, loops and colour. Existing entries keep their bytes and their order;
/// new cues go after the last existing cue (else after COLOR, else first), new
/// loops after the last loop (else after the last cue or COLOR). An occupied
/// slot is never overwritten.
pub fn fill_markers(existing: &[Entry], t: &DjTrack) -> (Vec<Entry>, FillCounts) {
    let mut out: Vec<Entry> = existing.to_vec();
    let mut c = FillCounts::default();

    // colour: only where Serato has none (no COLOR entry, or its "none" white)
    let colour_at = out.iter().position(|(n, _)| n == "COLOR");
    let has_colour = colour_at
        .map(|i| out[i].1.get(1..4).is_some_and(|rgb| rgb != [0xff, 0xff, 0xff]))
        .unwrap_or(false);
    if let (false, Some(col)) = (has_colour, t.colour) {
        let body = colour_body([col.r, col.g, col.b]);
        match colour_at {
            Some(i) => out[i].1 = body,
            None => out.insert(0, ("COLOR".into(), body)),
        }
        c.colour = true;
    }

    // cues
    let mut cue_slots = taken_slots(&out, "CUE");
    let mut hot: Vec<_> = t.cues.iter().collect();
    hot.sort_by_key(|q| match q.kind {
        CueKind::Hot { slot } => slot,
        CueKind::Memory => u8::MAX,
    });
    for q in hot {
        let CueKind::Hot { slot } = q.kind else {
            c.memory_cues_dropped += 1;
            continue;
        };
        if slot >= SLOTS || cue_slots.contains(&slot) {
            c.cues_skipped += 1;
            continue;
        }
        let body = cue_body(&SeratoCue {
            slot,
            pos_ms: ms(q.pos_sec),
            rgb: rgb_or(q.colour, DEFAULT_CUE_RGB),
            name: q.name.clone(),
        });
        let at = out
            .iter()
            .rposition(|(n, _)| n == "CUE")
            .or_else(|| out.iter().position(|(n, _)| n == "COLOR"))
            .map(|i| i + 1)
            .unwrap_or(0);
        out.insert(at, ("CUE".into(), body));
        cue_slots.push(slot);
        c.cues += 1;
    }

    // loops: hot loops keep their slot, memory loops take the first free one
    let mut loop_slots = taken_slots(&out, "LOOP");
    // A loop already in the file (same start and end within a few ms) is not
    // added again: a memory loop has no slot of its own, so without this a
    // second import would put a second copy into the next free slot.
    let mut spans: Vec<(u32, u32)> = out
        .iter()
        .filter(|(n, _)| n == "LOOP")
        .filter_map(|(_, b)| crate::core::serato_markers::parse_loop(b).ok())
        .map(|l| (l.start_ms, l.end_ms))
        .collect();
    let near = |a: u32, b: u32| a.abs_diff(b) <= 5;
    let mut loops: Vec<_> = t.loops.iter().collect();
    loops.sort_by_key(|l| l.slot.unwrap_or(u8::MAX)); // hot loops claim their slots first
    for l in loops {
        let (start_ms, end_ms) = (ms(l.start_sec), ms(l.end_sec));
        if spans.iter().any(|&(s, e)| near(s, start_ms) && near(e, end_ms)) {
            continue; // already there: neither added nor a loss
        }
        let slot = match l.slot {
            Some(s) if s < SLOTS && !loop_slots.contains(&s) => Some(s),
            Some(_) => None,
            None => (0..SLOTS).find(|s| !loop_slots.contains(s)),
        };
        let Some(slot) = slot else {
            c.loops_skipped += 1;
            continue;
        };
        spans.push((start_ms, end_ms));
        let body = loop_body(&SeratoLoop {
            slot,
            start_ms,
            end_ms,
            rgb: rgb_or(l.colour, DEFAULT_LOOP_RGB),
            locked: l.locked,
            name: l.name.clone(),
        });
        let at = out
            .iter()
            .rposition(|(n, _)| n == "LOOP")
            .or_else(|| out.iter().rposition(|(n, _)| n == "CUE"))
            .or_else(|| out.iter().position(|(n, _)| n == "COLOR"))
            .map(|i| i + 1)
            .unwrap_or(0);
        out.insert(at, ("LOOP".into(), body));
        loop_slots.push(slot);
        c.loops += 1;
    }
    (out, c)
}

/// Rekordbox tempo sections → a Serato grid. One section is a constant grid
/// anchored on bar 1 (`Battito` moves the anchor back, possibly below 0 s,
/// which Serato accepts); several become a dynamic grid with one marker per
/// section and the last section as the terminal marker.
pub fn grid_to_serato(sections: &[TempoSection]) -> Option<Beatgrid> {
    let usable: Vec<&TempoSection> =
        sections.iter().filter(|s| s.bpm.is_finite() && s.bpm > 0.0 && s.start_sec.is_finite()).collect();
    // A section shorter than half a beat holds no whole beat, so it cannot be
    // a Serato marker: drop it, and the section before counts its beats up to
    // the next kept one instead of stopping at a marker that never appears.
    let valid: Vec<&TempoSection> = usable
        .iter()
        .enumerate()
        .filter(|(i, s)| usable.get(i + 1).is_none_or(|n| (n.start_sec - s.start_sec) * s.bpm / 60.0 >= 0.5))
        .map(|(_, s)| *s)
        .collect();
    let first = *valid.first()?;
    let bar_one = |s: &TempoSection| s.start_sec - (s.beat_in_bar.saturating_sub(1)) as f64 * 60.0 / s.bpm;
    if valid.len() == 1 {
        return Some(Beatgrid::Constant { anchor_sec: bar_one(first) as f32, bpm: first.bpm as f32 });
    }
    let mut markers = Vec::new();
    for (i, s) in valid[..valid.len() - 1].iter().enumerate() {
        let next = valid[i + 1].start_sec;
        // the first marker sits on bar 1; the beats it moved back are counted
        let (pos, extra) = if i == 0 {
            (bar_one(s), s.beat_in_bar.saturating_sub(1) as f64)
        } else {
            (s.start_sec, 0.0)
        };
        let beats = ((next - s.start_sec) * s.bpm / 60.0 + extra).round();
        if beats >= 1.0 {
            markers.push(Marker { pos_sec: pos as f32, beats_to_next: beats as u32 });
        }
    }
    let last = valid[valid.len() - 1];
    if markers.is_empty() {
        return Some(Beatgrid::Constant { anchor_sec: bar_one(last) as f32, bpm: last.bpm as f32 });
    }
    Some(Beatgrid::Dynamic {
        markers,
        terminal_sec: last.start_sec as f32,
        terminal_bpm: last.bpm as f32,
    })
}

// ---------------------------------------------------------------------
// Matching Rekordbox tracks to the stick, and the plan
// ---------------------------------------------------------------------

/// An indexed track on the target stick.
#[derive(Debug, Clone, PartialEq)]
pub struct StickTrack {
    pub id: i64,
    /// Stick-relative, forward slashes ("Contents/A/x.mp3").
    pub stick_rel: String,
    pub size: i64,
}

fn file_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// For every XML track, the index of its stick track: the same path when the
/// XML location is on this stick's letter, otherwise the one stick file with
/// the same name (case-insensitive) and exactly the same size. Rekordbox's
/// XML usually points at the computer while the stick holds its exported
/// copies — same name, same bytes. Ambiguity is never guessed.
pub fn match_tracks(lib: &DjLibrary, letter: &str, stick: &[StickTrack]) -> Vec<Option<usize>> {
    let on_stick = format!("{}\\", letter.to_ascii_lowercase());
    lib.tracks
        .iter()
        .map(|t| {
            let abs = t.abs_path.to_string_lossy().to_ascii_lowercase();
            if abs.starts_with(&on_stick) {
                if let Some(i) = stick.iter().position(|s| s.stick_rel.eq_ignore_ascii_case(&t.stick_rel_path)) {
                    return Some(i);
                }
            }
            // a location off any Windows drive (a Mac export) has no stick
            // path, only its file name to go by
            let fallback = t.abs_path.file_name().map(|n| n.to_string_lossy().into_owned());
            let name = match file_name(&t.stick_rel_path) {
                "" => fallback.as_deref().unwrap_or(""),
                n => n,
            };
            if name.is_empty() {
                return None;
            }
            let mut hits = stick
                .iter()
                .enumerate()
                .filter(|(_, s)| s.size == t.file_size && file_name(&s.stick_rel).eq_ignore_ascii_case(name));
            match (hits.next(), hits.next()) {
                (Some((i, _)), None) => Some(i),
                _ => None,
            }
        })
        .collect()
}

/// One crate the import creates or adds links to.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CratePlan {
    /// File name inside `_Serato_/Subcrates` ("House%%Deep.crate").
    pub file: String,
    pub exists: bool,
    /// Stick-relative paths to link, in playlist order, none already linked.
    pub add: Vec<String>,
}

/// A playlist or folder name made safe for a crate file name: Serato uses
/// `%%` to nest, and Windows forbids `\/:*?"<>|`.
pub fn crate_segment(name: &str) -> String {
    name.chars()
        .map(|c| if "%\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Crates for the playlist tree: nested playlists become `Parent%%Child`
/// crates, and the parent crate files are created (empty) when missing, since
/// Serato keeps a crate file for every level. A playlist whose name matches an
/// existing crate (case-insensitive) gets only its missing links; a new crate
/// with nothing matched is not created.
pub fn plan_crates(
    lib: &DjLibrary,
    matches: &[Option<usize>],
    stick: &[StickTrack],
    existing: &[(String, Vec<String>)],
) -> Vec<CratePlan> {
    let mut out: Vec<CratePlan> = Vec::new();
    for n in &lib.playlists {
        walk(n, &[], lib, matches, stick, existing, &mut out);
    }
    out
}

fn existing_crate<'a>(existing: &'a [(String, Vec<String>)], file: &str) -> Option<&'a (String, Vec<String>)> {
    existing.iter().find(|(f, _)| f.eq_ignore_ascii_case(file))
}

fn walk(
    node: &DjPlaylistNode,
    parents: &[String],
    lib: &DjLibrary,
    matches: &[Option<usize>],
    stick: &[StickTrack],
    existing: &[(String, Vec<String>)],
    out: &mut Vec<CratePlan>,
) {
    match node {
        DjPlaylistNode::Folder { name, children } => {
            let mut path = parents.to_vec();
            path.push(crate_segment(name));
            for c in children {
                walk(c, &path, lib, matches, stick, existing, out);
            }
        }
        DjPlaylistNode::Playlist { name, track_refs } => {
            let mut path = parents.to_vec();
            path.push(crate_segment(name));
            let file = format!("{}.crate", path.join("%%"));
            let found = existing_crate(existing, &file);
            let linked: Vec<String> =
                found.map(|(_, p)| p.iter().map(|x| x.to_lowercase()).collect()).unwrap_or_default();
            let mut add: Vec<String> = Vec::new();
            for &r in track_refs {
                let Some(Some(si)) = matches.get(r) else { continue };
                let p = &stick[*si].stick_rel;
                let lower = p.to_lowercase();
                if !linked.contains(&lower) && !add.iter().any(|a| a.to_lowercase() == lower) {
                    add.push(p.clone());
                }
            }
            if found.is_none() && add.is_empty() {
                return; // nothing of it is on this stick
            }
            // parent crate files, outermost first, when neither present nor planned
            for depth in 1..path.len() {
                let parent = format!("{}.crate", path[..depth].join("%%"));
                let planned = out.iter().any(|c| c.file.eq_ignore_ascii_case(&parent));
                if !planned && existing_crate(existing, &parent).is_none() {
                    out.push(CratePlan { file: parent, exists: false, add: vec![] });
                }
            }
            if let Some(same) = out.iter_mut().find(|c| c.file.eq_ignore_ascii_case(&file)) {
                // two playlists naming the same crate: merge their links
                for a in add {
                    if !same.add.iter().any(|x| x.eq_ignore_ascii_case(&a)) {
                        same.add.push(a);
                    }
                }
                return;
            }
            out.push(CratePlan {
                file: found.map(|(f, _)| f.clone()).unwrap_or(file),
                exists: found.is_some(),
                add,
            });
        }
    }
}

/// What a file holds on the Serato side before the import.
#[derive(Debug, Clone, PartialEq)]
pub enum FileState {
    Readable { has_grid: bool, markers: Option<Vec<Entry>> },
    /// The grid or the Markers2 data could not be read: nothing is written.
    Unreadable,
    /// A format whose Serato data lives outside the tag SpinDex writes (m4a,
    /// aac, mp4 keep it in an atom): never read, never written.
    Unsupported,
}

/// What the import writes into one file.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackEdit {
    pub grid: Option<Beatgrid>,
    pub markers: Option<Vec<Entry>>,
    pub counts: FillCounts,
}

impl TrackEdit {
    pub fn writes_anything(&self) -> bool {
        self.grid.is_some() || self.markers.is_some()
    }
}

/// The edit for one matched track, or None when its Serato data cannot be
/// read (gaps cannot be filled blind).
pub fn plan_track(t: &DjTrack, state: &FileState) -> Option<TrackEdit> {
    let FileState::Readable { has_grid, markers } = state else { return None };
    let grid = if *has_grid { None } else { grid_to_serato(&t.grid) };
    let existing = markers.clone().unwrap_or_default();
    let (entries, counts) = fill_markers(&existing, t);
    Some(TrackEdit { grid, markers: counts.anything().then_some(entries), counts })
}

// ---------------------------------------------------------------------
// The whole plan for one stick: what the preview shows and the import does
// ---------------------------------------------------------------------

/// Everything the dialog shows before anything is written.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct ImportPreview {
    pub tracks_in_xml: usize,
    pub matched: usize,
    pub not_on_stick: usize,
    /// Matched files whose Serato grid or cue data could not be read.
    pub unreadable: usize,
    /// Matched files in a format SpinDex cannot write Serato data into (m4a, …).
    pub unsupported: usize,
    pub grids: usize,
    pub cues: usize,
    pub loops: usize,
    pub colours: usize,
    pub cues_skipped: usize,
    pub loops_skipped: usize,
    pub memory_cues_dropped: usize,
    /// Crate names as Serato shows them ("House / Deep").
    pub crates_new: Vec<String>,
    pub crates_merged: Vec<String>,
    pub links_added: usize,
    /// Playlist entries whose track is not on this stick: those crates come
    /// out shorter than the Rekordbox playlist.
    pub playlist_entries_missing: usize,
    /// Bytes the import needs on the stick: first-touch backups of the files
    /// it writes, plus one temporary copy while a file is saved.
    pub space_needed: u64,
    /// Bytes free on the stick; None when the drive could not be asked.
    pub free_space: Option<u64>,
}

impl ImportPreview {
    /// Whether the stick has room (unknown free space does not block).
    pub fn fits(&self) -> bool {
        self.free_space.is_none_or(|f| self.space_needed <= f)
    }
}

/// Space an import needs: every file still without a first-touch backup is
/// copied once, and saving works on one temporary copy at a time, as large as
/// the largest file written. `files` = (size, backup already exists).
pub fn space_needed(files: &[(u64, bool)]) -> u64 {
    let backups: u64 = files.iter().filter(|(_, has)| !has).map(|(s, _)| s).sum();
    let largest = files.iter().map(|(s, _)| *s).max().unwrap_or(0);
    backups + largest
}

/// One file the import writes.
#[derive(Debug, Clone)]
pub struct PreparedTrack {
    pub track_id: i64,
    pub abs: std::path::PathBuf,
    pub edit: TrackEdit,
}

#[derive(Debug, Clone)]
pub struct Prepared {
    pub preview: ImportPreview,
    pub tracks: Vec<PreparedTrack>,
    pub crates: Vec<CratePlan>,
}

fn crate_label(file: &str) -> String {
    file.strip_suffix(".crate").unwrap_or(file).replace("%%", " / ")
}

/// The Serato side of one file, read without changing anything.
fn file_state(path: &std::path::Path) -> FileState {
    use crate::core::metadata;
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !matches!(ext.as_str(), "mp3" | "wav" | "aif" | "aiff" | "flac") {
        return FileState::Unsupported;
    }
    let has_grid = match metadata::read_serato_beatgrid(path) {
        Ok(g) => g.is_some(),
        Err(_) => return FileState::Unreadable,
    };
    match metadata::read_serato_markers_entries(path) {
        Ok(markers) => FileState::Readable { has_grid, markers },
        Err(_) => FileState::Unreadable,
    }
}

/// Builds the import for `letter` from a Rekordbox XML: reads the XML, the
/// stick's indexed tracks, each matched file's Serato data and the stick's
/// crates, and decides every write. Reads only.
pub fn prepare(conn: &rusqlite::Connection, letter: &str, xml: &str) -> anyhow::Result<Prepared> {
    prepare_cancellable(conn, letter, xml, &|| false)?.ok_or_else(|| anyhow::anyhow!("cancelled"))
}

/// `prepare`, checking `cancelled` before every file it reads (a stick with
/// hundreds of matched files takes a while). `Ok(None)` = cancelled.
pub fn prepare_cancellable(
    conn: &rusqlite::Connection,
    letter: &str,
    xml: &str,
    cancelled: &dyn Fn() -> bool,
) -> anyhow::Result<Option<Prepared>> {
    let lib = crate::core::rekordbox_xml::read_library(xml)?;
    let rows = crate::core::db::stick_tracks(conn, letter)?;
    let stick: Vec<StickTrack> = rows
        .iter()
        .map(|r| StickTrack { id: r.id, stick_rel: r.stick_rel_path.clone(), size: r.file_size })
        .collect();
    let matches = match_tracks(&lib, letter, &stick);

    let mut preview = ImportPreview { tracks_in_xml: lib.tracks.len(), ..Default::default() };
    let mut tracks = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (i, m) in matches.iter().enumerate() {
        if cancelled() {
            return Ok(None);
        }
        let Some(si) = *m else {
            preview.not_on_stick += 1;
            continue;
        };
        preview.matched += 1;
        if !seen.insert(si) {
            continue; // two XML entries for one file: the first decides
        }
        let abs = rows[si].abs_path.clone();
        let state = file_state(&abs);
        let Some(edit) = plan_track(&lib.tracks[i], &state) else {
            if state == FileState::Unsupported {
                preview.unsupported += 1;
            } else {
                preview.unreadable += 1;
            }
            continue;
        };
        let c = edit.counts;
        preview.grids += edit.grid.is_some() as usize;
        preview.cues += c.cues;
        preview.loops += c.loops;
        preview.colours += c.colour as usize;
        preview.cues_skipped += c.cues_skipped;
        preview.loops_skipped += c.loops_skipped;
        preview.memory_cues_dropped += c.memory_cues_dropped;
        if edit.writes_anything() {
            tracks.push(PreparedTrack { track_id: rows[si].id, abs, edit });
        }
    }

    let files: Vec<(u64, bool)> = tracks
        .iter()
        .map(|t| {
            let size = std::fs::metadata(&t.abs).map(|m| m.len()).unwrap_or(0);
            let has_backup = crate::core::db::track_root_and_abs_path(conn, t.track_id)
                .ok()
                .and_then(|(root, abs)| {
                    let rel = abs.strip_prefix(&root).ok()?.to_string_lossy().replace('\\', "/");
                    Some(crate::core::enhance::backup_path_for(&root, &rel).exists())
                })
                .unwrap_or(false);
            (size, has_backup)
        })
        .collect();
    preview.space_needed = space_needed(&files);
    preview.free_space = crate::core::devices::free_space(letter);

    let subcrates = std::path::PathBuf::from(format!("{letter}\\_Serato_\\Subcrates"));
    let existing = super::serato::read_crate_files(&subcrates);
    let crates = plan_crates(&lib, &matches, &stick, &existing);
    for c in &crates {
        if c.exists {
            if !c.add.is_empty() {
                preview.crates_merged.push(crate_label(&c.file));
            }
        } else {
            preview.crates_new.push(crate_label(&c.file));
        }
        preview.links_added += c.add.len();
    }
    preview.playlist_entries_missing = playlist_refs(&lib.playlists)
        .into_iter()
        .filter(|&r| matches.get(r).is_none_or(|m| m.is_none()))
        .count();
    Ok(Some(Prepared { preview, tracks, crates }))
}

/// Every playlist entry in the tree, as indices into the XML's tracks.
fn playlist_refs(nodes: &[DjPlaylistNode]) -> Vec<usize> {
    let mut out = Vec::new();
    for n in nodes {
        match n {
            DjPlaylistNode::Folder { children, .. } => out.extend(playlist_refs(children)),
            DjPlaylistNode::Playlist { track_refs, .. } => out.extend(track_refs),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::djlib::model::{Cue, CueKind, DjLibrary, DjPlaylistNode, DjTrack, Loop, Rgb, TempoSection};
    use crate::core::serato_beatgrid::{Beatgrid, Marker};
    use crate::core::serato_markers::{colour_body, cue_body, parse_cue, SeratoCue};

    fn hot(slot: u8, sec: f64) -> Cue {
        Cue { kind: CueKind::Hot { slot }, pos_sec: sec, colour: None, name: String::new() }
    }

    fn track() -> DjTrack {
        DjTrack {
            cues: vec![
                hot(0, 1.0),
                Cue { kind: CueKind::Hot { slot: 1 }, pos_sec: 60.5, colour: Some(Rgb { r: 40, g: 226, b: 20 }), name: "Drop".into() },
                Cue { kind: CueKind::Memory, pos_sec: 0.1, colour: None, name: String::new() },
            ],
            loops: vec![
                Loop { slot: Some(0), start_sec: 30.0, end_sec: 31.935, colour: None, name: String::new(), locked: false },
                Loop { slot: None, start_sec: 90.0, end_sec: 92.0, colour: None, name: String::new(), locked: false },
            ],
            colour: Some(Rgb { r: 0xff, g: 0, b: 0 }),
            ..Default::default()
        }
    }

    fn names(e: &[Entry]) -> Vec<&str> {
        e.iter().map(|(n, _)| n.as_str()).collect()
    }

    #[test]
    fn an_empty_file_gets_everything() {
        let (out, c) = fill_markers(&[], &track());
        assert_eq!(names(&out), vec!["COLOR", "CUE", "CUE", "LOOP", "LOOP"]);
        assert_eq!((c.cues, c.loops, c.colour, c.memory_cues_dropped), (2, 2, true, 1));
        let cue = parse_cue(&out[1].1).unwrap();
        assert_eq!((cue.slot, cue.pos_ms, cue.rgb), (0, 1000, [0xcc, 0, 0]), "no colour -> Serato red");
        let drop = parse_cue(&out[2].1).unwrap();
        assert_eq!((drop.slot, drop.rgb, drop.name.as_str()), (1, [40, 226, 20], "Drop"));
    }

    #[test]
    fn occupied_slots_and_existing_entries_stay_untouched() {
        let theirs = cue_body(&SeratoCue { slot: 1, pos_ms: 7, rgb: [1, 2, 3], name: "Mine".into() });
        let existing: Vec<Entry> = vec![
            ("COLOR".into(), colour_body([0, 0, 0xcc])),
            ("CUE".into(), theirs.clone()),
            ("FLIP".into(), vec![9, 9, 9]),
            ("BPMLOCK".into(), vec![0]),
        ];
        let (out, c) = fill_markers(&existing, &track());
        assert_eq!(c.cues, 1, "only slot 0 was free");
        assert_eq!(c.cues_skipped, 1);
        assert!(!c.colour, "the track already had a colour");
        assert_eq!(out[0], existing[0], "COLOR kept");
        let cue_bodies: Vec<&Vec<u8>> = out.iter().filter(|(n, _)| n == "CUE").map(|(_, b)| b).collect();
        assert!(cue_bodies.contains(&&theirs), "Serato's own cue kept byte for byte");
        assert!(out.contains(&("FLIP".into(), vec![9, 9, 9])));
        assert_eq!(out.last(), Some(&("BPMLOCK".to_string(), vec![0])), "order after the inserts kept");
    }

    #[test]
    fn white_colour_counts_as_none_and_memory_loops_take_a_free_slot() {
        let existing: Vec<Entry> = vec![("COLOR".into(), colour_body([0xff, 0xff, 0xff]))];
        let (out, c) = fill_markers(&existing, &track());
        assert!(c.colour);
        assert_eq!(out[0].1, colour_body([0xff, 0, 0]));
        let slots: Vec<u8> = out.iter().filter(|(n, _)| n == "LOOP").map(|(_, b)| b[1]).collect();
        assert_eq!(slots, vec![0, 1], "hot loop at 0, memory loop into the next free slot");
    }

    #[test]
    fn space_counts_new_backups_plus_one_working_copy() {
        // two files still need a first-touch backup, one already has one;
        // saving works on one temp copy at a time, as big as the largest file
        assert_eq!(space_needed(&[(100, false), (300, true), (50, false)]), 100 + 50 + 300);
        assert_eq!(space_needed(&[]), 0);
    }

    #[test]
    fn formats_serato_keeps_in_atoms_are_left_out_up_front() {
        for f in ["x.m4a", "x.AAC", "x.mp4", "x"] {
            assert_eq!(file_state(std::path::Path::new(f)), FileState::Unsupported, "{f}");
        }
        let t = DjTrack { cues: vec![hot(0, 1.0)], ..Default::default() };
        assert!(plan_track(&t, &FileState::Unsupported).is_none());
    }

    #[test]
    fn importing_twice_adds_nothing_the_second_time() {
        let (once, c1) = fill_markers(&[], &track());
        assert!(c1.anything());
        let (twice, c2) = fill_markers(&once, &track());
        assert_eq!(twice, once, "a second run must not add a second copy of any loop");
        assert!(!c2.anything());
    }

    #[test]
    fn nothing_to_add_leaves_the_entries_equal() {
        let t = DjTrack::default();
        let existing: Vec<Entry> = vec![("BPMLOCK".into(), vec![1])];
        let (out, c) = fill_markers(&existing, &t);
        assert_eq!(out, existing);
        assert!(!c.anything());
    }

    #[test]
    fn one_tempo_becomes_a_constant_grid_on_bar_one() {
        let g = grid_to_serato(&[TempoSection { start_sec: 0.12, bpm: 124.0, beat_in_bar: 2 }]).unwrap();
        let Beatgrid::Constant { anchor_sec, bpm } = g else { panic!("constant expected") };
        assert!((anchor_sec as f64 - (0.12 - 60.0 / 124.0)).abs() < 1e-4, "{anchor_sec}");
        assert_eq!(bpm, 124.0);
        assert!(grid_to_serato(&[]).is_none());
    }

    fn st(id: i64, rel: &str, size: i64) -> StickTrack {
        StickTrack { id, stick_rel: rel.into(), size }
    }

    fn xml_track(abs: &str, rel: &str, size: i64) -> DjTrack {
        DjTrack { abs_path: abs.into(), stick_rel_path: rel.into(), file_size: size, ..Default::default() }
    }

    #[test]
    fn matches_by_path_on_the_stick_then_by_name_and_size() {
        let stick = vec![st(1, "Contents/A/one.mp3", 100), st(2, "Library/two.wav", 200)];
        let lib = DjLibrary {
            tracks: vec![
                xml_track(r"D:\Contents\A\one.mp3", "Contents/A/one.mp3", 999), // path wins, size ignored
                xml_track(r"C:\Users\dj\Music\TWO.wav", "Users/dj/Music/TWO.wav", 200),
                xml_track(r"C:\Users\dj\Music\two.wav", "Users/dj/Music/two.wav", 201), // size differs
                xml_track(r"E:\x\one.mp3", "x/one.mp3", 5),
            ],
            playlists: vec![],
        };
        assert_eq!(match_tracks(&lib, "D:", &stick), vec![Some(0), Some(1), None, None]);
    }

    #[test]
    fn a_mac_export_matches_by_name_and_size() {
        let stick = vec![st(1, "Library/two.wav", 200)];
        let lib = DjLibrary {
            tracks: vec![xml_track("/Users/dj/Music/Two.wav", "", 200), xml_track("/Users/dj/x.wav", "", 1)],
            playlists: vec![],
        };
        assert_eq!(match_tracks(&lib, "D:", &stick), vec![Some(0), None]);
    }

    #[test]
    fn an_unreadable_cue_or_loop_entry_blocks_new_ones_rather_than_risk_its_slot() {
        // a CUE body too short to show its slot could be any slot
        let (out, c) = fill_markers(&[("CUE".into(), vec![0])], &track());
        assert_eq!((c.cues, c.cues_skipped), (0, 2));
        assert_eq!(c.loops, 2, "loops are unaffected");
        assert!(out.iter().filter(|(n, _)| n == "CUE").count() == 1);
        let (_, c) = fill_markers(&[("LOOP".into(), vec![0])], &track());
        assert_eq!((c.loops, c.loops_skipped, c.cues), (0, 2, 2));
    }

    #[test]
    fn a_section_shorter_than_half_a_beat_folds_into_the_one_before() {
        let g = grid_to_serato(&[
            TempoSection { start_sec: 0.0, bpm: 180.0, beat_in_bar: 1 },
            TempoSection { start_sec: 10.0, bpm: 60.0, beat_in_bar: 1 }, // 0.4 s at 60 BPM
            TempoSection { start_sec: 10.4, bpm: 120.0, beat_in_bar: 1 },
        ])
        .unwrap();
        // the first marker counts its beats up to the next kept section
        assert_eq!(
            g,
            Beatgrid::Dynamic {
                markers: vec![Marker { pos_sec: 0.0, beats_to_next: 31 }],
                terminal_sec: 10.4,
                terminal_bpm: 120.0,
            }
        );
    }

    #[test]
    fn two_stick_files_with_the_same_name_and_size_stay_unmatched() {
        let stick = vec![st(1, "A/dup.mp3", 100), st(2, "B/dup.mp3", 100)];
        let lib = DjLibrary {
            tracks: vec![xml_track(r"C:\m\dup.mp3", "m/dup.mp3", 100)],
            playlists: vec![],
        };
        assert_eq!(match_tracks(&lib, "D:", &stick), vec![None]);
    }

    #[test]
    fn plans_nested_crates_and_merges_into_existing_ones() {
        let stick = vec![st(1, "A/one.mp3", 1), st(2, "A/two.mp3", 2)];
        let lib = DjLibrary {
            tracks: vec![xml_track("", "", 0), xml_track("", "", 0), xml_track("", "", 0)],
            playlists: vec![
                DjPlaylistNode::Folder {
                    name: "House".into(),
                    children: vec![DjPlaylistNode::Playlist { name: "Deep".into(), track_refs: vec![0, 1, 2] }],
                },
                DjPlaylistNode::Playlist { name: "Deep House".into(), track_refs: vec![1, 0] },
                DjPlaylistNode::Playlist { name: "Nothing matched".into(), track_refs: vec![2] },
            ],
        };
        let matches = vec![Some(0), Some(1), None];
        let existing = vec![("deep house.crate".to_string(), vec!["a/TWO.mp3".to_string()])];
        let plans = plan_crates(&lib, &matches, &stick, &existing);
        assert_eq!(
            plans,
            vec![
                CratePlan { file: "House.crate".into(), exists: false, add: vec![] },
                CratePlan {
                    file: "House%%Deep.crate".into(),
                    exists: false,
                    add: vec!["A/one.mp3".into(), "A/two.mp3".into()],
                },
                CratePlan { file: "deep house.crate".into(), exists: true, add: vec!["A/one.mp3".into()] },
            ]
        );
    }

    #[test]
    fn crate_names_lose_characters_serato_or_windows_cannot_take() {
        assert_eq!(crate_segment("50% Off: A/B?"), "50_ Off_ A_B_");
    }

    #[test]
    fn an_unreadable_file_gets_no_plan_and_an_existing_grid_is_kept() {
        let t = DjTrack {
            grid: vec![TempoSection { start_sec: 0.0, bpm: 120.0, beat_in_bar: 1 }],
            cues: vec![hot(0, 1.0)],
            ..Default::default()
        };
        assert!(plan_track(&t, &FileState::Unreadable).is_none());
        let p = plan_track(&t, &FileState::Readable { has_grid: true, markers: None }).unwrap();
        assert!(p.grid.is_none(), "Serato's own grid stays");
        assert!(p.markers.is_some(), "but the free cue slot is filled");
        let fresh = plan_track(&t, &FileState::Readable { has_grid: false, markers: None }).unwrap();
        assert!(fresh.grid.is_some());
    }

    /// A minimal valid 16-bit mono PCM WAV (0.1 s of silence).
    fn minimal_wav(path: &std::path::Path) {
        let data_len = 8820u32;
        let mut b = Vec::new();
        b.extend(b"RIFF");
        b.extend((36 + data_len).to_le_bytes());
        b.extend(b"WAVEfmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(44_100u32.to_le_bytes());
        b.extend(88_200u32.to_le_bytes());
        b.extend(2u16.to_le_bytes());
        b.extend(16u16.to_le_bytes());
        b.extend(b"data");
        b.extend(data_len.to_le_bytes());
        b.extend(vec![0u8; data_len as usize]);
        std::fs::write(path, b).unwrap();
    }

    #[test]
    fn prepare_previews_what_the_import_would_write() {
        let dir = std::env::temp_dir().join(format!("spindex-rbimport-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("song.wav");
        minimal_wav(&wav);
        let size = std::fs::metadata(&wav).unwrap().len();
        let conn = crate::core::db::open(&dir.join("t.db")).unwrap();
        let root = dir.to_string_lossy().to_string();
        let lib_id = crate::core::db::ensure_library(&conn, "L", &root).unwrap();
        conn.execute(
            "INSERT INTO track (library_id, rel_path, file_size, modified_at) VALUES (?1, 'song.wav', ?2, '0')",
            rusqlite::params![lib_id, size as i64],
        )
        .unwrap();
        let letter = &root[..2];
        let xml = format!(
            r#"<DJ_PLAYLISTS Version="1.0.0"><COLLECTION Entries="1">
<TRACK TrackID="1" Name="Song" Size="{size}" Location="file://localhost/Q:/elsewhere/song.wav">
<TEMPO Inizio="0.050" Bpm="128.00" Metro="4/4" Battito="1"/>
<POSITION_MARK Name="" Type="0" Start="1.000" Num="0"/>
<POSITION_MARK Name="" Type="0" Start="2.000" Num="-1"/>
</TRACK>
<TRACK TrackID="2" Name="Gone" Size="7" Location="file://localhost/Q:/elsewhere/gone.wav"/>
</COLLECTION><PLAYLISTS><NODE Type="0" Name="ROOT" Count="1">
<NODE Type="1" Name="Set" KeyType="0" Entries="2"><TRACK Key="1"/><TRACK Key="2"/></NODE>
</NODE></PLAYLISTS></DJ_PLAYLISTS>"#
        );
        let p = prepare(&conn, letter, &xml).unwrap();
        assert_eq!((p.preview.tracks_in_xml, p.preview.matched, p.preview.not_on_stick), (2, 1, 1));
        assert_eq!(p.preview.playlist_entries_missing, 1, "the Set entry that is not on the stick");
        let stop = prepare_cancellable(&conn, letter, &xml, &|| true).unwrap();
        assert!(stop.is_none(), "a cancelled prepare returns no plan");
        assert_eq!((p.preview.grids, p.preview.cues, p.preview.memory_cues_dropped), (1, 1, 1));
        assert_eq!(p.tracks.len(), 1, "one file to write");
        assert!(p.crates.iter().any(|c| c.file == "Set.crate" && !c.exists));
        assert_eq!(p.preview.links_added, 1);
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn several_tempos_become_a_dynamic_grid() {
        let g = grid_to_serato(&[
            TempoSection { start_sec: 0.0, bpm: 120.0, beat_in_bar: 1 },
            TempoSection { start_sec: 64.0, bpm: 122.0, beat_in_bar: 1 },
        ])
        .unwrap();
        assert_eq!(
            g,
            Beatgrid::Dynamic {
                markers: vec![Marker { pos_sec: 0.0, beats_to_next: 128 }],
                terminal_sec: 64.0,
                terminal_bpm: 122.0,
            }
        );
    }
}
