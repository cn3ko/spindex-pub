//! Duplicate-scan timing benchmark against a real database.
//!
//! Ignored by default; run explicitly with a database produced by
//! spindex-scan (or a copy of the app database):
//!
//!   SPINDEX_BENCH_DB=path\to\spindex.db cargo test --release --test dupe_bench -- --ignored --nocapture
//!
//! M3 acceptance: duplicate scan of 10k tracks finishes under a minute.

use std::time::Instant;

use spindex::core::{db, dupes};

#[test]
#[ignore]
fn dupe_scan_timing() {
    let path = std::env::var("SPINDEX_BENCH_DB").expect("set SPINDEX_BENCH_DB to a spindex db file");
    let conn = db::open(std::path::Path::new(&path)).expect("open db");

    let libs: Vec<(i64, String, i64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT l.id, l.name, COUNT(t.id) FROM library l \
                 LEFT JOIN track t ON t.library_id = l.id GROUP BY l.id",
            )
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows")
    };

    for (id, name, tracks) in libs {
        let start = Instant::now();
        let report = dupes::find_duplicates(&conn, id).expect("find_duplicates");
        let elapsed = start.elapsed();
        println!(
            "library {id} ({name}): {tracks} tracks -> {} dupe groups ({} tracks in groups, {} unhashed) in {:.2?}",
            report.groups.len(),
            report.groups.iter().map(|g| g.tracks.len()).sum::<usize>(),
            report.unhashed,
            elapsed
        );
        assert!(
            elapsed.as_secs() < 60,
            "duplicate scan of library {id} took {elapsed:.2?}, acceptance bar is 60s"
        );
    }
}
