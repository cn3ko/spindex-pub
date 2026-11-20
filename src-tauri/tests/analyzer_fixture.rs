//! Fixture tests for the spindex-analyzer sidecar. These need ffmpeg in
//! src-tauri/binaries/ and the ONNX models in src-tauri/binaries/models/
//! and silently skip when they are absent, so `cargo test` stays green on
//! machines that haven't run the fetch scripts.

use std::f64::consts::TAU;
use std::io::Write as _;
use std::process::{Command, Stdio};

use spindex::core::analysis::{self, AnalysisResult, PEAK_BUCKETS, SAMPLE_RATE};

fn sidecars() -> Option<analysis::Sidecars> {
    let sc = analysis::find_sidecars();
    if sc.is_none() {
        eprintln!("skipping analyzer fixture test: sidecar or models not present");
    }
    sc
}

/// Feeds raw f32le mono PCM straight into spindex-analyzer (no ffmpeg).
fn run_analyzer(analyzer: &std::path::Path, samples: &[f32]) -> AnalysisResult {
    let mut child = Command::new(analyzer)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn analyzer");
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(&bytes)
        .expect("write pcm");
    let out = child.wait_with_output().expect("analyzer output");
    assert!(out.status.success(), "analyzer exited with {}", out.status);
    serde_json::from_slice(&out.stdout).expect("valid analyzer json")
}

/// A-minor cadence (Am–Dm–E–Am) with sawtooth-ish harmonics — close enough
/// to real music for the key CNN, unambiguously A minor. Camelot: 8A.
fn a_minor_progression(secs: u32) -> Vec<f32> {
    let chords: [&[f64]; 4] = [
        &[220.0, 261.63, 329.63],        // Am
        &[293.66, 349.23, 440.0],        // Dm
        &[329.63, 415.30, 493.88],       // E major (dominant)
        &[220.0, 261.63, 329.63],        // Am
    ];
    let n = (SAMPLE_RATE * secs) as usize;
    let chord_len = n / 4;
    (0..n)
        .map(|i| {
            let t = i as f64 / SAMPLE_RATE as f64;
            let chord = chords[(i / chord_len).min(3)];
            let mut v = 0.0f64;
            for &f in chord {
                for h in 1..=4u32 {
                    v += (TAU * f * h as f64 * t).sin() * 0.25 / h as f64;
                }
            }
            (v * 0.5) as f32
        })
        .collect()
}

/// Percussive clicks every 0.5 s => 120 BPM.
fn click_track_120(secs: u32) -> Vec<f32> {
    let n = (SAMPLE_RATE * secs) as usize;
    let period = SAMPLE_RATE as usize / 2; // 0.5 s
    let click_len = SAMPLE_RATE as usize / 100; // 10 ms burst
    (0..n)
        .map(|i| {
            let in_click = i % period < click_len;
            if in_click {
                // decaying 1 kHz burst
                let k = (i % period) as f64;
                let env = 1.0 - k / click_len as f64;
                ((TAU * 1000.0 * k / SAMPLE_RATE as f64).sin() * 0.9 * env) as f32
            } else {
                0.0
            }
        })
        .collect()
}

#[test]
fn detects_key_of_a_minor_progression() {
    let Some(sc) = sidecars() else { return };
    let result = run_analyzer(&sc.analyzer, &a_minor_progression(40));
    assert_eq!(result.peaks.len(), PEAK_BUCKETS);
    assert_eq!(result.key_camelot.as_deref(), Some("8A"));
    let conf = result.key_confidence.expect("confidence present");
    assert!(conf > 0.0 && conf <= 1.0, "confidence in (0,1], got {conf}");
}

#[test]
fn detects_tempo_of_120_bpm_clicks() {
    let Some(sc) = sidecars() else { return };
    let result = run_analyzer(&sc.analyzer, &click_track_120(40));
    let bpm = result.bpm.expect("bpm detected");
    assert!(
        (bpm - 120.0).abs() <= 3.0,
        "expected ~120 bpm, got {bpm}"
    );
}

/// Full pipeline: ffmpeg decode | analyzer, driven through analyze_file,
/// using a generated wav file.
#[test]
fn analyze_file_runs_full_pipeline_on_wav() {
    let Some(sc) = sidecars() else { return };
    let samples = a_minor_progression(35);
    let dir = std::env::temp_dir().join(format!("spindex-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav_path = dir.join("triad.wav");
    write_wav_f32(&wav_path, &samples);

    let result = analysis::analyze_file(&sc, &wav_path).expect("pipeline ok");
    assert_eq!(result.peaks.len(), PEAK_BUCKETS);
    assert_eq!(result.key_camelot.as_deref(), Some("8A"));
    assert!(result.peaks.iter().any(|&p| p > 10), "waveform not flat");

    let _ = std::fs::remove_dir_all(&dir);
}

fn write_wav_f32(path: &std::path::Path, samples: &[f32]) {
    let data_len = (samples.len() * 4) as u32;
    let mut wav: Vec<u8> = Vec::with_capacity(58 + samples.len() * 4);
    wav.extend(b"RIFF");
    wav.extend((36 + data_len).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(3u16.to_le_bytes()); // IEEE float
    wav.extend(1u16.to_le_bytes()); // mono
    wav.extend(SAMPLE_RATE.to_le_bytes());
    wav.extend((SAMPLE_RATE * 4).to_le_bytes());
    wav.extend(4u16.to_le_bytes());
    wav.extend(32u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(data_len.to_le_bytes());
    for s in samples {
        wav.extend(s.to_le_bytes());
    }
    std::fs::write(path, wav).expect("write wav");
}
