//! Live diagnostic: for every Serato crate on a stick whose name matches a
//! Rekordbox playlist there, print how much of the crate the playlist holds.
//! Read-only: copies the database (.db + -wal + -shm) to a temp file and only
//! reads the stick.
//!
//!   SPINDEX_DIAG_DB="C:\Users\...\spindex.db" SPINDEX_DIAG_LETTER="D:" \
//!     cargo test --release --test compare_sets_live -- --ignored --nocapture
use spindex::core::dupes::{compare_scopes, Scope};
use spindex::core::{db, djlib, rekordbox, serato, stick_path};

#[test]
#[ignore]
fn serato_crates_vs_rekordbox_playlists() {
    let (Ok(src), Ok(letter)) = (std::env::var("SPINDEX_DIAG_DB"), std::env::var("SPINDEX_DIAG_LETTER")) else {
        eprintln!("SPINDEX_DIAG_DB / SPINDEX_DIAG_LETTER not set, skipping");
        return;
    };
    let tmp = std::env::temp_dir().join("spindex-compare-sets-diag.db");
    for suffix in ["", "-wal", "-shm"] {
        let to = format!("{}{suffix}", tmp.display());
        let _ = std::fs::remove_file(&to);
        let copied = std::fs::copy(format!("{src}{suffix}"), &to);
        if suffix.is_empty() {
            copied.expect("copying the database");
        }
    }
    let conn = db::open(&tmp).expect("opening the copy");

    let pdb = std::path::PathBuf::from(format!("{letter}\\PIONEER\\rekordbox\\export.pdb"));
    let playlists = std::fs::read(&pdb)
        .ok()
        .and_then(|b| rekordbox::resolve_playlists(&conn, &letter, &b).ok())
        .unwrap_or_default();
    println!("Rekordbox playlists: {}", playlists.iter().filter(|p| !p.is_folder).count());

    let subcrates = std::path::PathBuf::from(format!("{letter}\\_Serato_\\Subcrates"));
    let mut compared = 0;
    for (file, entries) in djlib::serato::read_crate_files(&subcrates) {
        let name = serato::crate_display_name(&file);
        let (_, serato_ids, _) =
            stick_path::resolve_crate_entries(&conn, &letter, entries.into_iter().map(Some));
        let Some(p) = playlists.iter().find(|p| !p.is_folder && p.name.eq_ignore_ascii_case(&name)) else {
            println!("{name:<20} serato {:>4}  (no Rekordbox playlist of that name)", serato_ids.len());
            continue;
        };
        let c = compare_scopes(&conn, Scope::Ids(&serato_ids), Scope::Ids(&p.track_ids)).expect("compare");
        compared += 1;
        println!(
            "{name:<20} serato {:>4}  rekordbox {:>4}  shared {:>4} ({:.1}%)",
            serato_ids.len(),
            p.track_ids.len(),
            c.shared,
            c.pct_of_source
        );
    }
    println!("compared {compared} crate/playlist pairs");
}
