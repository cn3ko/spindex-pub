//! Live diagnostic: how close is SpinDex's BPM to the one in Serato's own grid?
//! For every analyzed track (stored `beat_grid`) whose file carries a Serato
//! constant grid, prints the stored BPM, the BPM `dsp::bpm_from_beats` fits
//! from the same stored beats, and Serato's. Read-only: copies the database
//! (`.db` + `-wal` + `-shm`) to a temp file and only reads the audio files.
//!
//!   SPINDEX_DIAG_DB="C:\Users\...\spindex.db" \
//!     cargo test --release --test bpm_vs_serato_live -- --ignored --nocapture

use spindex::core::serato_beatgrid::Beatgrid;
use spindex::core::{db, dsp, metadata};

#[test]
#[ignore]
fn bpm_vs_serato() {
    let Ok(src) = std::env::var("SPINDEX_DIAG_DB") else {
        eprintln!("SPINDEX_DIAG_DB not set, skipping");
        return;
    };
    let tmp = std::env::temp_dir().join("spindex-bpm-diag.db");
    for suffix in ["", "-wal", "-shm"] {
        let to = format!("{}{suffix}", tmp.display());
        let _ = std::fs::remove_file(&to);
        let copied = std::fs::copy(format!("{src}{suffix}"), &to);
        if suffix.is_empty() {
            copied.expect("copying the database");
        }
    }
    let conn = db::open(&tmp).expect("opening the copy");
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM track WHERE beat_grid IS NOT NULL AND is_missing = 0")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .flatten()
        .collect();

    let (mut n, mut old_err, mut new_err, mut new_within_01, mut old_within_01) = (0, 0.0, 0.0, 0, 0);
    for id in ids {
        let Ok((_, abs)) = db::track_root_and_abs_path(&conn, id) else { continue };
        let Ok(Some(Beatgrid::Constant { bpm: serato, .. })) = metadata::read_serato_beatgrid(&abs) else {
            continue;
        };
        let stored: Option<f64> = conn
            .query_row("SELECT bpm FROM track WHERE id = ?1", [id], |r| r.get(0))
            .ok()
            .flatten();
        let beats = db::beat_grid(&conn, id).ok().flatten().unwrap_or_default();
        let (Some(stored), Some(fitted)) = (stored, dsp::bpm_from_beats(&beats)) else { continue };
        let serato = serato as f64;
        // Compare within an octave: Serato and SpinDex may pick half/double time.
        let off = |b: f64| [b, b * 2.0, b / 2.0].iter().map(|x| (x - serato).abs()).fold(f64::MAX, f64::min);
        n += 1;
        old_err += off(stored);
        new_err += off(fitted);
        old_within_01 += (off(stored) <= 0.1) as usize;
        new_within_01 += (off(fitted) <= 0.1) as usize;
        println!(
            "serato {serato:7.2}  stored {stored:7.2} ({:+.2})  fitted {fitted:7.2} ({:+.2})  {}",
            off(stored),
            off(fitted),
            abs.file_name().unwrap_or_default().to_string_lossy()
        );
    }
    println!("\n{n} tracks with both a SpinDex beat grid and a Serato constant grid");
    if n > 0 {
        println!(
            "mean |error|  stored {:.3}  fitted {:.3}\nwithin 0.1 BPM  stored {old_within_01}/{n}  fitted {new_within_01}/{n}",
            old_err / n as f64,
            new_err / n as f64
        );
    }
}
