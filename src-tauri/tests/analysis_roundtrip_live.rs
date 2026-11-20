//! Diagnostic: runs the app's own analyze-then-save path over one real file
//! and reports exactly what landed in the database. Ignored by default.
//!
//!   SPINDEX_ANALYZE_FILE="C:\path\to\track.aiff" \
//!     cargo test --release --test analysis_roundtrip_live -- --ignored --nocapture
//!
//! Exists because "analysis does not keep the key" can break at four
//! boundaries — sidecar output, JSON parse, the save UPDATE, and the read
//! back — and only running all four in order says which.
use spindex::core::{analysis, db};

#[test]
#[ignore]
fn analyze_then_save_keeps_the_key() {
    let Ok(file) = std::env::var("SPINDEX_ANALYZE_FILE") else {
        panic!("set SPINDEX_ANALYZE_FILE to a real audio file");
    };
    let path = std::path::PathBuf::from(&file);
    assert!(path.is_file(), "not a file: {file}");

    let Some(sc) = analysis::find_sidecars() else {
        panic!("find_sidecars() returned None — analyzer or models missing");
    };
    println!("ffmpeg:   {}", sc.ffmpeg.display());
    println!("analyzer: {}", sc.analyzer.display());

    let res = analysis::analyze_file(&sc, &path).expect("analyze_file failed");
    println!(
        "sidecar -> bpm {:?} key {:?} conf {:?} candidates {} peaks {} bands {:?} beats {:?}",
        res.bpm,
        res.key_camelot,
        res.key_confidence,
        res.key_candidates.as_ref().map(|c| c.len()).unwrap_or(0),
        res.peaks.len(),
        res.bands.as_ref().map(|b| b.len()),
        res.beats.as_ref().map(|b| b.len()),
    );

    let dir = std::env::temp_dir().join(format!("spindex-analyze-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let conn = db::open(&dir.join("t.db")).unwrap();
    let lib = db::ensure_library(&conn, "L", "C:/L").unwrap();
    conn.execute(
        "INSERT INTO track (library_id, rel_path, file_size, modified_at) VALUES (?1, 'a.mp3', 1, '0')",
        rusqlite::params![lib],
    )
    .unwrap();
    let id = conn.last_insert_rowid();

    let candidates_json = res
        .key_candidates
        .as_ref()
        .and_then(|c| serde_json::to_string(c).ok());
    db::save_analysis(
        &conn,
        id,
        res.bpm,
        res.key_camelot.as_deref(),
        res.key_confidence,
        candidates_json.as_deref(),
        &res.peaks,
        res.bands.as_deref(),
        res.beats.as_deref(),
        res.downbeats.as_deref(),
        false,
    )
    .expect("save_analysis failed");

    let (bpm, key, source, conf, drift, has_grid): (
        Option<f64>,
        Option<String>,
        Option<String>,
        Option<f64>,
        Option<f64>,
        i64,
    ) = conn
        .query_row(
            "SELECT bpm, key_camelot, key_source, key_confidence, tempo_drift_pct,
                    (beat_grid IS NOT NULL) FROM track WHERE id = ?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap();
    println!(
        "database -> bpm {bpm:?} key {key:?} source {source:?} conf {conf:?} drift {drift:?} grid {has_grid}"
    );

    assert!(key.is_some(), "the key did not survive the save");
    assert_eq!(source.as_deref(), Some("analysis"));

    let _ = std::fs::remove_dir_all(&dir);
}
