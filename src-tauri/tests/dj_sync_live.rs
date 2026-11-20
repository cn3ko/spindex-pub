//! Live diagnostic for the Serato → Rekordbox XML export. Read-only: copies
//! the database (.db + -wal + -shm) to a temp file, reads the stick, builds the
//! XML in memory and writes it ONLY to the temp folder for inspection. Fails on
//! any Serato Markers2 parse failure (an empty file is reported separately).
//!
//!   SPINDEX_DIAG_DB="C:\Users\...\spindex.db" SPINDEX_DIAG_LETTER="D:" \
//!     cargo test --release --test dj_sync_live -- --ignored --nocapture
use std::sync::atomic::AtomicBool;

use spindex::core::djlib::capabilities::{losses, REKORDBOX_XML};
use spindex::core::djlib::endpoint::DjEndpoint;
use spindex::core::djlib::serato::SeratoEndpoint;
use spindex::core::{db, metadata, rekordbox_xml};

#[test]
#[ignore]
fn dj_sync_export_dry_run() {
    let (Ok(src), Ok(letter)) = (std::env::var("SPINDEX_DIAG_DB"), std::env::var("SPINDEX_DIAG_LETTER")) else {
        eprintln!("SPINDEX_DIAG_DB / SPINDEX_DIAG_LETTER not set, skipping");
        return;
    };
    let tmp = std::env::temp_dir().join("spindex-djsync-diag.db");
    for suffix in ["", "-wal", "-shm"] {
        let to = format!("{}{suffix}", tmp.display());
        let _ = std::fs::remove_file(&to);
        let copied = std::fs::copy(format!("{src}{suffix}"), &to);
        if suffix.is_empty() {
            copied.expect("copying the database");
        }
    }
    let conn = db::open(&tmp).expect("opening the copy");

    // 1. every file's markers must parse (empty files counted apart)
    let (mut frames, mut empty, mut failures) = (0, 0, Vec::new());
    for t in db::stick_tracks(&conn, &letter).expect("stick tracks") {
        if std::fs::metadata(&t.abs_path).map(|m| m.len() == 0).unwrap_or(false) {
            empty += 1;
            continue;
        }
        match metadata::read_serato_markers(&t.abs_path) {
            Ok(Some(_)) => frames += 1,
            Ok(None) => {}
            Err(e) => failures.push(format!("{}: {e:#}", t.stick_rel_path)),
        }
    }
    println!("markers frames read: {frames}, empty files: {empty}, parse failures: {}", failures.len());
    for f in &failures {
        println!("  FAIL {f}");
    }

    // 2. the full export, in memory
    let source = SeratoEndpoint { conn: &conn, letter: letter.clone(), only_crates: None };
    let read = source.read(&|_, _| {}, &AtomicBool::new(false)).expect("read");
    let lib = &read.library;
    println!(
        "tracks {}  playlists {}  grids {}  cues {}  loops {}  unreadable {}  dangling {}",
        lib.tracks.len(),
        lib.playlist_count(),
        lib.tracks.iter().filter(|t| !t.grid.is_empty()).count(),
        lib.tracks.iter().map(|t| t.cues.len()).sum::<usize>(),
        lib.tracks.iter().map(|t| t.loops.len()).sum::<usize>(),
        read.unreadable.len(),
        read.dangling
    );
    for l in losses(lib, &REKORDBOX_XML) {
        println!("loss: {} ({} tracks)", l.what, l.tracks);
    }
    let xml = rekordbox_xml::write_xml(lib, "diag");
    let out = std::env::temp_dir().join("spindex-djsync-diag.xml");
    std::fs::write(&out, &xml).expect("writing the temp XML");
    println!("XML ({} bytes) written to {}", xml.len(), out.display());
    assert!(failures.is_empty(), "a Serato Markers2 frame did not parse");
}
