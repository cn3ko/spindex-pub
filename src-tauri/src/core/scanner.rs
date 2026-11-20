use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::Connection;
use walkdir::WalkDir;

use crate::core::{db, metadata};

#[derive(Debug, Default)]
pub struct ScanSummary {
    pub found: u64,
    pub new: u64,
    pub updated: u64,
    pub unchanged: u64,
    pub errors: u64,
    /// Indexed files the walk no longer found on disk, newly flagged missing.
    pub removed: u64,
    pub elapsed_secs: f64,
}

impl ScanSummary {
    pub fn tracks_per_second(&self) -> f64 {
        if self.elapsed_secs > 0.0 {
            self.found as f64 / self.elapsed_secs
        } else {
            0.0
        }
    }
}

/// How long a file may be briefly absent before it counts as gone: a tag save
/// renames the original aside, then moves the rewritten copy in, and a check
/// landing between the two renames must not flag the track missing.
const GONE_ATTEMPTS: u32 = 5;
const GONE_RETRY: std::time::Duration = std::time::Duration::from_millis(50);

/// True only when `path` is verifiably gone: absent on several checks over
/// ~250 ms, and not sitting renamed aside (`<name>.spindex-tmp.old`) by a
/// tag save in progress. The one test every code path must pass before it
/// sets `is_missing` — a transient failure (playback error, a check racing a
/// save) otherwise left a present file greyed out until a restart.
pub fn file_is_gone(path: &Path) -> bool {
    files_gone(&[path])[0]
}

/// `file_is_gone` for many paths, waiting at most once for the whole set.
pub fn files_gone(paths: &[&Path]) -> Vec<bool> {
    let mut gone: Vec<bool> = paths.iter().map(|p| !present_or_swapping(p)).collect();
    for _ in 1..GONE_ATTEMPTS {
        if !gone.iter().any(|g| *g) {
            break;
        }
        std::thread::sleep(GONE_RETRY);
        for (g, p) in gone.iter_mut().zip(paths) {
            if *g && present_or_swapping(p) {
                *g = false;
            }
        }
    }
    gone
}

/// Settles one track's `is_missing` flag from disk: flags it only when its
/// file is verifiably gone (`file_is_gone`), and clears a stale flag when the
/// file is there. Returns true when the file is gone. Used wherever something
/// merely *failed* on a track (playback), so a transient error can never grey
/// out a present file.
pub fn recheck_track(conn: &Connection, track_id: i64) -> Result<bool> {
    let abs = db::track_abs_path(conn, track_id)?;
    if file_is_gone(&abs) {
        db::mark_track_missing(conn, track_id)?;
        Ok(true)
    } else {
        conn.execute(
            "UPDATE track SET is_missing = 0 WHERE id = ?1 AND is_missing = 1",
            [track_id],
        )?;
        Ok(false)
    }
}

fn present_or_swapping(path: &Path) -> bool {
    if path.is_file() {
        return true;
    }
    let mut aside = path.as_os_str().to_os_string();
    aside.push(".spindex-tmp.old");
    Path::new(&aside).is_file()
}

/// Millisecond-precision mtime as a string, so equality comparison is exact.
/// Also used by the metadata editor to refresh stamps after a tag write.
pub fn mtime_string(modified: SystemTime) -> String {
    modified
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Normalize to forward slashes so rel_path is stable across platforms.
fn normalize_rel_path(rel: &Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

/// Strip the Windows extended-length prefix (`\\?\`) that canonicalize adds,
/// so stored and displayed paths look like what the user picked.
pub fn clean_path_string(path: &Path) -> String {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.to_string()
    }
}

pub fn scan_folder(conn: &mut Connection, root: &Path) -> Result<ScanSummary> {
    scan_folder_with_progress(conn, root, |_| {})
}

/// Scan `root` into the database: walk the tree, skip files whose size and
/// mtime are unchanged since the last scan, read tags for the rest and upsert.
/// Corrupt or unreadable files are logged and skipped, never fatal.
/// `on_progress` is called every 100 files with the number of files seen so far.
pub fn scan_folder_with_progress(
    conn: &mut Connection,
    root: &Path,
    mut on_progress: impl FnMut(u64),
) -> Result<ScanSummary> {
    let started = Instant::now();
    let root = root
        .canonicalize()
        .with_context(|| format!("resolving {}", root.display()))?;
    let root_str = clean_path_string(&root);
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| root_str.clone());

    let library_id = db::ensure_library(conn, &name, &root_str)?;
    let stamps = db::load_stamps(conn, library_id)?;

    let mut summary = ScanSummary::default();
    // Every rel_path the walk actually saw. What's in `stamps` but not here
    // was deleted from disk since the last scan (see the sweep after the loop).
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Directory-traversal failures specifically, NOT per-file tag errors: an
    // unreadable subfolder means the walk is incomplete, so deletion detection
    // must not run or it would flag that whole subtree as gone.
    let mut walk_errors = 0u64;
    let tx = conn.transaction()?;

    for entry in WalkDir::new(&root).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("walk error: {e}");
                summary.errors += 1;
                walk_errors += 1;
                continue;
            }
        };
        if entry.path().components().any(|c| c.as_os_str() == crate::core::enhance::BACKUP_DIR) {
            continue;
        }
        if !entry.file_type().is_file() || !metadata::is_audio_file(entry.path()) {
            continue;
        }
        // A tag save / enhance temp copy (or a crash's leftover), never a track.
        if entry.file_name().to_string_lossy().contains(".spindex-tmp.") {
            continue;
        }
        summary.found += 1;
        if summary.found % 100 == 0 {
            on_progress(summary.found);
        }

        // Resolved before the stat so a file that exists but can't be stat'ed
        // or tagged still counts as seen — deletion detection may only fire
        // for files the walk genuinely did not find.
        let rel_path = match entry.path().strip_prefix(&root) {
            Ok(rel) => normalize_rel_path(rel),
            Err(_) => normalize_rel_path(entry.path()),
        };
        seen.insert(rel_path.clone());

        let fs_meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("stat failed for {}: {e}", entry.path().display());
                summary.errors += 1;
                continue;
            }
        };
        let file_size = fs_meta.len() as i64;
        let modified_at = fs_meta
            .modified()
            .map(mtime_string)
            .unwrap_or_else(|_| "0".to_string());

        match stamps.get(&rel_path) {
            Some((size, mtime)) if *size == file_size && *mtime == modified_at => {
                summary.unchanged += 1;
                continue;
            }
            existing => {
                let meta = match metadata::read(entry.path()) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("skipping unreadable file {}: {e:#}", entry.path().display());
                        summary.errors += 1;
                        continue;
                    }
                };
                db::upsert_track(&tx, library_id, &rel_path, file_size, &modified_at, &meta)?;
                if existing.is_some() {
                    summary.updated += 1;
                } else {
                    summary.new += 1;
                }
            }
        }
    }

    // Deletion detection: anything the index called present that the walk never
    // saw is gone from disk. Rows are flagged, never deleted — the filesystem is
    // the source of truth, so the cache keeps the row (health counts it under
    // "broken") and a returning file clears the flag through `upsert_track`.
    // Skipped entirely when the traversal itself failed anywhere, since a
    // transiently unreadable folder would otherwise flag its whole subtree.
    if walk_errors == 0 {
        let unseen: Vec<String> = stamps
            .keys()
            .filter(|rel_path| !seen.contains(*rel_path))
            .cloned()
            .collect();
        // The walk skips `.spindex-tmp.` names, so a file a tag save has
        // renamed aside right now looks deleted: confirm before flagging.
        let abs: Vec<std::path::PathBuf> = unseen
            .iter()
            .map(|rel| root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)))
            .collect();
        let refs: Vec<&Path> = abs.iter().map(|p| p.as_path()).collect();
        let gone: Vec<String> = unseen
            .into_iter()
            .zip(files_gone(&refs))
            .filter_map(|(rel, gone)| gone.then_some(rel))
            .collect();
        summary.removed = db::mark_missing_paths(&tx, library_id, &gone)?;
    }

    db::touch_library_scanned(&tx, library_id)?;
    tx.commit()?;
    summary.elapsed_secs = started.elapsed().as_secs_f64();
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn gone_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("spindex-gone-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn file_is_gone_is_false_for_a_present_file() {
        let d = gone_dir("present");
        let f = d.join("a.mp3");
        std::fs::write(&f, b"x").unwrap();
        assert!(!file_is_gone(&f));
    }

    #[test]
    fn file_is_gone_is_true_for_an_absent_file() {
        let d = gone_dir("absent");
        assert!(file_is_gone(&d.join("a.mp3")));
    }

    /// Mid tag save: the original sits renamed aside as
    /// `<name>.spindex-tmp.old` for the moment before the new copy moves in.
    /// That is a file being rewritten, not a missing one.
    #[test]
    fn file_is_gone_is_false_while_a_tag_save_has_it_renamed_aside() {
        let d = gone_dir("aside");
        std::fs::write(d.join("a.mp3.spindex-tmp.old"), b"x").unwrap();
        assert!(!file_is_gone(&d.join("a.mp3")));
    }

    /// The swap finishing while we wait counts as present.
    #[test]
    fn file_is_gone_waits_out_a_short_absence() {
        let d = gone_dir("late");
        let f = d.join("a.mp3");
        let f2 = f.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(60));
            std::fs::write(&f2, b"x").unwrap();
        });
        assert!(!file_is_gone(&f));
        t.join().unwrap();
    }

    #[test]
    fn rel_path_uses_forward_slashes() {
        let rel = PathBuf::from(r"House\Deep\track.mp3");
        assert_eq!(normalize_rel_path(&rel), "House/Deep/track.mp3");
    }

    #[test]
    fn extended_length_prefix_is_stripped() {
        assert_eq!(
            clean_path_string(&PathBuf::from(r"\\?\C:\Music")),
            r"C:\Music"
        );
        assert_eq!(
            clean_path_string(&PathBuf::from(r"\\?\UNC\nas\music")),
            r"\\nas\music"
        );
        assert_eq!(clean_path_string(&PathBuf::from(r"D:\Music")), r"D:\Music");
    }

    #[test]
    fn mtime_is_millis() {
        let t = UNIX_EPOCH + std::time::Duration::from_millis(1_234_567);
        assert_eq!(mtime_string(t), "1234567");
    }

    /// Minimal valid WAV (silence) so lofty can actually read it during scan.
    fn write_silent_wav(path: &Path) {
        let sample_rate: u32 = 22050;
        let samples = vec![0.0f32; sample_rate as usize / 10]; // 0.1s
        let data_len = (samples.len() * 4) as u32;
        let mut wav: Vec<u8> = Vec::with_capacity(44 + samples.len() * 4);
        wav.extend(b"RIFF");
        wav.extend((36 + data_len).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(3u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(sample_rate.to_le_bytes());
        wav.extend((sample_rate * 4).to_le_bytes());
        wav.extend(4u16.to_le_bytes());
        wav.extend(32u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(data_len.to_le_bytes());
        for s in &samples {
            wav.extend(s.to_le_bytes());
        }
        std::fs::write(path, wav).unwrap();
    }

    fn scan_test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spindex-scan-test-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scan_finds_new_then_skips_unchanged_then_detects_update() {
        let dir = scan_test_dir("basic");
        write_silent_wav(&dir.join("a.wav"));
        write_silent_wav(&dir.join("b.wav"));
        std::fs::write(dir.join("cover.jpg"), b"not audio").unwrap();

        let db_path = dir.join("spindex-scan.db");
        let mut conn = db::open(&db_path).unwrap();

        let first = scan_folder(&mut conn, &dir).unwrap();
        assert_eq!(first.found, 2, "non-audio file ignored");
        assert_eq!(first.new, 2);
        assert_eq!(first.updated, 0);
        assert_eq!(first.unchanged, 0);
        assert_eq!(first.errors, 0);

        let second = scan_folder(&mut conn, &dir).unwrap();
        assert_eq!(second.found, 2);
        assert_eq!(second.new, 0);
        assert_eq!(second.updated, 0);
        assert_eq!(second.unchanged, 2, "unchanged size+mtime skips re-read");

        // grow the file so its size stamp no longer matches the recorded one
        let mut bytes = std::fs::read(dir.join("a.wav")).unwrap();
        bytes.extend_from_slice(&[0u8; 8]);
        std::fs::write(dir.join("a.wav"), bytes).unwrap();

        let third = scan_folder(&mut conn, &dir).unwrap();
        assert_eq!(third.updated, 1, "changed file re-read as an update");
        assert_eq!(third.unchanged, 1);

        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_skips_files_under_backup_dir() {
        let dir = scan_test_dir("backup-exclude");
        write_silent_wav(&dir.join("a.wav"));
        std::fs::create_dir_all(dir.join(crate::core::enhance::BACKUP_DIR)).unwrap();
        write_silent_wav(&dir.join(crate::core::enhance::BACKUP_DIR).join("a.wav"));

        let db_path = dir.join("spindex-scan.db");
        let mut conn = db::open(&db_path).unwrap();

        let summary = scan_folder(&mut conn, &dir).unwrap();
        assert_eq!(summary.found, 1, "the backup-dir copy is excluded from the walk");
        assert_eq!(summary.new, 1);

        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_skips_spindex_temp_files() {
        // A tag save or an enhance works on a `.spindex-tmp.` copy beside the
        // original; a crash can leave one behind. It is not a track.
        let dir = scan_test_dir("tmp-exclude");
        write_silent_wav(&dir.join("a.wav"));
        write_silent_wav(&dir.join("a.spindex-tmp.tag.wav"));
        write_silent_wav(&dir.join("b.spindex-tmp.mp3"));

        let db_path = dir.join("spindex-scan.db");
        let mut conn = db::open(&db_path).unwrap();
        let summary = scan_folder(&mut conn, &dir).unwrap();
        assert_eq!(summary.found, 1, "temp copies are excluded from the walk");

        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rescan running while a tag save has the file renamed aside must not
    /// flag it: the walk skips `.spindex-tmp.` names, so without a second look
    /// the track would read as deleted.
    #[test]
    fn scan_does_not_flag_a_file_mid_tag_save() {
        let dir = scan_test_dir("mid-save");
        write_silent_wav(&dir.join("a.wav"));
        let db_path = dir.join("spindex-scan.db");
        let mut conn = db::open(&db_path).unwrap();
        scan_folder(&mut conn, &dir).unwrap();

        std::fs::rename(dir.join("a.wav"), dir.join("a.wav.spindex-tmp.old")).unwrap();
        let second = scan_folder(&mut conn, &dir).unwrap();
        assert_eq!(second.removed, 0, "a file being rewritten is not gone");

        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The player reports a failure; `recheck_track` decides from disk. A
    /// present file must never end up flagged (and a stale flag is cleared),
    /// only a verifiably gone one does.
    #[test]
    fn recheck_track_flags_only_a_verifiably_gone_file() {
        let dir = scan_test_dir("recheck");
        write_silent_wav(&dir.join("a.wav"));
        let db_path = dir.join("spindex-scan.db");
        let mut conn = db::open(&db_path).unwrap();
        scan_folder(&mut conn, &dir).unwrap();
        let id: i64 = conn.query_row("SELECT id FROM track", [], |r| r.get(0)).unwrap();
        let missing = |c: &Connection| -> i64 {
            c.query_row("SELECT is_missing FROM track WHERE id = ?1", [id], |r| r.get(0))
                .unwrap()
        };

        conn.execute("UPDATE track SET is_missing = 1", []).unwrap();
        assert!(!recheck_track(&conn, id).unwrap(), "file is present");
        assert_eq!(missing(&conn), 0, "a stale flag on a present file is cleared");

        std::fs::rename(dir.join("a.wav"), dir.join("a.wav.spindex-tmp.old")).unwrap();
        assert!(!recheck_track(&conn, id).unwrap(), "mid tag save is not gone");
        assert_eq!(missing(&conn), 0);

        std::fs::remove_file(dir.join("a.wav.spindex-tmp.old")).unwrap();
        assert!(recheck_track(&conn, id).unwrap(), "file really gone");
        assert_eq!(missing(&conn), 1);

        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_flags_deleted_files_missing_and_unflags_on_return() {
        let dir = scan_test_dir("deleted");
        write_silent_wav(&dir.join("keep.wav"));
        write_silent_wav(&dir.join("gone.wav"));

        let db_path = dir.join("spindex-scan.db");
        let mut conn = db::open(&db_path).unwrap();

        let first = scan_folder(&mut conn, &dir).unwrap();
        assert_eq!(first.new, 2);
        assert_eq!(first.removed, 0, "nothing was indexed before, nothing can be gone");

        std::fs::remove_file(dir.join("gone.wav")).unwrap();
        let second = scan_folder(&mut conn, &dir).unwrap();
        assert_eq!(second.found, 1);
        assert_eq!(second.removed, 1, "the deleted file is flagged missing");

        let missing: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM track WHERE is_missing = 1 AND rel_path = 'gone.wav'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(missing, 1, "row is kept and flagged, never deleted");
        let kept_present: i64 = conn
            .query_row(
                "SELECT is_missing FROM track WHERE rel_path = 'keep.wav'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept_present, 0, "surviving file untouched");

        // A third scan must not re-count the already-flagged file.
        let third = scan_folder(&mut conn, &dir).unwrap();
        assert_eq!(third.removed, 0, "already-missing rows are not re-counted");

        // The file comes back (e.g. restored from the Recycle Bin).
        write_silent_wav(&dir.join("gone.wav"));
        let fourth = scan_folder(&mut conn, &dir).unwrap();
        assert_eq!(fourth.removed, 0);
        let back: i64 = conn
            .query_row(
                "SELECT is_missing FROM track WHERE rel_path = 'gone.wav'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(back, 0, "a returning file clears the flag");

        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deletion_detection_ignores_files_outside_the_library() {
        // Two libraries under one temp root: flagging must be scoped to the
        // library being scanned, never touching a sibling's rows.
        let base = scan_test_dir("scoped");
        let lib_a = base.join("a");
        let lib_b = base.join("b");
        std::fs::create_dir_all(&lib_a).unwrap();
        std::fs::create_dir_all(&lib_b).unwrap();
        write_silent_wav(&lib_a.join("same-name.wav"));
        write_silent_wav(&lib_b.join("same-name.wav"));

        let db_path = base.join("spindex-scan.db");
        let mut conn = db::open(&db_path).unwrap();
        scan_folder(&mut conn, &lib_a).unwrap();
        scan_folder(&mut conn, &lib_b).unwrap();

        std::fs::remove_file(lib_a.join("same-name.wav")).unwrap();
        let rescan_a = scan_folder(&mut conn, &lib_a).unwrap();
        assert_eq!(rescan_a.removed, 1);

        let b_missing: i64 = conn
            .query_row(
                "SELECT t.is_missing FROM track t JOIN library l ON l.id = t.library_id
                 WHERE l.name = 'b'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(b_missing, 0, "the identically-named file in library b is untouched");

        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_skips_corrupt_files_without_panicking() {
        let dir = scan_test_dir("corrupt");
        write_silent_wav(&dir.join("good.wav"));
        std::fs::write(dir.join("bad.wav"), b"this is not a real wav file").unwrap();

        let db_path = dir.join("spindex-scan.db");
        let mut conn = db::open(&db_path).unwrap();

        let summary = scan_folder(&mut conn, &dir).unwrap();
        assert_eq!(summary.found, 2);
        assert_eq!(summary.new, 1, "only the readable file is upserted");
        assert_eq!(summary.errors, 1, "corrupt file logged as an error, not fatal");

        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
