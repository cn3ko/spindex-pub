//! Live dry run of the Rekordbox → Serato import. Read-only: copies the
//! database (`.db` + `-wal` + `-shm`) to a temp file, builds the import plan
//! for a real Rekordbox XML and a real stick, and prints it. Nothing on the
//! stick or in the real database is written.
//!
//!   SPINDEX_DIAG_DB="C:\Users\...\spindex.db" SPINDEX_DIAG_LETTER="D:" \
//!   SPINDEX_RB_XML="C:\...\rekordbox.xml" \
//!     cargo test --release --test rekordbox_import_live -- --ignored --nocapture
//!
//! Pointed at the XML SpinDex itself exported from the stick's Serato data,
//! "only fill gaps" should find (almost) nothing to add — every grid and cue
//! in it came from those very files.

use spindex::core::db;
use spindex::core::djlib::serato_import::prepare;

#[test]
#[ignore]
fn rekordbox_import_dry_run() {
    let (Ok(src), Ok(letter), Ok(xml_path)) = (
        std::env::var("SPINDEX_DIAG_DB"),
        std::env::var("SPINDEX_DIAG_LETTER"),
        std::env::var("SPINDEX_RB_XML"),
    ) else {
        eprintln!("SPINDEX_DIAG_DB / SPINDEX_DIAG_LETTER / SPINDEX_RB_XML not set, skipping");
        return;
    };
    let tmp = std::env::temp_dir().join("spindex-rbimport-diag.db");
    for suffix in ["", "-wal", "-shm"] {
        let to = format!("{}{suffix}", tmp.display());
        let _ = std::fs::remove_file(&to);
        let copied = std::fs::copy(format!("{src}{suffix}"), &to);
        if suffix.is_empty() {
            copied.expect("copying the database");
        }
    }
    let conn = db::open(&tmp).expect("opening the copy");
    let xml = std::fs::read_to_string(&xml_path).expect("reading the XML");
    let p = prepare(&conn, &letter, &xml).expect("building the plan");
    println!("{:#?}", p.preview);
    println!("\nfiles that would be written: {}", p.tracks.len());
    for t in p.tracks.iter().take(20) {
        println!(
            "  {}  grid:{}  cues:+{}  loops:+{}  colour:{}",
            t.abs.file_name().unwrap_or_default().to_string_lossy(),
            t.edit.grid.is_some(),
            t.edit.counts.cues,
            t.edit.counts.loops,
            t.edit.counts.colour
        );
    }
    println!("\ncrates:");
    for c in &p.crates {
        println!("  {}  {}  +{} links", c.file, if c.exists { "merge" } else { "new" }, c.add.len());
    }
}
