//! spindex-analyzer: ML analysis sidecar. Reads mono f32le PCM at 22 050 Hz
//! on stdin, writes {"bpm": <num|null>, "key_camelot": <"8A"|null>,
//! "beats": [...], "downbeats": [...], "peaks": [400 x 0-255]} on stdout.
//! Exit 0 on success, 1 on fatal error.
//!
//! Models (git-ignored, fetched by scripts/fetch-models.ps1 + converted by
//! scripts/convert-key-model.py):
//!   beat_this.onnx   — Beat This! (CPJKU, MIT), input [1,T,128] log-mel
//!   spindex-key.onnx — OpenKeyScan v3 (rekordcloud, MIT) with the CQT
//!                      baked into the graph, input [1,N] 44.1 kHz PCM

use std::io::Read as _;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use ort::session::Session;
use ort::value::TensorRef;

use spindex::core::dsp;

const MIN_BPM_SECS: usize = 30;
/// Beat This! was trained on 1500-frame (30 s) excerpts; long tracks are
/// processed in chunks with a 6-frame border, keep-first on overlaps.
const CHUNK: usize = 1500;
const BORDER: usize = 6;

fn model_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("models"));
        }
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries/models"));
    candidates.into_iter().find(|d| d.join("beat_this.onnx").is_file())
}

fn read_all_pcm() -> Result<Vec<f32>> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut bytes)
        .context("reading PCM from stdin")?;
    let mut samples = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(samples)
}

/// Sampled mode covers at most this many 30 s windows, evenly spaced.
const SAMPLE_WINDOWS: usize = 4;

/// Windows of the mel spectrogram to run the beat model on.
///
/// `full` (the `--full-beats` flag) covers the track end to end in CHUNK-sized
/// pieces overlapping by 2*BORDER — the continuous grid warping needs, since
/// holding one stretch ratio across a 30 s hole is guessing, not warping.
///
/// Without it — the normal Analyze pass — long tracks sample at most
/// SAMPLE_WINDOWS evenly spaced windows. Full coverage costs ~4x the beat
/// inference (measured 3.70 s -> 16.04 s on a 5-minute track), and neither
/// BPM (median inter-beat interval) nor drift (local tempo at several points)
/// needs every beat. The sampled islands are disjoint, so the joins between
/// them read as huge gaps — `tempo::plausible_gaps` already discards anything
/// outside 0.6-1.6x the median gap.
fn beat_windows(frames: usize, full: bool) -> Vec<(usize, usize)> {
    if full || frames <= SAMPLE_WINDOWS * CHUNK {
        let mut windows = Vec::new();
        let mut start = 0usize;
        loop {
            let end = (start + CHUNK).min(frames);
            let s = if end == frames && frames > CHUNK { end - CHUNK } else { start };
            windows.push((s, end));
            if end == frames {
                return windows;
            }
            start += CHUNK - 2 * BORDER;
        }
    }
    // Centers evenly spread over the track, avoiding the very start and end
    // where intros and outros are least representative of the groove.
    let mut windows: Vec<(usize, usize)> = Vec::new();
    for i in 0..SAMPLE_WINDOWS {
        let p = (i + 1) as f64 / (SAMPLE_WINDOWS + 1) as f64;
        let center = (frames as f64 * p) as usize;
        let s = center.saturating_sub(CHUNK / 2).min(frames - CHUNK);
        if windows.last().map(|(prev, _)| *prev != s).unwrap_or(true) {
            windows.push((s, s + CHUNK));
        }
    }
    windows
}

/// Runs Beat This! over every window and returns (beat times, downbeat times)
/// in absolute seconds. Both heads are peak-picked the same way; window
/// overlap can pick the same event twice, so anything within MIN_BEAT_GAP of
/// the previous one in its own series is dropped.
fn beat_and_downbeat_times(
    session: &mut Session,
    mel: &[f32],
    full: bool,
) -> Result<(Vec<f64>, Vec<f64>)> {
    const MIN_BEAT_GAP: f64 = 0.05;
    let frames = mel.len() / dsp::MELS;
    let mut beats: Vec<f64> = Vec::new();
    let mut downs: Vec<f64> = Vec::new();
    for (s, end) in beat_windows(frames, full) {
        let chunk = &mel[s * dsp::MELS..end * dsp::MELS];
        let len = end - s;
        let input = TensorRef::from_array_view(([1usize, len, dsp::MELS], chunk))?;
        let outputs = session.run(ort::inputs!["input_spectrogram" => input])?;

        let lo = if s == 0 { 0 } else { BORDER };
        let hi = if end == frames { len } else { len - BORDER };
        let offset = (s + lo) as f64 / dsp::FPS;

        let collect = |name: &str, out: &mut Vec<f64>| -> Result<()> {
            let (_, logits) = outputs[name]
                .try_extract_tensor::<f32>()
                .with_context(|| format!("extracting {name} logits"))?;
            for t in dsp::pick_beats(&logits[lo..hi]) {
                let abs = t + offset;
                if out.last().map(|p| abs - p > MIN_BEAT_GAP).unwrap_or(true) {
                    out.push(abs);
                }
            }
            Ok(())
        };
        collect("beat", &mut beats)?;
        collect("downbeat", &mut downs)?;
    }
    Ok((beats, downs))
}

fn key_pass(session: &mut Session, pcm44: &[f32]) -> Result<Vec<f32>> {
    let input = TensorRef::from_array_view(([1usize, pcm44.len()], pcm44))?;
    let outputs = session.run(ort::inputs!["pcm" => input])?;
    let (_, logits) = outputs["logits"]
        .try_extract_tensor::<f32>()
        .context("extracting key logits")?;
    Ok(dsp::softmax(logits))
}

/// Full-track pass plus three 60 s windows (25/50/75%); majority vote with
/// the full-track pass breaking ties; confidence = mean winner probability.
/// Returns (confidence, top-3 candidates) — the winner is candidates[0].
fn detect_key(
    session: &mut Session,
    pcm: &[f32],
) -> Result<Option<(f64, Vec<(String, f64)>)>> {
    if pcm.iter().all(|s| s.abs() < 1e-4) {
        return Ok(None); // silence
    }
    let pcm44 = dsp::upsample_2x(pcm);
    let mut passes = vec![key_pass(session, &pcm44)?];
    let win = 60 * 44_100usize;
    if pcm44.len() >= win * 3 / 2 {
        for center in [0.25f64, 0.5, 0.75] {
            let mid = (pcm44.len() as f64 * center) as usize;
            let s = mid.saturating_sub(win / 2).min(pcm44.len() - win);
            passes.push(key_pass(session, &pcm44[s..s + win])?);
        }
    }
    let Some((_, conf)) = dsp::vote_key(&passes) else {
        return Ok(None);
    };
    // top 3: voted winner first, then alternatives by mean probability
    let candidates: Vec<(String, f64)> = dsp::key_candidates(&passes, 3)
        .into_iter()
        .map(|(idx, p)| (camelot_of(idx), p))
        .collect();
    Ok(Some((conf, candidates)))
}

/// CAMELOT_MAPPING order: 0..=11 are minors 1A..12A, 12..=23 majors 1B..12B.
fn camelot_of(idx: usize) -> String {
    if idx < 12 {
        format!("{}A", idx + 1)
    } else {
        format!("{}B", idx - 11)
    }
}

fn run() -> Result<()> {
    let models = model_dir().context(
        "model files missing — run scripts/fetch-models.ps1 and scripts/convert-key-model.py",
    )?;
    // --full-beats: cover the track end to end instead of sampling. Only the
    // stabilize path asks for it; the normal Analyze pass must stay fast.
    let full_beats = std::env::args().any(|a| a == "--full-beats");
    let pcm = read_all_pcm()?;
    if pcm.is_empty() {
        bail!("no audio data on stdin");
    }

    let timings = std::env::var("SPINDEX_ANALYZER_TIMINGS").is_ok();
    let mut mark = std::time::Instant::now();
    let stage = |name: &str, mark: &mut std::time::Instant| {
        if timings {
            eprintln!("{name}: {:.2}s", mark.elapsed().as_secs_f64());
        }
        *mark = std::time::Instant::now();
    };

    let peaks = dsp::rms_peaks(&pcm);
    let band3 = dsp::band_peaks(&pcm);
    let mut bands = Vec::with_capacity(dsp::PEAK_BUCKETS * 3);
    for i in 0..dsp::PEAK_BUCKETS {
        bands.push(band3[0][i]);
        bands.push(band3[1][i]);
        bands.push(band3[2][i]);
    }
    stage("peaks", &mut mark);

    // Absolute beat times, kept for the tempo-drift analysis in the app.
    let mut beats: Vec<f64> = Vec::new();
    let mut downbeats: Vec<f64> = Vec::new();

    let bpm = if pcm.len() >= dsp::SAMPLE_RATE * MIN_BPM_SECS {
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let mut session = Session::builder()?
            .with_intra_threads(threads)
            .map_err(|e| anyhow::anyhow!("setting intra threads: {e}"))?
            .commit_from_file(models.join("beat_this.onnx"))
            .context("loading beat_this.onnx")?;
        stage("beat model load", &mut mark);
        let mel = dsp::log_mel_spectrogram(&pcm);
        stage("mel spectrogram", &mut mark);
        let (b, d) = beat_and_downbeat_times(&mut session, &mel, full_beats)?;
        stage("beat inference", &mut mark);
        let bpm = dsp::bpm_from_beats(&b);
        beats = b;
        downbeats = d;
        bpm
    } else {
        None
    };

    let key = {
        let mut session = Session::builder()?
            .commit_from_file(models.join("spindex-key.onnx"))
            .context("loading spindex-key.onnx")?;
        stage("key model load", &mut mark);
        let k = detect_key(&mut session, &pcm)?;
        stage("key inference", &mut mark);
        k
    };

    let mut json = String::from("{\"bpm\":");
    match bpm {
        Some(b) => json.push_str(&format!("{b:.1}")),
        None => json.push_str("null"),
    }
    json.push_str(",\"beats\":[");
    for (i, t) in beats.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!("{t:.3}"));
    }
    json.push_str("],\"downbeats\":[");
    for (i, t) in downbeats.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!("{t:.3}"));
    }
    json.push_str("],\"key_camelot\":");
    match &key {
        Some((_, cands)) if !cands.is_empty() => {
            json.push_str(&format!("\"{}\"", cands[0].0))
        }
        _ => json.push_str("null"),
    }
    json.push_str(",\"key_confidence\":");
    match &key {
        Some((c, _)) => json.push_str(&format!("{c:.3}")),
        None => json.push_str("null"),
    }
    json.push_str(",\"key_candidates\":");
    match &key {
        Some((_, cands)) if !cands.is_empty() => {
            json.push('[');
            for (i, (k, p)) in cands.iter().enumerate() {
                if i > 0 {
                    json.push(',');
                }
                json.push_str(&format!("{{\"key\":\"{k}\",\"p\":{p:.3}}}"));
            }
            json.push(']');
        }
        _ => json.push_str("null"),
    }
    json.push_str(",\"peaks\":[");
    for (i, p) in peaks.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&p.to_string());
    }
    json.push(']');
    json.push_str(",\"bands\":[");
    for (i, b) in bands.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&b.to_string());
    }
    json.push_str("]}");
    println!("{json}");
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sampled mode must stay bounded no matter how long the track is —
    /// that bound is the whole reason the normal Analyze pass stays fast.
    #[test]
    fn sampled_windows_are_capped_and_spread_over_the_track() {
        let frames = 20 * CHUNK; // ~10 minutes
        let w = beat_windows(frames, false);
        assert_eq!(w.len(), SAMPLE_WINDOWS);
        for win in &w {
            assert_eq!(win.1 - win.0, CHUNK, "each sample is one model window");
            assert!(win.1 <= frames, "window runs past the track");
        }
        for pair in w.windows(2) {
            assert!(pair[1].0 > pair[0].0, "samples must advance through the track");
        }
        assert!(w[0].0 > 0, "first sample skips the very start");
        assert!(w[SAMPLE_WINDOWS - 1].1 < frames, "last sample skips the very end");
    }

    #[test]
    fn full_mode_covers_the_track_end_to_end() {
        let frames = 20 * CHUNK;
        let w = beat_windows(frames, true);
        assert_eq!(w[0].0, 0, "coverage starts at the first frame");
        assert_eq!(w[w.len() - 1].1, frames, "coverage reaches the last frame");
        for pair in w.windows(2) {
            assert!(
                pair[1].0 <= pair[0].1,
                "windows must not leave a hole: {pair:?}"
            );
        }
        assert!(w.len() > SAMPLE_WINDOWS, "full coverage needs more windows");
    }

    /// A track short enough to cover fully is covered fully in both modes —
    /// sampling four 30 s windows out of a 2-minute track would be no cheaper.
    #[test]
    fn short_tracks_are_covered_fully_in_either_mode() {
        let frames = 2 * CHUNK;
        assert_eq!(beat_windows(frames, false), beat_windows(frames, true));
    }

    #[test]
    fn a_track_barely_over_the_threshold_does_not_repeat_a_window() {
        // Clamping to `frames - CHUNK` can push neighbouring samples onto the
        // same start; a duplicate would run the model twice for nothing.
        let frames = SAMPLE_WINDOWS * CHUNK + 1;
        let w = beat_windows(frames, false);
        let mut starts: Vec<usize> = w.iter().map(|(s, _)| *s).collect();
        starts.dedup();
        assert_eq!(starts.len(), w.len(), "duplicate windows: {w:?}");
    }

    #[test]
    #[ignore = "needs beat_this.onnx present locally"]
    fn beat_this_model_exposes_a_downbeat_output() {
        let Some(dir) = model_dir() else {
            eprintln!("model absent — skipping");
            return;
        };
        let session = ort::session::Session::builder()
            .unwrap()
            .commit_from_file(dir.join("beat_this.onnx"))
            .unwrap();
        let names: Vec<String> = session.outputs().iter().map(|o| o.name().to_string()).collect();
        assert!(names.iter().any(|n| n == "beat"), "outputs: {names:?}");
        assert!(
            names.iter().any(|n| n == "downbeat"),
            "no `downbeat` output — STOP, escalate to spec Approach B. outputs: {names:?}"
        );
    }

    #[test]
    #[ignore = "needs beat_this.onnx present locally"]
    fn beat_and_downbeat_times_returns_two_series() {
        // Synthetic mel: silence. Both series come back empty, not a panic.
        let mel = vec![-10.0f32; 200 * dsp::MELS];
        let mut session = ort::session::Session::builder()
            .unwrap()
            .commit_from_file(model_dir().expect("model").join("beat_this.onnx"))
            .unwrap();
        let (beats, downs) = beat_and_downbeat_times(&mut session, &mel, false).unwrap();
        assert!(beats.is_empty() && downs.is_empty());
    }
}
