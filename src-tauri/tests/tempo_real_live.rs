//! Diagnostic: measure how well the warp actually flattens a REAL track.
//! Ignored by default. Never writes near the source — output goes to temp.
//!
//!   SPINDEX_TEMPO_FILE="D:\Library WAV\track.wav" \
//!     cargo test --release --test tempo_real_live -- --ignored --nocapture
//!
//! The synthetic click-track test (tempo_live.rs) flattens 4.33% -> 0.56%.
//! Real music reportedly does far worse, so this runs the same pipeline the
//! stabilize worker runs — full-beats grid, plan_warp, one ffmpeg pass — and
//! re-measures the OUTPUT with a full-beats grid to see what actually landed.
use std::path::Path;

use spindex::core::analysis::{self, Sidecars};
use spindex::core::analysis::analyze_file;
use spindex::core::tempo::{self, WarpPlan};

fn full_grid(sc: &Sidecars, path: &Path) -> Vec<f64> {
    analysis::analyze_beats_full(sc, path)
        .expect("full-beats analysis")
        .beats
        .unwrap_or_default()
}

fn describe(label: &str, beats: &[f64]) -> (Option<f64>, Option<f64>) {
    let drift = tempo::drift_pct(beats);
    let bpm = tempo::dominant_bpm(beats);
    println!(
        "{label:<22} beats={:<5} dominant={:>7}  drift={:>7}",
        beats.len(),
        bpm.map(|b| format!("{b:.2}")).unwrap_or("-".into()),
        drift.map(|d| format!("{d:.2}%")).unwrap_or("-".into()),
    );
    (bpm, drift)
}

#[test]
#[ignore]
fn warping_a_real_track_flattens_its_grid() {
    let src = std::env::var("SPINDEX_TEMPO_FILE").expect("set SPINDEX_TEMPO_FILE");
    let src = std::path::PathBuf::from(src);
    assert!(src.is_file(), "not a file: {}", src.display());
    let sc = analysis::find_sidecars().expect("analyzer + models");

    let dir = std::env::temp_dir().join(format!("spindex-tempo-real-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // The app stores the SAMPLED grid (normal Analyze) and derives
    // tempo_drift_pct from it; the warp uses a FULL grid. If the two disagree
    // the displayed figure is an artefact, not the track wandering.
    let sampled = analyze_file(&sc, &src).expect("sampled analysis").beats.unwrap_or_default();
    describe("SAMPLED (what the app stores)", &sampled);

    let beats = full_grid(&sc, &src);
    let (bpm, drift) = describe("BEFORE (full grid)", &beats);
    if let Ok(dump) = std::env::var("SPINDEX_TEMPO_DUMP") {
        let j = format!(
            "{{\"sampled\":{:?},\"full\":{:?}}}",
            sampled, beats
        );
        std::fs::write(&dump, j).unwrap();
        println!("  dumped grids -> {dump}");
    }
    let target = bpm.expect("a dominant tempo");
    println!("  target = {target:.3} BPM (the track's own dominant tempo)");

    // Gap histogram: an octave-flipped or half-time section shows up here and
    // would make "drift" a measurement artefact rather than real wandering.
    let gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    let mut sorted = gaps.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pick = |p: f64| sorted[((sorted.len() - 1) as f64 * p) as usize];
    println!(
        "  gaps: p5={:.4}s p25={:.4}s med={:.4}s p75={:.4}s p95={:.4}s  (max/min = {:.2}x)",
        pick(0.05), pick(0.25), pick(0.5), pick(0.75), pick(0.95),
        sorted[sorted.len() - 1] / sorted[0].max(1e-9),
    );

    let WarpPlan::Segments(segments) = tempo::plan_warp(&beats, target) else {
        panic!("planner refused this track");
    };
    println!("  plan: {} segments", segments.len());
    let ratios: Vec<String> = segments.iter().take(8).map(|s| format!("{:.3}@{:.1}s", s.ratio, s.at_secs)).collect();
    println!("  first segments: {}", ratios.join(" "));

    let script = tempo::asendcmd_script(&segments);
    std::fs::write(dir.join("spindex-warp.txt"), &script).unwrap();
    let out = dir.join("steady.flac");
    let st = std::process::Command::new(&sc.ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(&src)
        .args(["-map", "0:a", "-filter:a", &tempo::filter_arg(segments[0].ratio, Some("spindex-warp.txt"))])
        .args(["-c:a", "flac"])
        .arg(&out)
        .current_dir(&dir)
        .status()
        .unwrap();
    assert!(st.success(), "warp pass failed");

    let after = full_grid(&sc, &out);
    let (bpm2, drift2) = describe("AFTER  (full grid)", &after);

    println!();
    println!(
        "drift {:.2}% -> {:.2}%   tempo {:.2} -> {:.2} (target {:.2})",
        drift.unwrap_or(f64::NAN),
        drift2.unwrap_or(f64::NAN),
        target,
        bpm2.unwrap_or(f64::NAN),
        target,
    );

    let _ = std::fs::remove_dir_all(&dir);
}
