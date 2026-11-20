//! End-to-end tempo warp on a synthetic drifting click track. Ignored by
//! default; needs the bundled ffmpeg. Run with:
//!   cargo test --release --test tempo_live -- --ignored --nocapture
//!
//! Unlike the ML-restoration pipeline, this feature is self-verifying: the
//! success criterion (a steady grid) is exactly what we can measure.
use std::path::Path;
use std::process::Command;

use spindex::core::analysis::find_ffmpeg;
use spindex::core::tempo::{self, WarpPlan};

/// Beat times for a click track whose interval ramps linearly.
fn ramping_beats(n: usize, from: f64, to: f64) -> Vec<f64> {
    let mut t = 0.0;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(t);
        let f = i as f64 / (n - 1) as f64;
        t += from + (to - from) * f;
    }
    out
}

/// Writes a 16-bit mono 44.1 kHz WAV: silence with a 20 ms 1 kHz burst at each
/// beat time. Synthesized here rather than rendered by ffmpeg's `volume`
/// expression — a 160-term `between()` chain is both fragile and slow.
fn write_click_wav(path: &Path, beats: &[f64]) {
    const RATE: usize = 44_100;
    let total = ((beats[beats.len() - 1] + 1.0) * RATE as f64) as usize;
    let mut pcm = vec![0i16; total];
    for b in beats {
        let start = (*b * RATE as f64) as usize;
        let burst = RATE / 50; // 20 ms
        for i in 0..burst {
            let idx = start + i;
            if idx >= total {
                break;
            }
            // fade the burst out so the click has an onset but no click-off
            let env = 1.0 - i as f64 / burst as f64;
            let phase = 2.0 * std::f64::consts::PI * 1000.0 * i as f64 / RATE as f64;
            pcm[idx] = (phase.sin() * env * 0.9 * i16::MAX as f64) as i16;
        }
    }

    let data_len = (pcm.len() * 2) as u32;
    let mut out: Vec<u8> = Vec::with_capacity(44 + pcm.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&(RATE as u32).to_le_bytes());
    out.extend_from_slice(&((RATE * 2) as u32).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in &pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, out).expect("writing the click fixture");
}

/// Click onsets from a mono 8 kHz decode: the first sample of each run that
/// crosses half of full scale, with a 100 ms refractory period.
fn detect_click_times(ffmpeg: &Path, wav: &Path) -> Vec<f64> {
    const RATE: usize = 8000;
    let out = Command::new(ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(wav)
        .args(["-f", "f32le", "-ac", "1", "-ar", "8000", "-"])
        .output()
        .unwrap();
    assert!(out.status.success(), "decode for detection failed");
    let samples: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    let mut times = Vec::new();
    let mut last = -1.0f64;
    for (i, s) in samples.iter().enumerate() {
        if s.abs() > 0.5 {
            let t = i as f64 / RATE as f64;
            if t - last > 0.1 {
                times.push(t);
                last = t;
            }
        }
    }
    times
}

#[test]
#[ignore]
fn warping_a_drifting_click_track_makes_it_steady() {
    let Some(ffmpeg) = find_ffmpeg() else {
        panic!("ffmpeg not found — run scripts/fetch-ffmpeg.ps1");
    };
    let dir = std::env::temp_dir().join(format!("spindex-tempo-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 120 BPM (0.5 s) drifting to ~126 BPM (0.476 s) over 160 beats (~78 s).
    let beats = ramping_beats(160, 0.5, 0.476);
    let drift_before = tempo::drift_pct(&beats).expect("fixture drifts");
    println!("before: drift {drift_before:.2}%");
    assert!(drift_before > 3.0, "fixture must actually drift");

    let clicks = dir.join("clicks.wav");
    write_click_wav(&clicks, &beats);

    // Sanity: the detector must find the grid we just wrote, or a later
    // "the warp destroyed the clicks" failure would be unattributable.
    let detected_before = detect_click_times(&ffmpeg, &clicks);
    assert!(
        detected_before.len() >= beats.len() - 2,
        "detector found {} of {} written clicks",
        detected_before.len(),
        beats.len()
    );

    // Plan and execute the warp to a constant 120 BPM.
    let WarpPlan::Segments(segments) = tempo::plan_warp(&beats, 120.0) else {
        panic!("planner refused a plain ramp");
    };
    println!("plan: {} segments", segments.len());
    let script = tempo::asendcmd_script(&segments);
    std::fs::write(dir.join("spindex-warp.txt"), &script).unwrap();

    let out = dir.join("steady.wav");
    let st = Command::new(&ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(&clicks)
        .args([
            "-filter:a",
            &tempo::filter_arg(segments[0].ratio, Some("spindex-warp.txt")),
        ])
        .args(["-c:a", "pcm_s16le"])
        .arg(&out)
        .current_dir(&dir) // filter syntax can't carry a Windows absolute path
        .status()
        .unwrap();
    assert!(st.success(), "warp pass failed");
    assert!(out.exists() && std::fs::metadata(&out).unwrap().len() > 0);

    // Re-detect the clicks by envelope and confirm the grid is now steady.
    let detected = detect_click_times(&ffmpeg, &out);
    println!("detected {} beats after warp", detected.len());
    assert!(detected.len() > 100, "clicks survived the warp");
    let drift_after = tempo::drift_pct(&detected).expect("enough beats");
    println!("after: drift {drift_after:.2}%");
    assert!(
        drift_after < drift_before / 3.0,
        "warp must flatten the grid: before {drift_before:.2}% after {drift_after:.2}%"
    );
    let bpm_after = tempo::dominant_bpm(&detected).expect("enough beats");
    println!("after: {bpm_after:.2} BPM");
    assert!(
        (bpm_after - 120.0).abs() < 1.5,
        "warped tempo should land on target, got {bpm_after:.2}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
