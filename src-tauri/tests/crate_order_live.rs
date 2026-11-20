//! Live diagnostic for manual crate ordering: runs the exact engine that
//! `commands::list_tracks` runs (`crate_order::ensure_positions`) against a
//! COPY of the real database, and prints what each layer sees. The command
//! swallows this call's error (`let _ = …`) so a crate on an unplugged stick
//! still lists — which also means a real failure is invisible in the UI.
//! This test is where that error becomes visible.
//!
//!   SPINDEX_DIAG_DB="C:\Users\...\spindex.db" cargo test --test crate_order_live -- --ignored --nocapture

use spindex::core::{crate_order, db};

#[test]
#[ignore]
fn diagnose_crate_ordering() {
    let Ok(src) = std::env::var("SPINDEX_DIAG_DB") else {
        eprintln!("SPINDEX_DIAG_DB not set, skipping");
        return;
    };
    // Work on a copy: this writes crate_pos, and the real database should not
    // be mutated by a diagnostic.
    let tmp = std::env::temp_dir().join("spindex-crate-order-diag.db");
    let _ = std::fs::remove_file(&tmp);
    std::fs::copy(&src, &tmp).expect("copying the database");
    let conn = db::open(&tmp).expect("opening the copy");

    let libs = db::list_libraries(&conn).expect("listing libraries");
    for lib in &libs {
        println!("\n=== library {} — {:?} (root {:?})", lib.id, lib.name, lib.root_path);
        let crates = match db::crate_folders(&conn, lib.id) {
            Ok(c) => c,
            Err(e) => {
                println!("    crate_folders FAILED: {e:#}");
                continue;
            }
        };
        if crates.is_empty() {
            println!("    no crates — nothing here is orderable");
            continue;
        }
        for c in &crates {
            let files = db::crate_track_files(&conn, lib.id, &c.path);
            let n_files = files.as_ref().map(|v| v.len()).unwrap_or(0);
            print!(
                "    crate {:?} (depth {}, {} direct tracks, {} resolvable files) -> ensure_positions: ",
                c.path, c.depth, c.track_count, n_files
            );
            match crate_order::ensure_positions(&conn, lib.id, &c.path) {
                Ok(order) => {
                    let set: i64 = conn
                        .query_row(
                            "SELECT COUNT(*) FROM track WHERE library_id = ?1 AND crate_pos IS NOT NULL",
                            [lib.id],
                            |r| r.get(0),
                        )
                        .unwrap_or(-1);
                    println!("OK, {} ids, {} rows now positioned in this library", order.len(), set);
                }
                Err(e) => println!("FAILED: {e:#}"),
            }
        }
    }
}
