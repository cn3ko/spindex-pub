//! Diagnostic: dump what `find_duplicates` actually produces per match level,
//! so UI reports ("no select options in <level> groups") can be checked against
//! real data instead of guessed at.
//!
//! Ignored by default; run against a copy of the app database:
//!
//!   SPINDEX_INSPECT_DB=path\to\spindex.db cargo test --test dupe_inspect -- --ignored --nocapture

use spindex::core::{db, dupes};

#[test]
#[ignore]
fn dump_groups_per_level() {
    let path =
        std::env::var("SPINDEX_INSPECT_DB").expect("set SPINDEX_INSPECT_DB to a spindex db file");
    let conn = db::open(std::path::Path::new(&path)).expect("open db");

    let lib_ids: Vec<(i64, String)> = {
        let mut stmt = conn
            .prepare("SELECT id, name FROM library ORDER BY id")
            .expect("prepare libs");
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query libs");
        rows.filter_map(Result::ok).collect()
    };

    for (lib_id, lib_name) in lib_ids {
        let report = match dupes::find_duplicates(&conn, lib_id) {
            Ok(r) => r,
            Err(e) => {
                println!("library {lib_id} ({lib_name}): find_duplicates FAILED: {e:#}");
                continue;
            }
        };
        if report.groups.is_empty() {
            continue;
        }
        println!("\n=== library {lib_id}: {lib_name} — {} groups", report.groups.len());

        for level in ["identical", "same_tags", "similar", "same_filename"] {
            let of_level: Vec<_> = report.groups.iter().filter(|g| g.level == level).collect();
            let missing_keeper = of_level
                .iter()
                .filter(|g| g.suggested_keeper_id.is_none())
                .count();
            let keeper_not_member = of_level
                .iter()
                .filter(|g| match g.suggested_keeper_id {
                    Some(k) => !g.tracks.iter().any(|t| t.id == k),
                    None => false,
                })
                .count();
            let selectable_zero = of_level
                .iter()
                .filter(|g| {
                    g.tracks
                        .iter()
                        .filter(|t| Some(t.id) != g.suggested_keeper_id)
                        .count()
                        == 0
                })
                .count();
            println!(
                "  {level:<14} groups={:<5} keeper_missing={missing_keeper:<4} \
                 keeper_not_in_group={keeper_not_member:<4} groups_with_no_selectable_row={selectable_zero}",
                of_level.len(),
            );

            // Show the first two groups of this level in full.
            for g in of_level.iter().take(2) {
                println!("    --- group (keeper id {:?})", g.suggested_keeper_id);
                for t in &g.tracks {
                    let role = if Some(t.id) == g.suggested_keeper_id {
                        "KEEP    "
                    } else {
                        "checkbox"
                    };
                    println!(
                        "      [{role}] id={:<7} {:?} — {:?} | {}",
                        t.id, t.artist, t.title, t.rel_path
                    );
                }
            }
        }
    }
}
