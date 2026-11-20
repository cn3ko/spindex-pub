//! Live diagnostic for Serato crate ordering: reads the real .crate files on a
//! connected stick, resolves each otrk entry against a COPY of the real
//! database, and prints what the reorder would do — without writing anything.
//!
//!   SPINDEX_DIAG_DB="C:\Users\...\spindex.db" SPINDEX_DIAG_LETTER="D:" \
//!     cargo test --test serato_order_live -- --ignored --nocapture

use spindex::core::{db, serato};

#[test]
#[ignore]
fn diagnose_serato_crates() {
    let (Ok(src), Ok(letter)) = (
        std::env::var("SPINDEX_DIAG_DB"),
        std::env::var("SPINDEX_DIAG_LETTER"),
    ) else {
        eprintln!("SPINDEX_DIAG_DB / SPINDEX_DIAG_LETTER not set, skipping");
        return;
    };
    let tmp = std::env::temp_dir().join("spindex-serato-order-diag.db");
    let _ = std::fs::remove_file(&tmp);
    std::fs::copy(&src, &tmp).expect("copying the database");
    let conn = db::open(&tmp).expect("opening the copy");

    let dir = std::path::PathBuf::from(format!("{letter}\\_Serato_\\Subcrates"));
    let entries = std::fs::read_dir(&dir).expect("reading Subcrates");
    let prefix = format!("{letter}\\");
    let mut libs: Vec<(i64, String)> = db::list_libraries(&conn)
        .expect("listing libraries")
        .into_iter()
        .filter(|l| l.root_path.starts_with(&prefix))
        .map(|l| (l.id, l.root_path))
        .collect();
    libs.sort_by_key(|(_, root)| std::cmp::Reverse(root.len()));

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("crate") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let chunks = serato::read_chunks(&bytes);
        let resolved: Vec<Option<i64>> = chunks
            .iter()
            .map(|c| {
                serato::otrk_path(c).and_then(|rel| {
                    let full = format!("{prefix}{}", rel.replace('/', "\\"));
                    libs.iter().find_map(|(id, root)| {
                        let tail = full.strip_prefix(root.as_str())?;
                        db::track_id_by_rel(&conn, *id, &tail.replace('\\', "/")).ok().flatten()
                    })
                })
            })
            .collect();
        let order: Vec<i64> = resolved.iter().flatten().copied().collect();
        let dangling = chunks
            .iter()
            .zip(&resolved)
            .filter(|(c, r)| &c.tag == b"otrk" && r.is_none())
            .count();
        println!(
            "\n{:?}: {} chunks, {} resolvable tracks, {} dangling links",
            path.file_name().unwrap_or_default(),
            chunks.len(),
            order.len(),
            dangling
        );
        if order.len() < 2 {
            continue;
        }
        // move the last track to the front, in memory only
        let mut wanted = vec![order[order.len() - 1]];
        wanted.extend(order[..order.len() - 1].iter().copied());
        let out = serato::reorder_otrk(&chunks, &resolved, &wanted);
        let after: Vec<i64> = out
            .iter()
            .filter_map(serato::otrk_path)
            .filter_map(|rel| {
                let full = format!("{prefix}{}", rel.replace('/', "\\"));
                libs.iter().find_map(|(id, root)| {
                    let tail = full.strip_prefix(root.as_str())?;
                    db::track_id_by_rel(&conn, *id, &tail.replace('\\', "/")).ok().flatten()
                })
            })
            .collect();
        println!("   before: {:?}", &order[..order.len().min(6)]);
        println!("   after:  {:?}", &after[..after.len().min(6)]);
        println!(
            "   non-otrk chunks preserved: {}",
            chunks.iter().filter(|c| &c.tag != b"otrk").count()
                == out.iter().filter(|c| &c.tag != b"otrk").count()
        );
        println!("   byte length unchanged: {}", serato::write_chunks(&out).len() == bytes.len());
    }
}
