//! Live diagnostic: where do SpinDex's beats and the grid it would write sit
//! relative to Serato's own grid? For every analyzed track whose file (or, if
//! SpinDex already wrote a grid, its first-touch backup) carries a Serato
//! constant grid, prints:
//!
//! * the median phase offset of SpinDex's detected beats against Serato's
//!   beat lines, over the first and the last minute (constant vs drifting),
//! * the offset of the grid `serato_beatgrid::build` would write, at its
//!   anchor and at the end of the track,
//! * how many detected beats sit exactly on the model's 20 ms frame grid.
//!
//! Positive = SpinDex is LATER than Serato. Read-only: copies the database
//! (`.db` + `-wal` + `-shm`) to a temp file and only reads the audio files.
//!
//!   SPINDEX_DIAG_DB="C:\Users\...\spindex.db" \
//!     cargo test --release --test grid_phase_vs_serato_live -- --ignored --nocapture

use spindex::core::serato_beatgrid::{self, Beatgrid};
use spindex::core::{db, metadata};

/// `x` folded into [-p/2, p/2).
fn wrap(x: f64, p: f64) -> f64 {
    (x + p / 2.0).rem_euclid(p) - p / 2.0
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[v.len() / 2])
}

fn ms(x: Option<f64>) -> String {
    x.map_or("     -".into(), |v| format!("{:+6.1}", v * 1000.0))
}

#[test]
#[ignore]
fn grid_phase_vs_serato() {
    let Ok(src) = std::env::var("SPINDEX_DIAG_DB") else {
        eprintln!("SPINDEX_DIAG_DB not set, skipping");
        return;
    };
    let tmp = std::env::temp_dir().join("spindex-phase-diag.db");
    for suffix in ["", "-wal", "-shm"] {
        let to = format!("{}{suffix}", tmp.display());
        let _ = std::fs::remove_file(&to);
        let copied = std::fs::copy(format!("{src}{suffix}"), &to);
        if suffix.is_empty() {
            copied.expect("copying the database");
        }
    }
    let conn = db::open(&tmp).expect("opening the copy");
    let rows: Vec<(i64, Option<f64>, Option<f64>, Option<String>)> = conn
        .prepare(
            "SELECT id, duration_secs, tempo_drift_pct, format FROM track
             WHERE beat_grid IS NOT NULL AND downbeat_grid IS NOT NULL AND is_missing = 0",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .flatten()
        .collect();

    println!(
        "{:>5} {:>7} {:>7} | beats@start beats@end | grid@anchor grid@end | on20ms  track",
        "fmt", "serato", "bpm"
    );
    let (mut start_all, mut end_all, mut anchor_all, mut gend_all) = (vec![], vec![], vec![], vec![]);
    for (id, duration, drift, format) in rows {
        let Ok((root, abs)) = db::track_root_and_abs_path(&conn, id) else { continue };
        // Serato's own grid: the backup if SpinDex already rewrote the file.
        let rel = abs.strip_prefix(&root).map(|r| r.to_path_buf()).unwrap_or_default();
        let backup = root.join("_SpinDexBackups_").join(&rel);
        let source = if backup.exists() { &backup } else { &abs };
        let Ok(Some(Beatgrid::Constant { anchor_sec, bpm })) = metadata::read_serato_beatgrid(source)
        else {
            continue;
        };
        let (a, sb) = (anchor_sec as f64, bpm as f64);
        let p = 60.0 / sb;
        let beats = db::beat_grid(&conn, id).ok().flatten().unwrap_or_default();
        let downbeats = db::downbeat_grid(&conn, id).ok().flatten().unwrap_or_default();
        if beats.len() < 8 {
            continue;
        }
        let last = *beats.last().unwrap_or(&0.0);
        let start = median(beats.iter().filter(|&&t| t < beats[0] + 60.0).map(|&t| wrap(t - a, p)).collect());
        let end = median(beats.iter().filter(|&&t| t > last - 60.0).map(|&t| wrap(t - a, p)).collect());
        let on20 = beats.iter().filter(|&&t| ((t * 50.0) - (t * 50.0).round()).abs() < 1e-3).count();

        let (ganchor, gend, gbpm) = match serato_beatgrid::build(&downbeats, &beats, drift, serato_beatgrid::timing_offset(&abs)) {
            Ok(Beatgrid::Constant { anchor_sec, bpm }) => {
                let (ga, gb) = (anchor_sec as f64, bpm as f64);
                let end_t = duration.unwrap_or(last);
                // the SpinDex grid line nearest the end of the track
                let gp = 60.0 / gb;
                let line_end = ga + ((end_t - ga) / gp).round() * gp;
                (Some(wrap(ga - a, p)), Some(wrap(line_end - a, p)), Some(gb))
            }
            _ => (None, None, None),
        };
        start_all.extend(start);
        end_all.extend(end);
        anchor_all.extend(ganchor);
        gend_all.extend(gend);
        println!(
            "{:>5} {:>7.2} {:>7} |    {}    {} |     {}   {} | {:>3}/{:<3} {}{}",
            format.unwrap_or_default(),
            sb,
            gbpm.map_or("-".into(), |b| format!("{b:.2}")),
            ms(start),
            ms(end),
            ms(ganchor),
            ms(gend),
            on20,
            beats.len(),
            abs.file_name().unwrap_or_default().to_string_lossy(),
            if source == &backup { "  [backup]" } else { "" },
        );
    }
    let n = start_all.len();
    println!("\n{n} tracks with a SpinDex grid and a Serato constant grid");
    let abs_med = |v: &Vec<f64>| median(v.iter().map(|x| x.abs()).collect());
    println!(
        "median signed / |abs| (ms): beats@start {} / {}  beats@end {} / {}  grid@anchor {} / {}  grid@end {} / {}",
        ms(median(start_all.clone())),
        ms(abs_med(&start_all)),
        ms(median(end_all.clone())),
        ms(abs_med(&end_all)),
        ms(median(anchor_all.clone())),
        ms(abs_med(&anchor_all)),
        ms(median(gend_all.clone())),
        ms(abs_med(&gend_all)),
    );
}

/// Analyzes stick files that carry a Serato constant grid, fresh, and reports
/// per format how the detected beats and the grid `serato_beatgrid::build`
/// writes sit against Serato's, at the start and at the end of the track,
/// and whether both put bar 1 on the same beat.
///
///   SPINDEX_DIAG_DIR="D:\" SPINDEX_DIAG_PER_FORMAT=8 ///     cargo test --release --test grid_phase_vs_serato_live -- --ignored --nocapture fresh
#[test]
#[ignore]
fn fresh_phase_vs_serato() {
    use spindex::core::{analysis, dsp};
    let Ok(dir) = std::env::var("SPINDEX_DIAG_DIR") else {
        eprintln!("SPINDEX_DIAG_DIR not set, skipping");
        return;
    };
    let per: usize = std::env::var("SPINDEX_DIAG_PER_FORMAT").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
    let sc = analysis::find_sidecars().expect("sidecars");
    let mut taken: std::collections::HashMap<String, usize> = Default::default();
    // per format: beats@start, grid@start, grid@end, bar-1 agreements, n
    let mut summary: std::collections::BTreeMap<String, (Vec<f64>, Vec<f64>, Vec<f64>, usize, usize)> =
        Default::default();
    println!("{:>5} {:>7} {:>8} | beats@start | grid@start grid@end | bar  track", "fmt", "serato", "grid bpm");
    for e in walkdir::WalkDir::new(&dir).into_iter().flatten() {
        let path = e.path();
        let s = path.to_string_lossy();
        if s.contains("_SpinDexBackups_") || s.contains("_Serato_") || !e.file_type().is_file() {
            continue;
        }
        let ext = path.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
        let fmt = if ext == "aif" { "aiff".to_string() } else { ext };
        if !["mp3", "wav", "aiff", "flac", "m4a"].contains(&fmt.as_str()) {
            continue;
        }
        if *taken.get(&fmt).unwrap_or(&0) >= per {
            continue;
        }
        // a grid SpinDex already wrote says nothing about Serato's placement
        let backup_exists = path
            .ancestors()
            .any(|a| a.join("_SpinDexBackups_").exists());
        let Ok(Some(Beatgrid::Constant { anchor_sec, bpm })) = metadata::read_serato_beatgrid(path) else {
            continue;
        };
        if backup_exists && (bpm * 100.0).fract().abs() > 1e-3 {
            continue;
        }
        let Ok(res) = analysis::analyze_file(&sc, path) else { continue };
        let (Some(beats), Some(downbeats)) = (res.beats, res.downbeats) else { continue };
        if beats.len() < 16 {
            continue;
        }
        let (a, p) = (anchor_sec as f64, 60.0 / bpm as f64);
        let rough = dsp::fitted_beat_period(&beats).unwrap_or(p);
        if (rough / p - 1.0).abs() > 0.02 {
            println!("{fmt:>5} {:>7.2} tempo disagrees (SpinDex {:.2}), skipped  {}", bpm, 60.0 / rough, path.display());
            continue;
        }
        *taken.entry(fmt.clone()).or_default() += 1;
        let last = *beats.last().unwrap_or(&0.0);
        let start = median(beats.iter().filter(|&&t| t < beats[0] + 30.0).map(|&t| wrap(t - a, p)).collect());
        let built = serato_beatgrid::build(&downbeats, &beats, None, serato_beatgrid::timing_offset(path));
        let Ok(Beatgrid::Constant { anchor_sec: ga, bpm: gb }) = built else {
            println!("{fmt:>5} {:>7.2} no constant grid: {built:?}  {}", bpm, path.display());
            continue;
        };
        let (ga, gp) = (ga as f64, 60.0 / gb as f64);
        let line_end = ga + ((last - ga) / gp).round() * gp;
        let (gs, ge) = (wrap(ga - a, p), wrap(line_end - a, p));
        let bar_ok = wrap(ga - a, 4.0 * p).abs() < p / 2.0;
        let row = summary.entry(fmt.clone()).or_default();
        row.0.extend(start);
        row.1.push(gs);
        row.2.push(ge);
        row.3 += bar_ok as usize;
        row.4 += 1;
        println!(
            "{fmt:>5} {:>7.2} {:>8.3} |    {}   |   {}    {} | {}  {}",
            bpm,
            gb,
            ms(start),
            ms(Some(gs)),
            ms(Some(ge)),
            if bar_ok { " ok" } else { "BAR" },
            path.file_name().unwrap_or_default().to_string_lossy()
        );
    }
    println!("
per format, median signed (|abs|) in ms against Serato:");
    for (fmt, (bs, gs, ge, bars, n)) in summary {
        let absm = |v: &Vec<f64>| median(v.iter().map(|x| x.abs()).collect());
        println!(
            "{fmt:>5} n={n:<2} beats@start {} ({})  grid@start {} ({})  grid@end {} ({})  bar 1 agrees {bars}/{n}",
            ms(median(bs.clone())), ms(absm(&bs)),
            ms(median(gs.clone())), ms(absm(&gs)),
            ms(median(ge.clone())), ms(absm(&ge)),
        );
    }
}
