//! Pure DSP for the ML analyzer sidecar: the Beat This! log-mel frontend
//! (verified against torchaudio goldens in tests/golden/), the 2x linear
//! upsampler feeding the key model, waveform RMS peaks, and the "minimal"
//! beat postprocessing from the Beat This! paper.

use std::sync::Arc;

use rustfft::{num_complex::Complex, Fft, FftPlanner};

pub const SAMPLE_RATE: usize = 22_050;
pub const N_FFT: usize = 1024;
pub const HOP: usize = 441;
pub const MELS: usize = 128;
pub const FPS: f64 = 50.0;
pub const PEAK_BUCKETS: usize = 400;
const F_MIN: f64 = 30.0;
const F_MAX: f64 = 11_000.0;

/// Slaney-scale Hz -> mel (torchaudio mel_scale="slaney").
fn hz_to_mel(f: f64) -> f64 {
    const MIN_LOG_HZ: f64 = 1000.0;
    const MIN_LOG_MEL: f64 = 15.0;
    let logstep = (6.4f64).ln() / 27.0;
    if f >= MIN_LOG_HZ {
        MIN_LOG_MEL + (f / MIN_LOG_HZ).ln() / logstep
    } else {
        3.0 * f / 200.0
    }
}

fn mel_to_hz(m: f64) -> f64 {
    const MIN_LOG_HZ: f64 = 1000.0;
    const MIN_LOG_MEL: f64 = 15.0;
    let logstep = (6.4f64).ln() / 27.0;
    if m >= MIN_LOG_MEL {
        MIN_LOG_HZ * ((m - MIN_LOG_MEL) * logstep).exp()
    } else {
        200.0 * m / 3.0
    }
}

/// Triangular slaney filterbank, norm=None, row-major (n_freqs=513, MELS)
/// to mirror torchaudio's `MelScale.fb` layout.
pub fn mel_filterbank() -> Vec<f32> {
    let n_freqs = N_FFT / 2 + 1;
    let all_freqs: Vec<f64> = (0..n_freqs)
        .map(|k| k as f64 * SAMPLE_RATE as f64 / N_FFT as f64)
        .collect();
    let m_min = hz_to_mel(F_MIN);
    let m_max = hz_to_mel(F_MAX);
    let m_pts: Vec<f64> = (0..MELS + 2)
        .map(|i| mel_to_hz(m_min + (m_max - m_min) * i as f64 / (MELS + 1) as f64))
        .collect();

    let mut fb = vec![0f32; n_freqs * MELS];
    for m in 0..MELS {
        let (lo, center, hi) = (m_pts[m], m_pts[m + 1], m_pts[m + 2]);
        for (k, &f) in all_freqs.iter().enumerate() {
            let up = (f - lo) / (center - lo);
            let down = (hi - f) / (hi - center);
            let v = up.min(down).max(0.0);
            fb[k * MELS + m] = v as f32;
        }
    }
    fb
}

fn hann_periodic(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let x = std::f64::consts::TAU * i as f64 / n as f64;
            (0.5 * (1.0 - x.cos())) as f32
        })
        .collect()
}

/// Log-mel spectrogram exactly matching beat_this's LogMelSpect:
/// center=true reflect padding, hann(periodic), magnitude STFT divided by
/// sqrt(n_fft), slaney mel (norm=None), log1p(1000 x). Row-major (T, MELS),
/// T = 1 + len/HOP.
pub fn log_mel_spectrogram(pcm: &[f32]) -> Vec<f32> {
    let n_freqs = N_FFT / 2 + 1;
    let frames = 1 + pcm.len() / HOP;
    let window = hann_periodic(N_FFT);
    let fb = mel_filterbank();
    let norm = (N_FFT as f32).sqrt();

    // center=true: reflect-pad by N_FFT/2 on both sides
    let pad = N_FFT / 2;
    let padded_len = pcm.len() + 2 * pad;
    let sample_at = |i: usize| -> f32 {
        let n = pcm.len() as isize;
        if n == 0 {
            return 0.0;
        }
        let mut idx = i as isize - pad as isize;
        if idx < 0 {
            idx = -idx;
        }
        if idx >= n {
            idx = 2 * (n - 1) - idx;
        }
        pcm[idx.clamp(0, n - 1) as usize]
    };

    let mut planner = FftPlanner::<f32>::new();
    let fft: Arc<dyn Fft<f32>> = planner.plan_fft_forward(N_FFT);
    let mut out = vec![0f32; frames * MELS];
    let mut buf = vec![Complex::default(); N_FFT];

    for t in 0..frames {
        let start = t * HOP;
        if start + N_FFT > padded_len {
            break;
        }
        for i in 0..N_FFT {
            buf[i] = Complex::new(sample_at(start + i) * window[i], 0.0);
        }
        fft.process(&mut buf);
        for k in 0..n_freqs {
            let mag = buf[k].norm() / norm;
            if mag == 0.0 {
                continue;
            }
            for m in 0..MELS {
                out[t * MELS + m] += mag * fb[k * MELS + m];
            }
        }
        for m in 0..MELS {
            out[t * MELS + m] = (1000.0 * out[t * MELS + m]).ln_1p();
        }
    }
    out
}

/// 2x linear-interpolation upsample (22.05 kHz -> 44.1 kHz for the key model;
/// its CQT only looks below ~1.4 kHz, so linear interpolation is plenty).
pub fn upsample_2x(pcm: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(pcm.len() * 2);
    for i in 0..pcm.len() {
        let a = pcm[i];
        let b = if i + 1 < pcm.len() { pcm[i + 1] } else { a };
        out.push(a);
        out.push((a + b) * 0.5);
    }
    out
}

/// Per-bucket RMS overview peaks (400 buckets, 0-255). Port of the C++
/// analyzer's peaksOf: RMS not max, so intros/breakdowns keep their shape.
pub fn rms_peaks(pcm: &[f32]) -> Vec<u8> {
    let mut peaks = vec![0u8; PEAK_BUCKETS];
    if pcm.is_empty() {
        return peaks;
    }
    let per = (pcm.len() / PEAK_BUCKETS).max(1);
    for (b, peak) in peaks.iter_mut().enumerate() {
        let start = b * per;
        if start >= pcm.len() {
            break;
        }
        let end = (start + per).min(pcm.len());
        let sum: f64 = pcm[start..end].iter().map(|&s| s as f64 * s as f64).sum();
        let rms = (sum / (end - start) as f64).sqrt().min(1.0);
        *peak = (rms * 255.0).round() as u8;
    }
    peaks
}

const BAND_SPLIT_LOW_HZ: f64 = 200.0;
const BAND_SPLIT_HIGH_HZ: f64 = 2_000.0;
/// Q pair for a 4th-order Butterworth as two cascaded RBJ biquads.
const BUTTER_Q: [f64; 2] = [0.541_196_100_146_197, 1.306_562_964_876_377];

/// One biquad section, direct form II transposed (same shape as quality.rs;
/// duplicated because that struct is private and K-weighting-specific).
struct Biquad {
    b0: f64, b1: f64, b2: f64, a1: f64, a2: f64,
    z1: f64, z2: f64,
}

impl Biquad {
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// RBJ cookbook lowpass.
fn rbj_lowpass(fs: f64, f0: f64, q: f64) -> Biquad {
    let w0 = std::f64::consts::TAU * f0 / fs;
    let (sw, cw) = (w0.sin(), w0.cos());
    let alpha = sw / (2.0 * q);
    let a0 = 1.0 + alpha;
    Biquad {
        b0: ((1.0 - cw) / 2.0) / a0,
        b1: (1.0 - cw) / a0,
        b2: ((1.0 - cw) / 2.0) / a0,
        a1: (-2.0 * cw) / a0,
        a2: (1.0 - alpha) / a0,
        z1: 0.0, z2: 0.0,
    }
}

/// RBJ cookbook highpass.
fn rbj_highpass(fs: f64, f0: f64, q: f64) -> Biquad {
    let w0 = std::f64::consts::TAU * f0 / fs;
    let (sw, cw) = (w0.sin(), w0.cos());
    let alpha = sw / (2.0 * q);
    let a0 = 1.0 + alpha;
    Biquad {
        b0: ((1.0 + cw) / 2.0) / a0,
        b1: (-(1.0 + cw)) / a0,
        b2: ((1.0 + cw) / 2.0) / a0,
        a1: (-2.0 * cw) / a0,
        a2: (1.0 - alpha) / a0,
        z1: 0.0, z2: 0.0,
    }
}

/// Per-bucket RMS of the low (<200 Hz), mid (200 Hz-2 kHz) and high (>2 kHz)
/// bands, same 400 buckets and 0-255 scaling as `rms_peaks`. Drives the
/// frequency-colored deck waveform.
pub fn band_peaks(pcm: &[f32]) -> [Vec<u8>; 3] {
    // Filter the band into a full-length buffer and immediately reduce it to
    // its 400-bucket RMS peaks, so only one full-length band buffer is ever
    // alive at a time (it drops at the end of this closure call before the
    // next band is filtered) rather than keeping low/mid/high simultaneously.
    let run_band = |mut filter: BandFilter| -> Vec<u8> {
        let filtered: Vec<f32> = pcm.iter().map(|&s| filter.process(s as f64) as f32).collect();
        rms_peaks(&filtered)
    };
    let [low, mid, high] = band_filters(SAMPLE_RATE as f64);
    [run_band(low), run_band(mid), run_band(high)]
}

/// One band of the low/mid/high split: cascaded biquads, fed one sample at a
/// time. The split frequencies live here only, so the deck's overview and the
/// grid editor's detail waveform colour the same sound the same way.
pub struct BandFilter(Vec<Biquad>);

impl BandFilter {
    pub fn process(&mut self, x: f64) -> f64 {
        self.0.iter_mut().fold(x, |x, b| b.process(x))
    }
}

/// The low (<200 Hz), mid (200 Hz-2 kHz) and high (>2 kHz) filters, each a
/// 4th-order Butterworth, for audio at `fs` Hz.
pub fn band_filters(fs: f64) -> [BandFilter; 3] {
    let low = BUTTER_Q.iter().map(|&q| rbj_lowpass(fs, BAND_SPLIT_LOW_HZ, q)).collect();
    let mid = BUTTER_Q
        .iter()
        .flat_map(|&q| [rbj_highpass(fs, BAND_SPLIT_LOW_HZ, q), rbj_lowpass(fs, BAND_SPLIT_HIGH_HZ, q)])
        .collect();
    let high = BUTTER_Q.iter().map(|&q| rbj_highpass(fs, BAND_SPLIT_HIGH_HZ, q)).collect();
    [BandFilter(low), BandFilter(mid), BandFilter(high)]
}

/// Beat This! "minimal" postprocessing: a frame is a beat when its logit
/// equals the max over a 7-frame window and is > 0; runs of adjacent peak
/// frames are merged by averaging. Returns beat times in seconds (50 fps).
pub fn pick_beats(logits: &[f32]) -> Vec<f64> {
    let n = logits.len();
    if n == 0 {
        return Vec::new();
    }
    let mut peak_frames: Vec<usize> = Vec::new();
    for i in 0..n {
        let lo = i.saturating_sub(3);
        let hi = (i + 3).min(n - 1);
        let max = logits[lo..=hi].iter().cloned().fold(f32::MIN, f32::max);
        if logits[i] > 0.0 && logits[i] == max {
            peak_frames.push(i);
        }
    }
    // deduplicate_peaks(width=1): merge frames within 1 of each other
    let mut beats = Vec::new();
    let mut i = 0;
    while i < peak_frames.len() {
        let mut j = i;
        let mut sum = peak_frames[i] as f64;
        while j + 1 < peak_frames.len() && peak_frames[j + 1] - peak_frames[j] <= 1 {
            j += 1;
            sum += peak_frames[j] as f64;
        }
        beats.push(sum / (j - i + 1) as f64 / FPS);
        i = j + 1;
    }
    beats
}

/// Numerically stable softmax.
pub fn softmax(x: &[f32]) -> Vec<f32> {
    let max = x.iter().cloned().fold(f32::MIN, f32::max);
    let exps: Vec<f32> = x.iter().map(|v| (v - max).exp()).collect();
    let sum: f32 = exps.iter().sum();
    exps.iter().map(|e| e / sum).collect()
}

/// Majority vote over per-pass softmax distributions. Pass 0 is the
/// full-track pass and breaks ties. Returns (class, mean probability the
/// passes assign to that class) — the mean is the stored key confidence.
pub fn vote_key(pass_probs: &[Vec<f32>]) -> Option<(usize, f64)> {
    if pass_probs.is_empty() {
        return None;
    }
    let argmax = |p: &Vec<f32>| {
        p.iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
            .unwrap_or(0)
    };
    let votes: Vec<usize> = pass_probs.iter().map(argmax).collect();
    let mut counts = [0usize; 24];
    for &v in &votes {
        if v < 24 {
            counts[v] += 1;
        }
    }
    let best_count = *counts.iter().max().unwrap_or(&0);
    // among top-voted classes, the full-track pass's pick wins ties;
    // fall back to the smallest index for determinism
    let winner = if counts[votes[0]] == best_count {
        votes[0]
    } else {
        (0..24).find(|&c| counts[c] == best_count).unwrap_or(votes[0])
    };
    let mean = pass_probs
        .iter()
        .map(|p| *p.get(winner).unwrap_or(&0.0) as f64)
        .sum::<f64>()
        / pass_probs.len() as f64;
    Some((winner, mean))
}

/// Ranked key candidates: the voted winner first, then the remaining
/// classes by mean probability across passes, `k` entries total. Each entry
/// is (class, mean probability).
pub fn key_candidates(pass_probs: &[Vec<f32>], k: usize) -> Vec<(usize, f64)> {
    let Some((winner, winner_conf)) = vote_key(pass_probs) else {
        return Vec::new();
    };
    let mean_of = |class: usize| {
        pass_probs
            .iter()
            .map(|p| *p.get(class).unwrap_or(&0.0) as f64)
            .sum::<f64>()
            / pass_probs.len() as f64
    };
    let mut rest: Vec<(usize, f64)> = (0..24)
        .filter(|&c| c != winner)
        .map(|c| (c, mean_of(c)))
        .collect();
    rest.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = vec![(winner, winner_conf)];
    out.extend(rest.into_iter().take(k.saturating_sub(1)));
    out
}

/// Fewer beats than this in a window and a line fit says nothing.
const MIN_FIT_BEATS: usize = 8;
/// Beats fitted as one straight line: long enough to average away the beat
/// model's 20 ms frame grid, short enough that real drift doesn't bend it.
const FIT_WINDOW_BEATS: usize = 64;

fn median_of(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let m = v.len() / 2;
    Some(if v.len() % 2 == 0 { (v[m - 1] + v[m]) / 2.0 } else { v[m] })
}

/// Least-squares slope of time over beat index.
fn slope(points: &[(f64, f64)]) -> Option<f64> {
    let n = points.len() as f64;
    let mx = points.iter().map(|p| p.0).sum::<f64>() / n;
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let den: f64 = points.iter().map(|p| (p.0 - mx).powi(2)).sum();
    (den > 0.0).then(|| points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>() / den)
}

/// Beat period of one window, in two passes.
///
/// 1. Number the beats step by step from the last accepted one: a gap of ~1
///    rough period is +1, ~2 is +2 (a dropped beat), and a gap well under one
///    period is a doubled detection and is skipped. Stepping, not dividing
///    total elapsed time, because `rough` is itself frame-quantized (0.48 s
///    for a 0.469 s beat) and that error would add up across the window.
/// 2. Renumber against the fitted period, which is now accurate, drop points
///    more than a quarter beat off the line, and fit again.
fn fit_window(beats: &[f64], rough: f64) -> Option<f64> {
    let mut points: Vec<(f64, f64)> = vec![(0.0, beats[0])];
    for &t in &beats[1..] {
        let (last_n, last_t) = points[points.len() - 1];
        let steps = (t - last_t) / rough;
        if steps < 0.75 {
            continue;
        }
        points.push((last_n + steps.round(), t));
    }
    let first = slope(&points)?;
    let t0 = beats[0];
    let mut renumbered: Vec<(f64, f64)> = Vec::with_capacity(beats.len());
    for &t in beats {
        let n = ((t - t0) / first).round();
        if renumbered.last().map_or(true, |p| n > p.0) {
            renumbered.push((n, t));
        }
    }
    let s = slope(&renumbered)?;
    let b = renumbered.iter().map(|p| p.1 - s * p.0).sum::<f64>() / renumbered.len() as f64;
    renumbered.retain(|p| (p.1 - (b + s * p.0)).abs() <= 0.25 * s);
    if renumbered.len() < MIN_FIT_BEATS {
        return None;
    }
    slope(&renumbered).filter(|s| *s > 0.0)
}

/// Beat period in seconds, fitted over beat TIMES rather than read off single
/// gaps. The beat model reports times on a 20 ms frame grid, so any one gap
/// is a whole number of frames and a median of gaps can only land on
/// 60 / (k * 0.02) BPM — 131 read as 130.435. Beats are split into runs at
/// holes (the sampled windows of a normal Analyze pass, breakdowns), each run
/// into windows of `FIT_WINDOW_BEATS`, each window fitted as a line, and the
/// median taken across windows so one misdetected section cannot move it.
pub fn fitted_beat_period(beats: &[f64]) -> Option<f64> {
    if beats.len() < MIN_FIT_BEATS {
        return None;
    }
    let gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > 0.0).collect();
    let rough = median_of(gaps)?;
    let mut periods = Vec::new();
    let mut start = 0;
    for i in 1..=beats.len() {
        if i == beats.len() || beats[i] - beats[i - 1] > rough * 1.6 {
            for w in beats[start..i].chunks(FIT_WINDOW_BEATS) {
                if w.len() >= MIN_FIT_BEATS {
                    periods.extend(fit_window(w, rough));
                }
            }
            start = i;
        }
    }
    median_of(periods)
}

/// One straight beat line `t = start + k * period` through EVERY detected
/// beat of the track, for placing a constant grid. `fitted_beat_period` takes a
/// median over short windows, which is robust for a BPM readout but leaves a
/// few hundredths of a BPM that add up to tens of milliseconds by the end of a
/// track; a single least-squares line over the whole span averages the 20 ms
/// frame quantization away. Beats are numbered from the current line (holes
/// between sampled windows keep their count because the window period is
/// already close), refitted a few times, and beats further than a quarter
/// beat off the line are dropped so a misdetected intro cannot pull it.
/// Returns `(start, period)`, `start` being the line position nearest the
/// first beat.
pub fn fitted_beat_line(beats: &[f64]) -> Option<(f64, f64)> {
    let mut period = fitted_beat_period(beats)?;
    let t0 = beats[0];
    let mut start = t0;
    let mut used: Vec<f64> = beats.to_vec();
    for _ in 0..4 {
        let points: Vec<(f64, f64)> =
            used.iter().map(|&t| (((t - start) / period).round(), t)).collect();
        let s = slope(&points).filter(|s| *s > 0.0)?;
        let b = points.iter().map(|p| p.1 - s * p.0).sum::<f64>() / points.len() as f64;
        period = s;
        start = b;
        used = beats
            .iter()
            .copied()
            .filter(|&t| {
                let k = ((t - start) / period).round();
                (t - (start + k * period)).abs() <= 0.25 * period
            })
            .collect();
        if used.len() < MIN_FIT_BEATS {
            return None;
        }
    }
    // the line position nearest the first beat
    let start = start + ((t0 - start) / period).round() * period;
    Some((start, period))
}

/// BPM from detected beat times (see `fitted_beat_period`), folded into
/// [70, 200) for display and tagging.
pub fn bpm_from_beats(beats: &[f64]) -> Option<f64> {
    let period = fitted_beat_period(beats)?;
    let mut bpm = 60.0 / period;
    while bpm >= 200.0 {
        bpm /= 2.0;
    }
    while bpm < 70.0 {
        bpm *= 2.0;
    }
    Some(bpm)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn golden(name: &str) -> serde_json::Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn mel_filterbank_matches_torchaudio_rows() {
        let g = golden("mel_fbank_rows.json");
        let fb = mel_filterbank();
        for row in ["0", "64", "127"] {
            let want: Vec<f64> = g[row]
                .as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
            let idx: usize = row.parse().unwrap();
            for (k, w) in want.iter().enumerate() {
                let got = fb[k * MELS + idx] as f64;
                assert!((got - w).abs() < 1e-4, "mel {idx} bin {k}: {got} vs {w}");
            }
        }
    }

    #[test]
    fn log_mel_of_sine_matches_torchaudio() {
        let g = golden("mel_sine_440.json");
        let frames = g["frames"].as_u64().unwrap() as usize;
        let want: Vec<f64> = g["data"]
            .as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();

        let sr = 22050usize;
        let sine: Vec<f32> = (0..sr)
            .map(|i| (std::f64::consts::TAU * 440.0 * i as f64 / sr as f64).sin() as f32 * 0.5)
            .collect();
        let got = log_mel_spectrogram(&sine);
        assert_eq!(got.len(), frames * MELS);
        let mut max_err = 0f64;
        for (a, b) in got.iter().zip(&want) {
            max_err = max_err.max((*a as f64 - b).abs());
        }
        assert!(max_err < 5e-3, "max abs error {max_err}");
    }

    #[test]
    fn upsample_2x_doubles_length_and_interpolates() {
        let out = upsample_2x(&[0.0, 1.0, 0.0]);
        assert_eq!(out.len(), 6);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[1], 0.5);
        assert_eq!(out[2], 1.0);
        assert_eq!(out[3], 0.5);
        assert_eq!(out[4], 0.0);
        assert_eq!(out[5], 0.0); // last sample repeated
    }

    #[test]
    fn rms_peaks_shape_and_dynamics() {
        // loud first half, silent second half
        let mut pcm = vec![0.8f32; 22050];
        pcm.extend(vec![0.0f32; 22050]);
        let peaks = rms_peaks(&pcm);
        assert_eq!(peaks.len(), 400);
        assert!(peaks[10] > 150, "loud region should be high");
        assert_eq!(peaks[390], 0, "silence should be zero");
        assert_eq!(rms_peaks(&[]).len(), 400);
    }

    #[test]
    fn pick_beats_finds_isolated_peaks() {
        // logits: peaks at frames 50, 100, 150 (1s, 2s, 3s at 50 fps)
        let mut logits = vec![-5.0f32; 200];
        for f in [50usize, 100, 150] {
            logits[f - 1] = 1.0;
            logits[f] = 3.0;
            logits[f + 1] = 1.0;
        }
        let beats = pick_beats(&logits);
        assert_eq!(beats.len(), 3);
        assert!((beats[0] - 1.0).abs() < 0.03);
        assert!((beats[1] - 2.0).abs() < 0.03);
        assert!((beats[2] - 3.0).abs() < 0.03);
        // sub-threshold peaks are ignored
        assert!(pick_beats(&vec![-1.0f32; 100]).is_empty());
    }

    #[test]
    fn adjacent_peak_frames_are_merged() {
        let mut logits = vec![-5.0f32; 100];
        logits[40] = 2.0;
        logits[41] = 2.0; // plateau: same pooled max, both retained -> merged
        let beats = pick_beats(&logits);
        assert_eq!(beats.len(), 1);
        assert!((beats[0] - 0.81).abs() < 0.011, "mean of frames 40,41 = 40.5/50");
    }

    #[test]
    fn softmax_normalizes() {
        let p = softmax(&[1.0, 1.0, 1.0, 1.0]);
        assert!((p.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!((p[0] - 0.25).abs() < 1e-6);
        let p = softmax(&[10.0, 0.0]);
        assert!(p[0] > 0.99);
    }

    fn mk_pass(winner: usize, p: f32) -> Vec<f32> {
        let mut v = vec![(1.0 - p) / 23.0; 24];
        v[winner] = p;
        v
    }

    #[test]
    fn vote_key_majority_and_confidence() {
        // 3 passes vote class 2, one votes class 5
        let passes = vec![mk_pass(2, 0.8), mk_pass(2, 0.6), mk_pass(5, 0.9), mk_pass(2, 0.7)];
        let (idx, conf) = vote_key(&passes).unwrap();
        assert_eq!(idx, 2);
        // mean prob of class 2 across passes: (0.8+0.6+~0.0043+0.7)/4
        assert!((conf - 0.526).abs() < 0.01, "got {conf}");
    }

    #[test]
    fn vote_key_tie_broken_by_full_track_pass() {
        // 2 vs 2: full-track (pass 0) says class 7
        let passes = vec![mk_pass(7, 0.6), mk_pass(3, 0.6), mk_pass(3, 0.6), mk_pass(7, 0.6)];
        assert_eq!(vote_key(&passes).unwrap().0, 7);
        // single pass (short track)
        let (idx, conf) = vote_key(&[mk_pass(1, 0.9)]).unwrap();
        assert_eq!(idx, 1);
        assert!((conf - 0.9).abs() < 1e-3);
        assert!(vote_key(&[]).is_none());
    }

    #[test]
    fn key_candidates_winner_first_then_by_mean_prob() {
        // passes vote class 2 (majority), class 5 has the single strongest pass
        let passes = vec![mk_pass(2, 0.8), mk_pass(2, 0.6), mk_pass(5, 0.9), mk_pass(2, 0.7)];
        let cands = key_candidates(&passes, 3);
        assert_eq!(cands.len(), 3);
        assert_eq!(cands[0].0, 2, "voted winner first");
        assert_eq!(cands[1].0, 5, "runner-up by mean prob");
        assert!(cands[1].1 < cands[0].1);
        assert!(cands[2].1 <= cands[1].1);
        assert!(key_candidates(&[], 3).is_empty());
    }

    #[test]
    fn bpm_median_and_octave_fold() {
        // 128 bpm: gap 0.46875 s
        let beats: Vec<f64> = (0..20).map(|i| i as f64 * 60.0 / 128.0).collect();
        let bpm = bpm_from_beats(&beats).unwrap();
        assert!((bpm - 128.0).abs() < 0.01);

        // 55 bpm folds up to 110
        let slow: Vec<f64> = (0..20).map(|i| i as f64 * 60.0 / 55.0).collect();
        assert!((bpm_from_beats(&slow).unwrap() - 110.0).abs() < 0.01);

        // 280 bpm folds down to 140
        let fast: Vec<f64> = (0..20).map(|i| i as f64 * 60.0 / 280.0).collect();
        assert!((bpm_from_beats(&fast).unwrap() - 140.0).abs() < 0.01);

        assert_eq!(bpm_from_beats(&beats[..5]), None, "fewer than 8 beats");
    }

    /// Beats at `bpm` snapped to the beat model's 20 ms frame grid, in four
    /// 30 s islands with gaps between them — the shape the normal Analyze
    /// pass produces (FEZZO - Million Dollar: true 131, median said 130.435).
    fn sampled_quantized(bpm: f64) -> Vec<f64> {
        let period = 60.0 / bpm;
        let mut out = Vec::new();
        for start in [50.0, 114.1, 178.9, 243.3] {
            let first = (start / period).ceil() as i64;
            let last = ((start + 30.0) / period).floor() as i64;
            out.extend((first..=last).map(|k| (k as f64 * period / FRAME_SECS).round() * FRAME_SECS));
        }
        out
    }

    const FRAME_SECS: f64 = 0.02;

    #[test]
    fn bpm_is_not_quantized_to_whole_frames() {
        let beats = sampled_quantized(131.0);
        let bpm = bpm_from_beats(&beats).unwrap();
        assert!((bpm - 131.0).abs() < 0.05, "got {bpm} (median of 20 ms gaps gives 130.435)");
    }

    #[test]
    fn bpm_survives_a_dropped_and_a_doubled_beat() {
        let mut beats = sampled_quantized(128.0);
        beats.remove(10); // dropped
        let extra = (beats[40] + beats[41]) / 2.0;
        beats.insert(41, extra); // doubled
        let bpm = bpm_from_beats(&beats).unwrap();
        assert!((bpm - 128.0).abs() < 0.05, "got {bpm}");
    }

    #[test]
    fn one_misdetected_island_does_not_move_the_bpm() {
        // Mirrors FEZZO's third window reading 131.95 while the others read 131.0x.
        let good = sampled_quantized(126.0);
        let mut beats: Vec<f64> = good.iter().copied().filter(|&t| t < 170.0 || t > 215.0).collect();
        let period = 60.0 / 126.0 * 0.99;
        beats.extend((0..60).map(|k| 180.0 + k as f64 * period));
        beats.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let bpm = bpm_from_beats(&beats).unwrap();
        assert!((bpm - 126.0).abs() < 0.05, "got {bpm}");
    }

    fn sine(freq: f64, secs: f64) -> Vec<f32> {
        let n = (SAMPLE_RATE as f64 * secs) as usize;
        (0..n)
            .map(|i| (std::f64::consts::TAU * freq * i as f64 / SAMPLE_RATE as f64).sin() as f32 * 0.5)
            .collect()
    }

    /// Mean of the non-zero buckets, as f64.
    fn band_mean(band: &[u8]) -> f64 {
        let nz: Vec<f64> = band.iter().filter(|&&v| v > 0).map(|&v| v as f64).collect();
        if nz.is_empty() { 0.0 } else { nz.iter().sum::<f64>() / nz.len() as f64 }
    }

    #[test]
    fn band_peaks_60hz_is_low() {
        let [low, mid, high] = band_peaks(&sine(60.0, 5.0));
        assert!(band_mean(&low) > 3.0 * band_mean(&mid).max(1.0));
        assert!(band_mean(&low) > 3.0 * band_mean(&high).max(1.0));
    }

    #[test]
    fn band_peaks_1khz_is_mid() {
        let [low, mid, high] = band_peaks(&sine(1_000.0, 5.0));
        assert!(band_mean(&mid) > 3.0 * band_mean(&low).max(1.0));
        assert!(band_mean(&mid) > 3.0 * band_mean(&high).max(1.0));
    }

    #[test]
    fn band_peaks_8khz_is_high() {
        let [low, mid, high] = band_peaks(&sine(8_000.0, 5.0));
        assert!(band_mean(&high) > 3.0 * band_mean(&low).max(1.0));
        assert!(band_mean(&high) > 3.0 * band_mean(&mid).max(1.0));
    }

    #[test]
    fn band_peaks_empty_is_zero() {
        let [low, mid, high] = band_peaks(&[]);
        assert!(low.iter().chain(&mid).chain(&high).all(|&v| v == 0));
        assert_eq!(low.len(), PEAK_BUCKETS);
    }
}
