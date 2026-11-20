//! Live diagnostic for Serato beat-grid generation: for every folder-crate and
//! every Serato crate on a connected stick, runs the exact grid builder the
//! `regenerate_*_grid` commands run (`serato_beatgrid::build`) against a COPY of
//! the real database, prints what would be written, then reads back the grid
//! currently stored in each file for comparison. Read-only: it copies the
//! database (`.db` + `-wal` + `-shm`) to a temp file and never writes anything
//! to the device or the real database.
//!
//! It reads the on-disk grid of EVERY track, analyzed or not, and fails on
//! any parse failure: that is the check that the wire format matches the
//! grids Serato itself writes. Its first real run (2026-09-24) only read the
//! 4 analyzed tracks and so missed that the marker count was off by one —
//! every SpinDex grid was rejected by Serato.
//!
//!   SPINDEX_DIAG_DB="C:\Users\...\spindex.db" SPINDEX_DIAG_LETTER="D:" \
//!     cargo test --test serato_beatgrid_live -- --ignored --nocapture

use spindex::core::serato_beatgrid::{self, Beatgrid};
use spindex::core::{db, metadata, serato};

#[derive(Default)]
struct Summary {
    crates: usize,
    tracks: usize,
    would_constant: usize,
    would_dynamic: usize,
    would_skip: usize,
    build_failures: usize,
    on_disk_grids: usize,
    on_disk_none: usize,
    empty_files: usize,
    on_disk_parse_failures: usize,
}

fn describe(g: &Beatgrid) -> String {
    match g {
        Beatgrid::Constant { anchor_sec, bpm } => {
            format!("Constant  anchor {anchor_sec:.3}s  bpm {bpm:.2}")
        }
        Beatgrid::Dynamic { markers, terminal_sec, terminal_bpm } => format!(
            "Dynamic   {} markers  terminal {terminal_sec:.3}s @ {terminal_bpm:.2} bpm",
            markers.len()
        ),
    }
}

/// Builds the grid for one track exactly as `spawn_grid_run` does, printing the
/// result or the skip/failure reason, then reads back the file's current grid.
fn report_track(conn: &rusqlite::Connection, id: i64, abs: &std::path::Path, s: &mut Summary) {
    s.tracks += 1;
    let downs = db::downbeat_grid(conn, id).ok().flatten().unwrap_or_default();
    if downs.len() < serato_beatgrid::MIN_DOWNBEATS {
        s.would_skip += 1;
        println!(
            "      #{id:<6} SKIP  {} downbeats (need {})  {}",
            downs.len(),
            serato_beatgrid::MIN_DOWNBEATS,
            abs.file_name().unwrap_or_default().to_string_lossy()
        );
        print_on_disk(abs, s);
        return;
    }
    let beats = db::beat_grid(conn, id).ok().flatten().unwrap_or_default();
    let drift: Option<f64> = conn
        .query_row(
            "SELECT tempo_drift_pct FROM track WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    match serato_beatgrid::build(&downs, &beats, drift, serato_beatgrid::timing_offset(&abs)) {
        Ok(grid) => {
            match grid {
                Beatgrid::Constant { .. } => s.would_constant += 1,
                Beatgrid::Dynamic { .. } => s.would_dynamic += 1,
            }
            println!(
                "      #{id:<6} WOULD WRITE  {}  (drift {:?})  {}",
                describe(&grid),
                drift,
                abs.file_name().unwrap_or_default().to_string_lossy()
            );
        }
        Err(e) => {
            s.build_failures += 1;
            println!(
                "      #{id:<6} BUILD FAILED  {e:#}  {}",
                abs.file_name().unwrap_or_default().to_string_lossy()
            );
        }
    }
    print_on_disk(abs, s);
}

/// Reads the grid currently in the file. Called for EVERY track, analyzed or
/// not, because checking the reader against Serato-authored grids is the
/// point: a parse failure here means the wire format is wrong.
fn print_on_disk(abs: &std::path::Path, s: &mut Summary) {
    if !abs.is_file() {
        println!("             on disk:  (file not reachable)");
        return;
    }
    // An empty file is damage, not a grid-format question: count it apart so
    // the assert below only ever means "the wire format is wrong".
    if std::fs::metadata(abs).map(|m| m.len() == 0).unwrap_or(false) {
        s.empty_files += 1;
        println!("             on disk:  EMPTY FILE (0 bytes) — audio lost, restore it");
        return;
    }
    match metadata::read_serato_beatgrid(abs) {
        Ok(Some(g)) => {
            s.on_disk_grids += 1;
            println!("             on disk:  {}", describe(&g));
        }
        Ok(None) => {
            s.on_disk_none += 1;
            println!("             on disk:  (no grid)");
        }
        Err(e) => {
            s.on_disk_parse_failures += 1;
            println!("             on disk:  PARSE FAILED  {e:#}");
        }
    }
}

#[test]
#[ignore]
fn diagnose_serato_beatgrids() {
    let (Ok(src), Ok(letter)) = (
        std::env::var("SPINDEX_DIAG_DB"),
        std::env::var("SPINDEX_DIAG_LETTER"),
    ) else {
        eprintln!("SPINDEX_DIAG_DB / SPINDEX_DIAG_LETTER not set, skipping");
        return;
    };

    let tmp = std::env::temp_dir().join("spindex-beatgrid-diag.db");
    for suffix in ["", "-wal", "-shm"] {
        let from = format!("{src}{suffix}");
        let to = format!("{}{suffix}", tmp.display());
        let _ = std::fs::remove_file(&to);
        // -wal / -shm may legitimately be absent; only the base file must copy.
        if suffix.is_empty() {
            std::fs::copy(&from, &to).expect("copying the database");
        } else {
            let _ = std::fs::copy(&from, &to);
        }
    }
    let conn = db::open(&tmp).expect("opening the copy");
    let mut s = Summary::default();

    // ---- folder-crates ---------------------------------------------------
    let libs = db::list_libraries(&conn).expect("listing libraries");
    for lib in &libs {
        let crates = match db::crate_folders(&conn, lib.id) {
            Ok(c) => c,
            Err(e) => {
                println!("\n=== library {} — crate_folders FAILED: {e:#}", lib.name);
                continue;
            }
        };
        for c in &crates {
            let Ok(files) = db::crate_track_files(&conn, lib.id, &c.path) else {
                continue;
            };
            if files.is_empty() {
                continue;
            }
            s.crates += 1;
            println!(
                "\n=== folder-crate {:?} / {:?}  ({} tracks)",
                lib.name, c.path, files.len()
            );
            for t in &files {
                let abs = db::track_root_and_abs_path(&conn, t.id)
                    .map(|(_, abs)| abs)
                    .unwrap_or_default();
                report_track(&conn, t.id, &abs, &mut s);
            }
        }
    }

    // ---- Serato crates -------------------------------------------------- //
    // Mirrors serato_order_live.rs: resolve each otrk entry against the copy,
    // walking libraries longest-root-first.
    let prefix = format!("{letter}\\");
    let mut serato_libs: Vec<(i64, String)> = libs
        .iter()
        .filter(|l| l.root_path.starts_with(&prefix))
        .map(|l| (l.id, l.root_path.clone()))
        .collect();
    serato_libs.sort_by_key(|(_, root)| std::cmp::Reverse(root.len()));

    let dir = std::path::PathBuf::from(format!("{letter}\\_Serato_\\Subcrates"));
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("crate") {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else { continue };
                let chunks = serato::read_chunks(&bytes);
                let ids: Vec<i64> = chunks
                    .iter()
                    .filter_map(|c| {
                        let rel = serato::otrk_path(c)?;
                        let full = format!("{prefix}{}", rel.replace('/', "\\"));
                        serato_libs.iter().find_map(|(id, root)| {
                            let tail = full.strip_prefix(root.as_str())?;
                            db::track_id_by_rel(&conn, *id, &tail.replace('\\', "/"))
                                .ok()
                                .flatten()
                        })
                    })
                    .collect();
                if ids.is_empty() {
                    continue;
                }
                s.crates += 1;
                println!(
                    "\n=== Serato crate {:?}  ({} resolvable tracks)",
                    path.file_name().unwrap_or_default(),
                    ids.len()
                );
                for id in ids {
                    let abs = db::track_root_and_abs_path(&conn, id)
                        .map(|(_, abs)| abs)
                        .unwrap_or_default();
                    report_track(&conn, id, &abs, &mut s);
                }
            }
        }
        Err(e) => println!("\n(no Serato Subcrates at {}: {e})", dir.display()),
    }

    println!(
        "\n---- summary ----\n\
         crates seen:            {}\n\
         tracks examined:        {}\n\
         would write constant:   {}\n\
         would write dynamic:    {}\n\
         would skip (few downs): {}\n\
         grid build failures:    {}\n\
         grids read from disk:   {}\n\
         no grid on disk:        {}\n\
         EMPTY FILES:            {}\n\
         PARSE FAILURES:         {}",
        s.crates,
        s.tracks,
        s.would_constant,
        s.would_dynamic,
        s.would_skip,
        s.build_failures,
        s.on_disk_grids,
        s.on_disk_none,
        s.empty_files,
        s.on_disk_parse_failures,
    );
    assert_eq!(
        s.on_disk_parse_failures, 0,
        "a Serato-authored grid did not parse: the wire format is wrong"
    );
}
