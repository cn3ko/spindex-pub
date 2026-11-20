//! Live diagnostic for Rekordbox export.pdb parsing: reads the real
//! `export.pdb` on a connected stick, resolves its playlist tree against a
//! COPY of the real database, and prints what the read-only viewer would
//! show — without writing anything to either the stick or the real
//! spindex.db. Mirrors `crate_order_live.rs` / `serato_order_live.rs`.
//!
//!   SPINDEX_DIAG_DB="C:\Users\...\spindex.db" SPINDEX_DIAG_LETTER="D:" \
//!     cargo test --test rekordbox_read_live -- --ignored --nocapture

use spindex::core::rekordbox;

#[test]
#[ignore]
fn survey_real_device_playlists() {
    let (Ok(db_path), Ok(letter)) = (
        std::env::var("SPINDEX_DIAG_DB"),
        std::env::var("SPINDEX_DIAG_LETTER"),
    ) else {
        eprintln!("SPINDEX_DIAG_DB / SPINDEX_DIAG_LETTER not set, skipping");
        return;
    };

    // Work on a copy: this is read-only against the real device and the
    // real database, but resolve_playlists takes a &Connection and nothing
    // here should ever risk mutating the user's actual spindex.db.
    let tmp_db = std::env::temp_dir().join("spindex_rekordbox_diag.db");
    let _ = std::fs::remove_file(&tmp_db);
    std::fs::copy(&db_path, &tmp_db).expect("copy db to temp — never touch the original");
    // Also copy -wal/-shm, per the WAL-mode gotcha documented in CLAUDE.md's
    // 2026-08-10 progress log entries: recent writes can sit in the -wal
    // file rather than the main .db file, and reading the .db alone can
    // silently show a stale, pre-scan state.
    for suffix in ["-wal", "-shm"] {
        let src = format!("{db_path}{suffix}");
        if std::path::Path::new(&src).exists() {
            let dst = format!("{}{suffix}", tmp_db.display());
            let _ = std::fs::remove_file(&dst);
            std::fs::copy(&src, &dst).expect("copy -wal/-shm to temp");
        }
    }

    let conn = rusqlite::Connection::open(&tmp_db).expect("open copied db");

    let pdb_path = format!("{letter}\\PIONEER\\rekordbox\\export.pdb");
    let bytes = std::fs::read(&pdb_path).expect("read real export.pdb (read-only)");
    println!("read {} bytes from {pdb_path}", bytes.len());

    let playlists = rekordbox::resolve_playlists(&conn, &letter, &bytes)
        .expect("parse real device export.pdb");

    println!("\nfound {} playlist/folder nodes", playlists.len());

    let mut total_resolved = 0usize;
    let mut total_unresolved = 0usize;
    let mut folder_count = 0usize;
    let mut playlist_count = 0usize;

    // Simple depth-first indent by walking parent chains — purely cosmetic,
    // for readability of the printed tree; resolve_playlists itself returns
    // a flat list.
    let depth_of = |id: u32| -> usize {
        let mut d = 0usize;
        let mut cur = id;
        loop {
            let Some(node) = playlists.iter().find(|p| p.id == cur) else {
                break;
            };
            if node.parent_id == 0 || node.parent_id == cur {
                break;
            }
            cur = node.parent_id;
            d += 1;
            if d > 32 {
                break; // guard against any cycle in malformed data
            }
        }
        d
    };

    for p in &playlists {
        if p.is_folder {
            folder_count += 1;
        } else {
            playlist_count += 1;
        }
        total_resolved += p.track_ids.len();
        total_unresolved += p.unresolved;

        let indent = "  ".repeat(depth_of(p.id));
        println!(
            "{indent}{}{} id={} parent={} resolved={} unresolved={}",
            if p.is_folder { "[folder] " } else { "" },
            p.name,
            p.id,
            p.parent_id,
            p.track_ids.len(),
            p.unresolved,
        );

        // Light internal-consistency check: resolved + unresolved should
        // equal the playlist's total entry count. resolve_playlists doesn't
        // expose the raw entry count directly, so this is implicitly
        // checked by the fact that both numbers come from splitting one
        // entries-per-playlist list — nothing to assert further here beyond
        // both being non-negative, which the types already guarantee.
    }

    println!(
        "\nsummary: {} folders, {} playlists, {} tracks resolved, {} unresolved",
        folder_count, playlist_count, total_resolved, total_unresolved
    );

    // Read-only: leave the real spindex.db and export.pdb untouched, only
    // clean up our own temp copy.
    let _ = std::fs::remove_file(&tmp_db);
    let _ = std::fs::remove_file(format!("{}-wal", tmp_db.display()));
    let _ = std::fs::remove_file(format!("{}-shm", tmp_db.display()));
}
