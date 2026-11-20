//! Manual ordering of tracks inside a folder-crate. The authority is a small
//! JSON file inside the folder itself, so the order travels with a USB stick
//! and survives a database rebuild; `track.crate_pos` is only a cache of it.

use std::path::Path;
use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::core::db;

/// Sidecar file holding a folder-crate's hand-made order. Lives inside the
/// folder itself; filenames, never track ids, because ids are database-local.
pub const ORDER_FILE_NAME: &str = ".spindex-order.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct OrderFile {
    version: u32,
    order: Vec<String>,
}

/// Reads the order file. A missing file and a corrupt one both yield an empty
/// list: they genuinely mean "no stored order", so the crate falls back to add
/// order and the next reorder rewrites the file.
///
/// Any OTHER I/O error (an antivirus lock, a sharing violation, a permission
/// problem on a connected medium) is propagated instead. Degrading those to an
/// empty list would destroy work: the cache would be overwritten with pure add
/// order and the next reorder would write that scrambled order back to the
/// file permanently.
pub fn read_order(dir: &Path) -> std::io::Result<Vec<String>> {
    match std::fs::read(dir.join(ORDER_FILE_NAME)) {
        Ok(bytes) => Ok(serde_json::from_slice::<OrderFile>(&bytes)
            .map(|f| f.order)
            .unwrap_or_default()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Writes the order file atomically (tmp-write + rename), the same pattern the
/// Serato writer uses, so an interrupted write can never leave a half file.
pub fn write_order(dir: &Path, names: &[String]) -> std::io::Result<()> {
    let file = OrderFile { version: 1, order: names.to_vec() };
    let json = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!("{ORDER_FILE_NAME}.spindex-tmp"));
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, dir.join(ORDER_FILE_NAME))
}

/// One indexed file sitting directly in a crate folder.
#[derive(Debug, Clone)]
pub struct CrateTrack {
    pub id: i64,
    /// Last path segment of `rel_path`, e.g. "Modjo - Lady.aiff".
    pub file_name: String,
    pub added_at: Option<i64>,
}

/// Resolves the stored order against the files actually indexed, returning
/// track ids in display order (position = index + 1).
///
/// Filenames listed but no longer present are dropped; files present but not
/// listed are appended in add order (`added_at`, then filename, both ascending)
/// so an import, copy-in or Explorer drop queues at the bottom. Matching is
/// case-insensitive because FAT and NTFS are.
pub fn reconcile_order(file_order: &[String], disk: &[CrateTrack]) -> Vec<i64> {
    // One pre-lowercased index, built once, instead of an O(n²) scan that
    // allocated a lowercased String per comparison. Buckets hold the disk
    // indices for a name in DESCENDING order so `pop()` yields the earliest
    // unconsumed entry — exactly what the old first-match-then-remove scan
    // did, including "a filename repeated in the order file never yields the
    // same track twice".
    let mut index: HashMap<String, Vec<usize>> = HashMap::with_capacity(disk.len());
    for (i, t) in disk.iter().enumerate().rev() {
        index.entry(t.file_name.to_lowercase()).or_default().push(i);
    }
    let mut taken = vec![false; disk.len()];
    let mut out = Vec::with_capacity(disk.len());
    for name in file_order {
        if let Some(i) = index.get_mut(&name.to_lowercase()).and_then(|b| b.pop()) {
            taken[i] = true;
            out.push(disk[i].id);
        }
    }
    let mut remaining: Vec<&CrateTrack> =
        disk.iter().enumerate().filter(|(i, _)| !taken[*i]).map(|(_, t)| t).collect();
    remaining.sort_by(|a, b| {
        a.added_at
            .cmp(&b.added_at)
            .then_with(|| a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()))
    });
    out.extend(remaining.iter().map(|t| t.id));
    out
}

/// Lifts the selected ids out of `order` and drops them back in at `target`,
/// an index into the remaining (unselected) list.
fn place(order: &[i64], set: &HashSet<i64>, target: usize) -> Vec<i64> {
    let selected: Vec<i64> = order.iter().copied().filter(|id| set.contains(id)).collect();
    let rest: Vec<i64> = order.iter().copied().filter(|id| !set.contains(id)).collect();
    let t = target.min(rest.len());
    let mut out = Vec::with_capacity(order.len());
    out.extend_from_slice(&rest[..t]);
    out.extend_from_slice(&selected);
    out.extend_from_slice(&rest[t..]);
    out
}

/// Moves the selected ids one slot up (`delta` < 0) or down (`delta` > 0),
/// insert-and-renumber style. A multi-selection moves as one block, keeping the
/// members' relative order; a scattered selection gathers at the block's slot.
/// Clamped at both ends — no wraparound.
pub fn move_block(order: &[i64], ids: &[i64], delta: i32) -> Vec<i64> {
    let set: HashSet<i64> = ids.iter().copied().collect();
    let Some(first) = order.iter().position(|id| set.contains(id)) else {
        return order.to_vec();
    };
    let before = order[..first].iter().filter(|id| !set.contains(id)).count();
    let rest_len = order.iter().filter(|id| !set.contains(id)).count();
    let target = if delta < 0 { before.saturating_sub(1) } else { (before + 1).min(rest_len) };
    place(order, &set, target)
}

/// Places the selected ids immediately before `before_index`, an index into the
/// current `order` (i.e. the row the user dropped onto). `order.len()` or
/// anything larger means "after the last row".
pub fn insert_block(order: &[i64], ids: &[i64], before_index: usize) -> Vec<i64> {
    let set: HashSet<i64> = ids.iter().copied().collect();
    let cut = before_index.min(order.len());
    let target = order[..cut].iter().filter(|id| !set.contains(id)).count();
    place(order, &set, target)
}

/// The order-file names after dropping `added` before row `before_index` of
/// `current` (the crate as shown). A name already listed moves rather than
/// appearing twice; names match case-insensitively, like the file system.
pub fn insert_names(current: &[String], added: &[String], before_index: usize) -> Vec<String> {
    let drop: HashSet<String> = added.iter().map(|n| n.to_lowercase()).collect();
    let cut = before_index.min(current.len());
    let target = current[..cut]
        .iter()
        .filter(|n| !drop.contains(&n.to_lowercase()))
        .count();
    let mut out: Vec<String> = current
        .iter()
        .filter(|n| !drop.contains(&n.to_lowercase()))
        .cloned()
        .collect();
    let mut seen = HashSet::new();
    let block: Vec<String> = added
        .iter()
        .filter(|n| seen.insert(n.to_lowercase()))
        .cloned()
        .collect();
    out.splice(target..target, block);
    out
}

/// Resolves a crate's manual order and caches it in `track.crate_pos`.
///
/// Read-only with respect to the filesystem: a crate that has never been
/// hand-ordered (or whose medium is currently unplugged, so the order file is
/// simply absent) resolves to add order. Returns the ids in display order.
///
/// An order file that exists but cannot be read is an error, not add order —
/// see `read_order`. Bailing out here leaves `crate_pos` exactly as it was.
pub fn ensure_positions(conn: &Connection, library_id: i64, folder: &str) -> Result<Vec<i64>> {
    let root = db::library_root(conn, library_id)?;
    let dir = crate_dir(&root, folder);
    heal_against_disk(&dir);
    let disk = db::crate_track_files(conn, library_id, folder)?;
    let stored = read_order(&dir).context("reading the crate order file")?;
    let order = reconcile_order(&stored, &disk);
    persist_positions(conn, &order)?;
    Ok(order)
}

/// Deletes a crate folder, first removing SpinDex's own order sidecar if it is
/// the ONLY thing left in it.
///
/// `remove_dir` refusing a non-empty directory is the safety guard that keeps
/// audio out of this path, and that stays. But `.spindex-order.json` is our
/// metadata, not user content, and it is not hidden on Windows: a hand-ordered
/// crate whose tracks were later moved out looks empty and would otherwise be
/// undeletable forever. If anything else is present we touch nothing at all
/// and let the usual non-empty refusal stand.
pub fn delete_crate_dir(dir: &Path) -> std::io::Result<()> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        let names: Vec<std::ffi::OsString> =
            entries.filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        if names.len() == 1 && names[0] == ORDER_FILE_NAME {
            std::fs::remove_file(dir.join(ORDER_FILE_NAME))?;
        }
    }
    std::fs::remove_dir(dir)
}

/// The order a copied crate folder ends up with: the destination's own order
/// first (a copy that merges into an existing crate never reshuffles it), then
/// the source's names it does not hold yet, in the source's order. Names match
/// case-insensitively, like everywhere else on a Windows file system.
pub fn merge_order(dest: &[String], src: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = dest.iter().map(|n| n.to_lowercase()).collect();
    let mut out = dest.to_vec();
    for name in src {
        if seen.insert(name.to_lowercase()) {
            out.push(name.clone());
        }
    }
    out
}

/// Carries a folder-crate's hand-made order along with a copy of its files.
/// Does nothing when the source has no order. Names of files that did not
/// arrive (skipped, failed, cancelled) are harmless: listing a crate drops
/// names it cannot find on disk.
pub fn carry_order(src_dir: &Path, dest_dir: &Path) -> std::io::Result<()> {
    let src = read_order(src_dir)?;
    if src.is_empty() || !dest_dir.is_dir() {
        return Ok(());
    }
    let dest = read_order(dest_dir)?;
    let merged = merge_order(&dest, &src);
    if merged != dest {
        write_order(dest_dir, &merged)?;
    }
    Ok(())
}

/// Persists a new hand-made order: the order file first, then the cache. If the
/// file cannot be written (read-only medium, unplugged stick, vanished folder)
/// the error propagates and `crate_pos` is left exactly as it was, so the two
/// can never disagree.
pub fn apply_order(
    conn: &Connection,
    library_id: i64,
    folder: &str,
    new_order: &[i64],
) -> Result<()> {
    let root = db::library_root(conn, library_id)?;
    let dir = crate_dir(&root, folder);
    let disk = db::crate_track_files(conn, library_id, folder)?;
    // Derive the file names and the ids fed to persist_positions from the
    // SAME filtered set. A stale id (a track no longer in this folder, e.g.
    // moved or deleted between the caller's read and this apply) must be
    // dropped from both consistently - filtering only the file and then
    // persisting positions from the original `new_order` would shift every
    // later track's crate_pos one slot ahead of its real index in the file.
    let matched: Vec<(i64, String)> = new_order
        .iter()
        .filter_map(|id| disk.iter().find(|t| t.id == *id).map(|t| (*id, t.file_name.clone())))
        .collect();
    let names: Vec<String> = matched.iter().map(|(_, n)| n.clone()).collect();
    write_order(&dir, &names).context("writing the crate order file")?;
    let ids: Vec<i64> = matched.iter().map(|(id, _)| *id).collect();
    persist_positions(conn, &ids)
}

/// Removes `names` from a folder's order file, if it has one. A no-op — no
/// read past checking the file exists, no write — when nothing in `names`
/// appears in it, so deleting a track outside any hand-ordered crate never
/// touches disk. Best-effort throughout: an unreadable or unwritable order
/// file is left alone rather than failing the caller, since pruning stale
/// entries is cleanup, not the point of a delete.
pub fn prune_names(dir: &Path, names: &[String]) {
    let Ok(order) = read_order(dir) else { return };
    if order.is_empty() {
        return;
    }
    let drop: HashSet<String> = names.iter().map(|n| n.to_lowercase()).collect();
    let before = order.len();
    let kept: Vec<String> = order.into_iter().filter(|n| !drop.contains(&n.to_lowercase())).collect();
    if kept.len() == before {
        return;
    }
    let _ = write_order(dir, &kept);
}

/// Self-heals a crate's order file against what's physically in the folder
/// RIGHT NOW: an entry naming a file that's gone is dropped, no rescan or
/// explicit delete required. Runs only when the folder itself is reachable
/// (`dir.is_dir()`) — if it isn't (e.g. an unplugged USB stick), nothing here
/// is trustworthy and every entry is left exactly as it was, since the files
/// may simply be disconnected, not gone.
pub fn heal_against_disk(dir: &Path) {
    if !dir.is_dir() {
        return;
    }
    let Ok(order) = read_order(dir) else { return };
    if order.is_empty() {
        return;
    }
    let before = order.len();
    let kept: Vec<String> = order.into_iter().filter(|n| dir.join(n).is_file()).collect();
    if kept.len() == before {
        return;
    }
    let _ = write_order(dir, &kept);
}

/// Spot-checks a folder-crate's own tracks against disk RIGHT NOW and flips
/// `is_missing` accordingly — cheap (one `Path::is_file` per track already in
/// this folder, no tag reads, no hashing), unlike a full library rescan.
/// Companion to `heal_against_disk`: that fixes the order file, this fixes
/// why the crate's track LIST still showed a hand-deleted file as a normal,
/// undimmed row (a full rescan hadn't run yet to flag it). A file that
/// reappears (restored from the Recycle Bin, moved back) is un-dimmed the
/// same way. Never removes a row — CLAUDE.md's non-destructive rule: missing
/// is a FLAG, not a deletion, so the row stays visible and still deletable.
///
/// Guarded by `dir.is_dir()`, same as `heal_against_disk`: an unreachable
/// root (an unplugged USB stick) must never mass-flag its tracks missing —
/// they may simply be disconnected, not gone.
pub fn heal_missing_flags(conn: &Connection, library_id: i64, folder: &str) -> Result<()> {
    let root = db::library_root(conn, library_id)?;
    let dir = crate_dir(&root, folder);
    if !dir.is_dir() {
        return Ok(());
    }
    let tracks = db::crate_track_files(conn, library_id, folder)?;
    let mut suspects: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut back = Vec::new();
    for t in &tracks {
        let rel = if folder.is_empty() {
            t.file_name.clone()
        } else {
            format!("{folder}/{}", t.file_name)
        };
        let abs = dir.join(&t.file_name);
        if abs.is_file() {
            back.push(rel);
        } else {
            suspects.push((rel, abs));
        }
    }
    // A failed stat is only a suspicion: the listing may have landed inside a
    // tag save's rename window. Only verifiably gone files get flagged.
    let paths: Vec<&Path> = suspects.iter().map(|(_, p)| p.as_path()).collect();
    let verdicts = crate::core::scanner::files_gone(&paths);
    let gone: Vec<String> = suspects
        .into_iter()
        .zip(verdicts)
        .filter_map(|((rel, _), gone)| gone.then_some(rel))
        .collect();
    db::mark_missing_paths(conn, library_id, &gone).context("flagging gone tracks missing")?;
    db::clear_missing_paths(conn, library_id, &back).context("clearing returned tracks")?;
    Ok(())
}

/// Splits a stored `rel_path` ("/"-separated, same convention as
/// `crate_track_files`) into its parent folder and file name.
fn split_rel_path(rel_path: &str) -> (&str, &str) {
    match rel_path.rfind('/') {
        Some(i) => (&rel_path[..i], &rel_path[i + 1..]),
        None => ("", rel_path),
    }
}

/// Prunes a batch of just-deleted tracks from whatever folder-crate order
/// files they belonged to. `entries` is (library_id, rel_path) captured
/// BEFORE the caller removes the rows — a deleted track has no row left to
/// look either up from. Groups by crate directory so a multi-folder
/// selection only reads/writes each order file once. Best-effort, same as
/// `prune_names`: a library whose root can't be resolved is skipped rather
/// than failing the whole delete.
pub fn prune_deleted_tracks(conn: &Connection, entries: &[(i64, String)]) {
    let mut by_dir: HashMap<std::path::PathBuf, Vec<String>> = HashMap::new();
    for (library_id, rel_path) in entries {
        let Ok(root) = db::library_root(conn, *library_id) else { continue };
        let (folder, name) = split_rel_path(rel_path);
        by_dir.entry(crate_dir(&root, folder)).or_default().push(name.to_string());
    }
    for (dir, names) in by_dir {
        prune_names(&dir, &names);
    }
}

fn crate_dir(root: &str, folder: &str) -> std::path::PathBuf {
    if folder.is_empty() {
        std::path::PathBuf::from(root)
    } else {
        Path::new(root).join(folder.replace('/', std::path::MAIN_SEPARATOR_STR))
    }
}

fn persist_positions(conn: &Connection, order: &[i64]) -> Result<()> {
    let positions: Vec<(i64, i64)> = order
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i as i64 + 1))
        .collect();
    db::set_crate_positions(conn, &positions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir()
            .join(format!("spindex-order-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn merge_order_keeps_the_destination_order_and_appends_new_names() {
        let dest = vec!["C.mp3".to_string(), "A.mp3".to_string()];
        let src = vec!["B.mp3".to_string(), "a.MP3".to_string(), "D.mp3".to_string()];
        assert_eq!(merge_order(&dest, &src), vec!["C.mp3", "A.mp3", "B.mp3", "D.mp3"]);
        assert_eq!(merge_order(&[], &src), src);
    }

    #[test]
    fn carry_order_copies_a_crate_order_to_its_copy() {
        let src = tmp_dir("carry-src");
        let dest = tmp_dir("carry-dest");
        let names = vec!["B.mp3".to_string(), "A.flac".to_string()];
        write_order(&src, &names).unwrap();
        carry_order(&src, &dest).unwrap();
        assert_eq!(read_order(&dest).unwrap(), names);
        // a second copy into the same folder merges and changes nothing
        carry_order(&src, &dest).unwrap();
        assert_eq!(read_order(&dest).unwrap(), names);
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn carry_order_without_a_source_order_writes_nothing() {
        let src = tmp_dir("carry-none-src");
        let dest = tmp_dir("carry-none-dest");
        carry_order(&src, &dest).unwrap();
        assert!(!dest.join(ORDER_FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn write_then_read_round_trips() {
        let d = tmp_dir("round");
        let names = vec!["B.mp3".to_string(), "A.flac".to_string()];
        write_order(&d, &names).unwrap();
        assert_eq!(read_order(&d).unwrap(), names);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_file_reads_empty() {
        let d = tmp_dir("missing");
        assert!(read_order(&d).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn corrupt_file_reads_empty() {
        let d = tmp_dir("corrupt");
        std::fs::write(d.join(ORDER_FILE_NAME), b"not json{{").unwrap();
        assert!(read_order(&d).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn write_leaves_no_tmp_file() {
        let d = tmp_dir("tmp");
        write_order(&d, &["A.mp3".to_string()]).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != ORDER_FILE_NAME)
            .collect();
        assert!(leftovers.is_empty(), "unexpected files: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The order file should match what's physically on the folder RIGHT NOW,
    /// not only after an explicit delete goes through SpinDex — a file removed
    /// by hand (Explorer, another machine) must drop out of the list too, the
    /// next time the crate is viewed.
    #[test]
    fn heal_against_disk_removes_names_of_files_gone_from_the_physical_folder() {
        let d = tmp_dir("heal-gone");
        std::fs::write(d.join("a.flac"), b"x").unwrap();
        // b.flac and c.flac are listed but were deleted by hand.
        write_order(
            &d,
            &["a.flac".to_string(), "b.flac".to_string(), "c.flac".to_string()],
        )
        .unwrap();

        heal_against_disk(&d);

        assert_eq!(read_order(&d).unwrap(), vec!["a.flac"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn heal_against_disk_is_a_noop_when_everything_still_exists() {
        let d = tmp_dir("heal-fine");
        std::fs::write(d.join("a.flac"), b"x").unwrap();
        std::fs::write(d.join("b.flac"), b"x").unwrap();
        let names = vec!["a.flac".to_string(), "b.flac".to_string()];
        write_order(&d, &names).unwrap();

        heal_against_disk(&d);

        assert_eq!(read_order(&d).unwrap(), names);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A folder that isn't reachable right now (e.g. an unplugged USB stick)
    /// must be left alone entirely — the files may simply be disconnected,
    /// not gone, and there is nothing trustworthy to check against.
    #[test]
    fn heal_against_disk_does_nothing_when_the_folder_itself_is_unreachable() {
        let d = std::env::temp_dir()
            .join(format!("spindex-order-heal-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        assert!(!d.exists());

        heal_against_disk(&d); // must not panic, must not create the dir

        assert!(!d.exists());
    }

    /// Deleting a track (the track context menu / duplicates-modal delete
    /// path) never touched a folder-crate's order file, so a deleted track's
    /// name lingered in `.spindex-order.json` forever — reconcile_order
    /// happily drops an unmatched name from the *resolved* order, but the
    /// file on disk itself was never rewritten to match.
    #[test]
    fn prune_names_removes_matching_entries_case_insensitively() {
        let d = tmp_dir("prune-match");
        write_order(&d, &[
            "A.mp3".to_string(),
            "B.mp3".to_string(),
            "C.mp3".to_string(),
        ])
        .unwrap();

        prune_names(&d, &["b.MP3".to_string()]);

        assert_eq!(read_order(&d).unwrap(), vec!["A.mp3", "C.mp3"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn prune_names_is_a_noop_when_nothing_matches() {
        let d = tmp_dir("prune-nomatch");
        let names = vec!["A.mp3".to_string(), "B.mp3".to_string()];
        write_order(&d, &names).unwrap();

        prune_names(&d, &["Z.mp3".to_string()]);

        assert_eq!(read_order(&d).unwrap(), names);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn prune_names_leaves_a_folder_with_no_order_file_untouched() {
        let d = tmp_dir("prune-none");
        // no write_order call: no .spindex-order.json exists
        prune_names(&d, &["A.mp3".to_string()]);
        assert!(!d.join(ORDER_FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn ct(id: i64, name: &str, added: Option<i64>) -> CrateTrack {
        CrateTrack { id, file_name: name.to_string(), added_at: added }
    }

    #[test]
    fn reconcile_follows_the_file_and_drops_missing() {
        let disk = vec![ct(1, "A.mp3", Some(10)), ct(3, "C.mp3", Some(30))];
        let file = vec!["C.mp3".to_string(), "B.mp3".to_string(), "A.mp3".to_string()];
        // B is in the file but gone from disk: dropped, no gap left behind.
        assert_eq!(reconcile_order(&file, &disk), vec![3, 1]);
    }

    #[test]
    fn reconcile_appends_unlisted_in_add_order() {
        let disk = vec![
            ct(1, "A.mp3", Some(10)),
            ct(2, "B.mp3", Some(50)),
            ct(3, "C.mp3", Some(30)),
        ];
        let file = vec!["B.mp3".to_string()];
        // listed first, then the rest by added_at ascending
        assert_eq!(reconcile_order(&file, &disk), vec![2, 1, 3]);
    }

    #[test]
    fn reconcile_without_a_file_is_pure_add_order() {
        let disk = vec![ct(2, "B.mp3", Some(50)), ct(1, "A.mp3", Some(10))];
        assert_eq!(reconcile_order(&[], &disk), vec![1, 2]);
    }

    #[test]
    fn reconcile_ties_break_on_filename_case_insensitively() {
        let disk = vec![ct(1, "b.mp3", Some(10)), ct(2, "A.mp3", Some(10))];
        assert_eq!(reconcile_order(&[], &disk), vec![2, 1]);
    }

    #[test]
    fn reconcile_matches_filenames_case_insensitively() {
        let disk = vec![ct(1, "Lady.AIFF", Some(10)), ct(2, "Funk.mp3", Some(20))];
        let file = vec!["lady.aiff".to_string()];
        assert_eq!(reconcile_order(&file, &disk), vec![1, 2]);
    }

    #[test]
    fn move_single_track_up_and_down() {
        let order = vec![1, 2, 3, 4];
        assert_eq!(move_block(&order, &[4], -1), vec![1, 2, 4, 3]);
        assert_eq!(move_block(&order, &[1], 1), vec![2, 1, 3, 4]);
    }

    #[test]
    fn move_clamps_at_both_ends() {
        let order = vec![1, 2, 3];
        assert_eq!(move_block(&order, &[1], -1), vec![1, 2, 3]);
        assert_eq!(move_block(&order, &[3], 1), vec![1, 2, 3]);
    }

    #[test]
    fn move_contiguous_block_keeps_internal_order() {
        let order = vec![1, 2, 3, 4, 5];
        assert_eq!(move_block(&order, &[3, 4], -1), vec![1, 3, 4, 2, 5]);
        assert_eq!(move_block(&order, &[3, 4], 1), vec![1, 2, 5, 3, 4]);
    }

    #[test]
    fn move_scattered_selection_coalesces() {
        let order = vec![1, 2, 3, 4, 5];
        // 1 and 4 selected: they gather at the first one's slot, shifted down
        assert_eq!(move_block(&order, &[1, 4], 1), vec![2, 1, 4, 3, 5]);
    }

    #[test]
    fn insert_block_at_start_middle_and_end() {
        let order = vec![1, 2, 3, 4];
        assert_eq!(insert_block(&order, &[4], 0), vec![4, 1, 2, 3]);
        assert_eq!(insert_block(&order, &[4], 2), vec![1, 2, 4, 3]);
        assert_eq!(insert_block(&order, &[1], 4), vec![2, 3, 4, 1]);
        // an out-of-range index clamps to the end instead of panicking
        assert_eq!(insert_block(&order, &[1], 99), vec![2, 3, 4, 1]);
    }

    #[test]
    fn insert_names_places_new_files_at_the_drop_position() {
        let cur: Vec<String> = ["a.mp3", "b.mp3", "c.mp3"].iter().map(|s| s.to_string()).collect();
        let add = vec!["x.mp3".to_string(), "y.mp3".to_string()];
        assert_eq!(insert_names(&cur, &add, 1), ["a.mp3", "x.mp3", "y.mp3", "b.mp3", "c.mp3"]);
        assert_eq!(insert_names(&cur, &add, 0)[..2], ["x.mp3", "y.mp3"]);
        assert_eq!(insert_names(&cur, &add, 99)[3..], ["x.mp3", "y.mp3"]);
    }

    #[test]
    fn insert_names_moves_a_name_already_listed_instead_of_doubling_it() {
        let cur: Vec<String> = ["a.mp3", "B.mp3", "c.mp3"].iter().map(|s| s.to_string()).collect();
        // case-insensitive, like the file system; index counts the current rows
        assert_eq!(insert_names(&cur, &["b.MP3".to_string()], 3), ["a.mp3", "c.mp3", "b.MP3"]);
        assert_eq!(insert_names(&cur, &["c.mp3".to_string()], 0), ["c.mp3", "a.mp3", "B.mp3"]);
    }

    #[test]
    fn insert_block_spanning_the_target_is_a_no_op() {
        let order = vec![1, 2, 3, 4];
        assert_eq!(insert_block(&order, &[2, 3], 3), vec![1, 2, 3, 4]);
    }

    #[test]
    fn move_with_no_matching_ids_is_a_no_op() {
        let order = vec![1, 2, 3];
        assert_eq!(move_block(&order, &[99], -1), vec![1, 2, 3]);
    }

    use rusqlite::Connection;

    /// Builds an in-memory library whose root is a real temp directory, so the
    /// order file can actually be written.
    fn order_db(dir: &Path) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::core::db::SCHEMA).unwrap();
        // Later columns (added_at, crate_pos) come from migrations, not the
        // base schema (see CLAUDE.md: new columns go through migrate(),
        // never into the base schema) — run them so this test DB has both.
        db::migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO library (id, name, root_path) VALUES (1, 'L', ?1)",
            rusqlite::params![dir.to_string_lossy()],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO track (id, library_id, rel_path, file_size, modified_at, added_at)
             VALUES (1, 1, 'House/a.mp3', 1, '0', 10),
                    (2, 1, 'House/b.mp3', 1, '0', 20),
                    (3, 1, 'House/c.mp3', 1, '0', 30);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn ensure_then_apply_persists_file_and_positions() {
        let root = tmp_dir("orch");
        let house = root.join("House");
        std::fs::create_dir_all(&house).unwrap();
        // order_db's rows name these three files; heal_against_disk now runs
        // on every ensure_positions, so they must actually exist for a test
        // that expects the order file's names to survive intact.
        std::fs::write(house.join("a.mp3"), b"x").unwrap();
        std::fs::write(house.join("b.mp3"), b"x").unwrap();
        std::fs::write(house.join("c.mp3"), b"x").unwrap();
        let conn = order_db(&root);

        // no order file yet -> add order, positions written
        let order = ensure_positions(&conn, 1, "House").unwrap();
        assert_eq!(order, vec![1, 2, 3]);
        let pos: i64 = conn
            .query_row("SELECT crate_pos FROM track WHERE id = 3", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pos, 3);

        // move the last track to the top and persist
        let moved = move_block(&order, &[3], -1);
        apply_order(&conn, 1, "House", &moved).unwrap();
        assert_eq!(read_order(&root.join("House")).unwrap(), vec!["a.mp3", "c.mp3", "b.mp3"]);

        // a fresh ensure now follows the file
        assert_eq!(ensure_positions(&conn, 1, "House").unwrap(), vec![1, 3, 2]);
        let pos: i64 = conn
            .query_row("SELECT crate_pos FROM track WHERE id = 2", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pos, 3);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The real-world case that motivated `heal_against_disk`: a file is
    /// deleted by hand (Explorer, another machine) — its DB row survives
    /// until the next rescan, but the order file must stop naming it the very
    /// next time the crate is viewed, without waiting on a rescan or an
    /// explicit SpinDex delete.
    #[test]
    fn ensure_positions_heals_the_order_file_against_a_track_deleted_by_hand() {
        let root = tmp_dir("heal-ensure");
        let house = root.join("House");
        std::fs::create_dir_all(&house).unwrap();
        // a.mp3 and c.mp3 are still there; b.mp3 was deleted by hand, but its
        // track row (id 2) is untouched — no rescan has run yet.
        std::fs::write(house.join("a.mp3"), b"x").unwrap();
        std::fs::write(house.join("c.mp3"), b"x").unwrap();
        let conn = order_db(&root);
        write_order(
            &house,
            &["a.mp3".to_string(), "b.mp3".to_string(), "c.mp3".to_string()],
        )
        .unwrap();

        ensure_positions(&conn, 1, "House").unwrap();

        assert_eq!(
            read_order(&house).unwrap(),
            vec!["a.mp3", "c.mp3"],
            "the order file itself must drop the hand-deleted entry",
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `heal_against_disk` fixes the order file, but a hand-deleted track's
    /// row still reads `is_missing = 0` until a full library rescan runs —
    /// so the crate's track LIST kept showing it as a perfectly normal row.
    /// `heal_missing_flags` is the scoped, cheap (no tag reads, no hashing,
    /// just Path::is_file per track in this one folder) counterpart: it dims
    /// a hand-deleted file the moment the crate is viewed, and un-dims one
    /// that reappears (e.g. restored from the Recycle Bin) the same way.
    #[test]
    fn heal_missing_flags_dims_a_hand_deleted_track_and_undims_a_restored_one() {
        let root = tmp_dir("heal-flags");
        let house = root.join("House");
        std::fs::create_dir_all(&house).unwrap();
        std::fs::write(house.join("a.mp3"), b"x").unwrap();
        // b.mp3 is NOT written: deleted by hand, DB doesn't know yet.
        let conn = order_db(&root);
        // c.mp3 is written on disk but the DB thinks it's missing (e.g. it
        // was restored from the Recycle Bin after an earlier deletion).
        std::fs::write(house.join("c.mp3"), b"x").unwrap();
        conn.execute("UPDATE track SET is_missing = 1 WHERE id = 3", []).unwrap();

        heal_missing_flags(&conn, 1, "House").unwrap();

        let missing = |id: i64| -> i64 {
            conn.query_row("SELECT is_missing FROM track WHERE id = ?1", [id], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(missing(1), 0, "a.mp3 is present, stays clear");
        assert_eq!(missing(2), 1, "b.mp3 is gone from disk, now flagged");
        assert_eq!(missing(3), 0, "c.mp3 reappeared, flag cleared");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A crate view refreshing while a tag save has b.mp3 renamed aside (the
    /// moment between moving the original away and moving the rewritten copy
    /// in) must not grey b.mp3 out — it is being rewritten, not deleted.
    #[test]
    fn heal_missing_flags_leaves_a_track_mid_tag_save_alone() {
        let root = tmp_dir("heal-flags-swap");
        let house = root.join("House");
        std::fs::create_dir_all(&house).unwrap();
        std::fs::write(house.join("a.mp3"), b"x").unwrap();
        std::fs::write(house.join("b.mp3.spindex-tmp.old"), b"x").unwrap();
        std::fs::write(house.join("c.mp3"), b"x").unwrap();
        let conn = order_db(&root);

        heal_missing_flags(&conn, 1, "House").unwrap();

        let missing: i64 = conn
            .query_row("SELECT is_missing FROM track WHERE id = 2", [], |r| r.get(0))
            .unwrap();
        assert_eq!(missing, 0, "a file mid tag save is not missing");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Same reachable-root caution as `heal_against_disk`: an unplugged
    /// device must never have its tracks mass-flagged missing just because
    /// the folder can't currently be statted.
    #[test]
    fn heal_missing_flags_does_nothing_when_the_folder_is_unreachable() {
        let root = tmp_dir("heal-flags-unreachable");
        // "House" is never created on disk.
        let conn = order_db(&root);

        heal_missing_flags(&conn, 1, "House").unwrap();

        let missing: i64 = conn
            .query_row("SELECT is_missing FROM track WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(missing, 0, "an unreachable root must not flag anything");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn apply_order_fails_before_touching_the_db_when_the_folder_is_gone() {
        let root = tmp_dir("nofolder");
        let conn = order_db(&root);
        // "House" was never created on disk
        assert!(apply_order(&conn, 1, "House", &[3, 2, 1]).is_err());
        let pos: Option<i64> = conn
            .query_row("SELECT crate_pos FROM track WHERE id = 3", [], |r| r.get(0))
            .unwrap();
        assert!(pos.is_none(), "a failed file write must leave the DB untouched");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The delete path (commands::delete_tracks) captures (library_id,
    /// rel_path) for every row it's about to remove and hands them to
    /// `prune_deleted_tracks` so the crate's order file matches what's left,
    /// instead of accumulating names for files that no longer exist.
    #[test]
    fn prune_deleted_tracks_removes_the_track_from_its_folders_order_file() {
        let root = tmp_dir("prune-integration");
        std::fs::create_dir_all(root.join("House")).unwrap();
        let conn = order_db(&root);
        write_order(
            &root.join("House"),
            &["a.mp3".to_string(), "b.mp3".to_string(), "c.mp3".to_string()],
        )
        .unwrap();

        prune_deleted_tracks(&conn, &[(1, "House/a.mp3".to_string())]);

        assert_eq!(
            read_order(&root.join("House")).unwrap(),
            vec!["b.mp3", "c.mp3"],
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// I1: a missing file keeps its slot. list_tracks deliberately still shows
    /// a missing row (dimmed), so crate_track_files must number it too —
    /// otherwise the renumbering covers only the present tracks, the missing
    /// row keeps a stale crate_pos, and every row after it sits at a different
    /// screen index than its index in the backend's order.
    #[test]
    fn missing_tracks_keep_their_slot_and_match_the_listing() {
        let root = tmp_dir("missingslot");
        std::fs::create_dir_all(root.join("House")).unwrap();
        let conn = order_db(&root);
        conn.execute("UPDATE track SET is_missing = 1 WHERE id = 2", []).unwrap();

        let order = ensure_positions(&conn, 1, "House").unwrap();
        assert_eq!(order, vec![1, 2, 3], "the missing row is numbered like any other");

        // contiguous 1..n over exactly the rows the table lists
        let mut positions: Vec<i64> = Vec::new();
        for id in &order {
            positions.push(
                conn.query_row("SELECT crate_pos FROM track WHERE id = ?1", [id], |r| r.get(0))
                    .unwrap(),
            );
        }
        assert_eq!(positions, vec![1, 2, 3]);

        let page = db::list_tracks(
            &conn, "", Some(1), Some("num"), false, None, Some("House"), None, 100, 0,
        )
        .unwrap();
        let listed: Vec<i64> = page.rows.iter().map(|r| r.id).collect();
        assert_eq!(listed, order, "screen row order must equal the backend order");
    }

    /// I3a: a steady-state listing must not rewrite a single row — every write
    /// fires the blanket track_fts AFTER UPDATE trigger.
    #[test]
    fn a_second_ensure_writes_nothing() {
        let root = tmp_dir("nowrite");
        std::fs::create_dir_all(root.join("House")).unwrap();
        let conn = order_db(&root);
        ensure_positions(&conn, 1, "House").unwrap();

        let before = conn.total_changes();
        ensure_positions(&conn, 1, "House").unwrap();
        assert_eq!(conn.total_changes(), before, "no row may be rewritten");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// M1: an order file that exists but cannot be read must abort the refresh
    /// instead of silently degrading to add order and overwriting the cache.
    /// A directory standing in for the file is a portable "unreadable path".
    #[test]
    fn an_unreadable_order_file_aborts_instead_of_renumbering() {
        let root = tmp_dir("unreadable");
        std::fs::create_dir_all(root.join("House").join(ORDER_FILE_NAME)).unwrap();
        let conn = order_db(&root);

        assert!(ensure_positions(&conn, 1, "House").is_err());
        let pos: Option<i64> = conn
            .query_row("SELECT crate_pos FROM track WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert!(pos.is_none(), "crate_pos must be left untouched");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// I5: a crate holding nothing but our own order sidecar is deletable;
    /// anything else present and the non-empty refusal stands, untouched.
    #[test]
    fn delete_crate_dir_removes_a_lone_order_file_only() {
        let root = tmp_dir("delcrate");
        let lone = root.join("Lone");
        std::fs::create_dir_all(&lone).unwrap();
        write_order(&lone, &["a.mp3".to_string()]).unwrap();
        assert!(delete_crate_dir(&lone).is_ok());
        assert!(!lone.exists());

        let busy = root.join("Busy");
        std::fs::create_dir_all(&busy).unwrap();
        write_order(&busy, &["a.mp3".to_string()]).unwrap();
        std::fs::write(busy.join("a.mp3"), b"x").unwrap();
        assert!(delete_crate_dir(&busy).is_err());
        assert!(busy.join(ORDER_FILE_NAME).exists(), "order file must survive");
        assert!(busy.join("a.mp3").exists(), "user content must survive");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A stale id in `new_order` (a track no longer in the folder, e.g. moved
    /// or deleted between the caller's read and this apply) must be dropped
    /// from BOTH the written file and the persisted positions consistently —
    /// never just from the file, which would leave crate_pos one slot ahead
    /// of every track that follows the stale id in the file.
    #[test]
    fn apply_order_drops_a_stale_id_from_file_and_positions_together() {
        let root = tmp_dir("staleid");
        std::fs::create_dir_all(root.join("House")).unwrap();
        let conn = order_db(&root);

        // id 99 is not among House's tracks (1, 2, 3) - simulates a track
        // that vanished from the folder between ensure_positions and apply.
        apply_order(&conn, 1, "House", &[1, 99, 2, 3]).unwrap();

        assert_eq!(
            read_order(&root.join("House")).unwrap(),
            vec!["a.mp3", "b.mp3", "c.mp3"],
            "the stale id must not appear in the order file"
        );
        for (id, expected_pos) in [(1, 1), (2, 2), (3, 3)] {
            let pos: i64 = conn
                .query_row(
                    "SELECT crate_pos FROM track WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                pos, expected_pos,
                "track {id} crate_pos must match its index in the order file"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }
}
