//! Extra colourings for the grid editor's zoomed waveform, both plain DSP on
//! the editor's 11,025 Hz decode and both one value set per millisecond:
//!
//! - drums vs tonal: per frequency of a short-window spectrogram, energy that
//!   rises above that frequency's steady background (a running median over
//!   time) counts as a hit, the rest as tonal, giving each moment's kick-type
//!   hits, snare/hat-type hits and sustained tonal sound;
//! - harmony: per ~50 ms the 24 major/minor chords are matched against the
//!   sounding notes, and the best one is given as its Camelot code, so the
//!   frontend can use the key badge colours.

use rustfft::{num_complex::Complex, FftPlanner};

/// Drums: 46 ms windows every 11.6 ms at 11,025 Hz.
const HPSS_N: usize = 512;
const HPSS_HOP: usize = 128;
/// Frames (~380 ms) over which a frequency's steady background level is the
/// median. A hit is energy rising above it. The window has to be well over
/// twice a kick's length: a house kick sustains ~120 ms, and textbook
/// harmonic/percussive separation (a ~200 ms time median, plus a median across
/// frequency that a kick's few low bins fail) read the kick body as tonal on a
/// real track.
const BACKGROUND_FRAMES: usize = 33;
/// The background is measured every this many frames (~46 ms) and reused in
/// between.
const BACKGROUND_STEP: usize = 4;
/// Percussive energy below this counts as kick, above as snare/hats.
const KICK_SPLIT_HZ: f64 = 200.0;

/// Harmony: 370 ms windows (2.7 Hz bins, enough to tell low notes apart)
/// every 46 ms.
const CHROMA_N: usize = 4096;
const CHROMA_HOP: usize = 512;
/// Notes counted: from A2 up. Lower would be the kick's 50-150 Hz sweep, which
/// dragged the chord around on every hit; bass notes still count through
/// their upper harmonics.
const CHROMA_MIN_HZ: f64 = 110.0;
const CHROMA_MAX_HZ: f64 = 2_000.0;
/// Frames either side summed before matching (±~180 ms): chords hold for
/// beats, and a wider window halved chord flips on a real track (5.1 → 2.6
/// per second among confident moments) for little loss of confident share.
const CHROMA_SMOOTH: usize = 4;
/// How far above a flat spectrum the best triad scores in plain noise: grey.
/// Measured on white noise: 0.06 median, 0.07 at the 90th percentile.
const NOISE_CLARITY: f64 = 0.07;
/// The least a track's clear-chord level (its 90th percentile) is taken to be
/// when scaling confidence, so a track with no harmony never has its noise
/// stretched into colour. Measured on a real tech-house track: 0.09 median,
/// 0.16 at the 90th percentile; a clean triad scores 0.5.
const CLARITY_FLOOR: f64 = 0.15;

/// `harmony` byte for "no clear chord".
pub const NO_CHORD: u8 = 255;

fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos()) as f32)
        .collect()
}

/// Power spectra of frames of `n` samples every `hop`, frame `f` centred on
/// sample `f * hop` (the signal is zero beyond its ends).
fn power_frames(pcm: &[f32], n: usize, hop: usize) -> Vec<Vec<f32>> {
    if pcm.is_empty() {
        return Vec::new();
    }
    let frames = pcm.len().div_ceil(hop);
    let win = hann(n);
    let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
    let mut buf = vec![Complex::new(0.0f32, 0.0); n];
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let start = (f * hop) as isize - (n / 2) as isize;
        for (i, b) in buf.iter_mut().enumerate() {
            let j = start + i as isize;
            let s = if j >= 0 && (j as usize) < pcm.len() { pcm[j as usize] } else { 0.0 };
            *b = Complex::new(s * win[i], 0.0);
        }
        fft.process(&mut buf);
        out.push(buf[..n / 2 + 1].iter().map(|c| c.norm_sqr()).collect());
    }
    out
}

fn median(window: &mut [f32]) -> f32 {
    let mid = window.len() / 2;
    *window.select_nth_unstable_by(mid, |a, b| a.total_cmp(b)).1
}

/// The frame for millisecond `ms`: the one whose centre is nearest.
fn frame_at(ms: usize, sample_rate: u32, hop: usize, frames: usize) -> usize {
    let sample = ms as f64 * sample_rate as f64 / 1000.0 + sample_rate as f64 / 2000.0;
    ((sample / hop as f64).round() as usize).min(frames.saturating_sub(1))
}

/// Per millisecond `[kick, hats, tonal]`: the percussive energy below and
/// above 200 Hz and the tonal energy, each as an amplitude scaled to its own
/// loudest moment (0-255), like the frequency bands. `n_ms` columns.
pub fn drums_tonal(pcm: &[f32], sample_rate: u32, n_ms: usize) -> Vec<u8> {
    if n_ms == 0 {
        return Vec::new();
    }
    let spec = power_frames(pcm, HPSS_N, HPSS_HOP);
    if spec.is_empty() {
        return vec![0; n_ms * 3];
    }
    let (frames, bins) = (spec.len(), spec[0].len());
    // magnitudes for the medians
    let mag: Vec<Vec<f32>> = spec.iter().map(|f| f.iter().map(|p| p.sqrt()).collect()).collect();
    let split = ((KICK_SPLIT_HZ * HPSS_N as f64 / sample_rate as f64).round() as usize).min(bins);
    let mut energy = vec![[0.0f64; 3]; frames];
    let half = BACKGROUND_FRAMES / 2;
    let mut win = Vec::with_capacity(BACKGROUND_FRAMES);
    // the bin's steady level, measured every BACKGROUND_STEP frames: the median
    // was nearly all the cost (11 s for 3 minutes in a debug build), and over a
    // 380 ms window it barely moves between neighbouring frames
    let samples = frames.div_ceil(BACKGROUND_STEP);
    let mut background = vec![vec![0.0f32; bins]; samples];
    for (s, row) in background.iter_mut().enumerate() {
        let f = s * BACKGROUND_STEP;
        for (b, out) in row.iter_mut().enumerate() {
            win.clear();
            win.extend((f.saturating_sub(half)..(f + half + 1).min(frames)).map(|g| mag[g][b]));
            *out = median(&mut win);
        }
    }
    for f in 0..frames {
        let row = &background[((f + BACKGROUND_STEP / 2) / BACKGROUND_STEP).min(samples - 1)];
        for b in 0..bins {
            // whatever rises above the bin's steady level is a hit
            let background = row[b];
            let m = mag[f][b];
            let mask = if m > 0.0 { (1.0 - (background * background) / (m * m)).max(0.0) } else { 0.0 };
            let e = spec[f][b] as f64;
            let perc = e * mask as f64;
            energy[f][if b < split { 0 } else { 1 }] += perc;
            energy[f][2] += e - perc;
        }
    }
    let mut max = [0.0f64; 3];
    for e in &energy {
        for k in 0..3 {
            max[k] = max[k].max(e[k].sqrt());
        }
    }
    let mut out = Vec::with_capacity(n_ms * 3);
    for m in 0..n_ms {
        let e = energy[frame_at(m, sample_rate, HPSS_HOP, frames)];
        for k in 0..3 {
            let v = if max[k] > 0.0 { e[k].sqrt() / max[k] } else { 0.0 };
            out.push((v * 255.0).round() as u8);
        }
    }
    out
}

/// The Camelot code of a chord as a byte: `(number - 1) * 2`, plus 1 for B
/// (major). A major chord on pitch class `root` (C = 0) is that major key, a
/// minor chord the minor key with the same root.
pub fn camelot_index(root: usize, minor: bool) -> u8 {
    // the relative major of a minor key sits three semitones up
    let major_root = if minor { (root + 3) % 12 } else { root % 12 };
    // C major = 8B; each fifth up (+7 semitones) is one Camelot step up
    let number = ((7 * major_root) % 12 + 7) % 12 + 1;
    ((number - 1) * 2 + usize::from(!minor)) as u8
}

/// Per millisecond `[camelot, confidence]`: the major or minor chord that best
/// matches the sounding notes (`camelot_index`, or `NO_CHORD` in silence) and
/// how clearly it stands out (0-255; a flat, noise-like spectrum reads 0).
pub fn harmony(pcm: &[f32], sample_rate: u32, n_ms: usize) -> Vec<u8> {
    if n_ms == 0 {
        return Vec::new();
    }
    let spec = power_frames(pcm, CHROMA_N, CHROMA_HOP);
    if spec.is_empty() {
        return [NO_CHORD, 0].repeat(n_ms);
    }
    let hz_per_bin = sample_rate as f64 / CHROMA_N as f64;
    // pitch class of every bin in range
    let classes: Vec<Option<usize>> = (0..spec[0].len())
        .map(|b| {
            let f = b as f64 * hz_per_bin;
            (CHROMA_MIN_HZ..=CHROMA_MAX_HZ).contains(&f).then(|| {
                let midi = 69.0 + 12.0 * (f / 440.0).log2();
                (midi.round() as i64).rem_euclid(12) as usize
            })
        })
        .collect();
    let chroma: Vec<[f64; 12]> = spec
        .iter()
        .map(|frame| {
            let mut c = [0.0f64; 12];
            for (p, class) in frame.iter().zip(&classes) {
                if let Some(k) = class {
                    c[*k] += (*p as f64).sqrt();
                }
            }
            c
        })
        .collect();
    // (camelot, how far the best triad scores above a flat spectrum, energy)
    let frame_chords: Vec<(u8, f64, f64)> = (0..chroma.len())
        .map(|f| {
            // a few frames either side: chords hold for beats, single hits
            // should not repaint them
            let mut c = [0.0f64; 12];
            for g in f.saturating_sub(CHROMA_SMOOTH)..(f + CHROMA_SMOOTH + 1).min(chroma.len()) {
                for k in 0..12 {
                    c[k] += chroma[g][k];
                }
            }
            let total: f64 = chroma[f].iter().sum();
            let norm = c.iter().map(|v| v * v).sum::<f64>().sqrt();
            if norm <= 0.0 {
                return (NO_CHORD, 0.0, 0.0);
            }
            // cosine with each triad (root, third, fifth all 1)
            let mut best = (0usize, false, f64::MIN);
            for root in 0..12 {
                for minor in [false, true] {
                    let third = (root + if minor { 3 } else { 4 }) % 12;
                    let score = (c[root] + c[third] + c[(root + 7) % 12]) / (norm * 3f64.sqrt());
                    if score > best.2 {
                        best = (root, minor, score);
                    }
                }
            }
            // equal energy in all twelve classes scores 0.5 against any triad
            (camelot_index(best.0, best.1), (best.2 - 0.5).max(0.0), total)
        })
        .collect();
    // silence gets no chord: under 1% of the loudest frame's note energy
    let loudest = frame_chords.iter().map(|c| c.2).fold(0.0f64, f64::max);
    let audible = |total: f64| loudest > 0.0 && total >= loudest * 0.01;
    // Confidence runs from noise level (grey) to this track's clearest moments
    // (full colour; a full mix never matches a bare triad), but that top is
    // never taken below CLARITY_FLOOR, so a track with no harmony at all
    // (noise, drums only) stays grey.
    let mut clarity: Vec<f64> = frame_chords.iter().filter(|c| audible(c.2)).map(|c| c.1).collect();
    clarity.sort_by(|a, b| a.total_cmp(b));
    let p90 = clarity.get(clarity.len() * 9 / 10).copied().unwrap_or(0.0);
    let top = p90.max(CLARITY_FLOOR);
    let mut out = Vec::with_capacity(n_ms * 2);
    for m in 0..n_ms {
        let (code, clear, total) = frame_chords[frame_at(m, sample_rate, CHROMA_HOP, frame_chords.len())];
        if audible(total) {
            let conf = ((clear - NOISE_CLARITY) / (top - NOISE_CLARITY)).clamp(0.0, 1.0);
            out.extend([code, (conf * 255.0).round() as u8]);
        } else {
            out.extend([NO_CHORD, 0]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 11_025;

    fn tones(freqs: &[f64], secs: f64) -> Vec<f32> {
        let fs = SR as f64;
        (0..(fs * secs) as usize)
            .map(|i| {
                let t = i as f64 / fs;
                (freqs.iter().map(|f| (std::f64::consts::TAU * f * t).sin()).sum::<f64>()
                    / freqs.len() as f64
                    * 0.5) as f32
            })
            .collect()
    }

    /// Share of column `k` among the three drums/tonal columns over ms `a..b`.
    fn share(d: &[u8], a: usize, b: usize, k: usize) -> f64 {
        let sum = |c: usize| (a..b).map(|m| d[m * 3 + c] as f64).sum::<f64>();
        sum(k) / (sum(0) + sum(1) + sum(2))
    }

    #[test]
    fn a_steady_tone_is_tonal() {
        let pcm = tones(&[440.0], 2.0);
        let d = drums_tonal(&pcm, SR, 2000);
        assert_eq!(d.len(), 6000);
        assert!(share(&d, 500, 1500, 2) > 0.8, "tonal share {}", share(&d, 500, 1500, 2));
    }

    /// A house kick as measured on a real track: a sine sweeping 150 → 50 Hz
    /// that holds for ~40 ms and dies away over ~130 ms. (A kick that decays
    /// in 30 ms is no test: the real ones sustain long enough to look tonal.)
    fn kick_at(pcm: &mut [f32], start: usize) {
        let fs = SR as f64;
        let mut phase = 0.0f64;
        for i in 0..(fs * 0.15) as usize {
            let t = i as f64 / fs;
            let f = 50.0 + 100.0 * (-t / 0.015).exp();
            phase += std::f64::consts::TAU * f / fs;
            let env = if t < 0.04 { 1.0 } else { (-(t - 0.04) / 0.035).exp() };
            pcm[start + i] += (phase.sin() * env * 0.9) as f32;
        }
    }

    #[test]
    fn kicks_and_hat_clicks_land_in_their_own_columns() {
        // Each column is scaled to its own loudest moment, so this needs both
        // sounds in one track: kicks on the beat, a single-sample click
        // (broadband, hat-like) halfway between.
        let mut pcm = vec![0.0f32; SR as usize * 2];
        for k in 0..4 {
            kick_at(&mut pcm, k * SR as usize / 2);
            pcm[k * SR as usize / 2 + SR as usize / 4] = 0.9;
        }
        let d = drums_tonal(&pcm, SR, 2000);
        let col = |ms: usize, c: usize| d[ms * 3 + c];
        // third kick at 1000 ms, its click at 1250 ms
        assert!(col(1010, 0) > col(1010, 1), "at the kick: kick {} hats {}", col(1010, 0), col(1010, 1));
        assert!(col(1250, 1) > col(1250, 0), "at the click: hats {} kick {}", col(1250, 1), col(1250, 0));
        assert!(col(1250, 1) > col(1250, 2), "the click is not tonal: {}", col(1250, 2));
    }

    #[test]
    fn a_falling_low_thump_is_a_kick() {
        let mut pcm = vec![0.0f32; SR as usize * 2];
        for hit in 0..4 {
            kick_at(&mut pcm, hit * SR as usize / 2);
        }
        let d = drums_tonal(&pcm, SR, 2000);
        let (a, b) = (1000, 1040); // the third hit's first 40 ms
        assert!(share(&d, a, b, 0) > share(&d, a, b, 1), "kick {} hats {}", share(&d, a, b, 0), share(&d, a, b, 1));
        assert!(share(&d, a, b, 0) > share(&d, a, b, 2), "kick {} tonal {}", share(&d, a, b, 0), share(&d, a, b, 2));
    }

    #[test]
    fn a_kick_over_a_steady_bassline_is_still_a_kick() {
        // house: a sustained 55 Hz bass under a kick every 500 ms. The kick
        // shares the bass's bins and lasts ~120 ms; it must still read as a
        // drum hit, and the bass between the kicks as tonal.
        let mut pcm: Vec<f32> = tones(&[55.0], 2.0).iter().map(|s| s * 0.6).collect();
        for k in 0..4 {
            kick_at(&mut pcm, k * SR as usize / 2);
        }
        let d = drums_tonal(&pcm, SR, 2000);
        let col = |ms: usize, c: usize| d[ms * 3 + c];
        assert!(col(1015, 0) > col(1015, 2), "at the kick: kick {} tonal {}", col(1015, 0), col(1015, 2));
        assert!(col(1300, 2) > col(1300, 0), "between kicks: tonal {} kick {}", col(1300, 2), col(1300, 0));
    }

    #[test]
    fn camelot_codes_follow_the_wheel() {
        assert_eq!(camelot_index(0, false), 15); // C major = 8B
        assert_eq!(camelot_index(9, true), 14); // A minor = 8A
        assert_eq!(camelot_index(7, false), 17); // G major = 9B
        assert_eq!(camelot_index(5, false), 13); // F major = 7B
        assert_eq!(camelot_index(11, false), 1); // B major = 1B
        assert_eq!(camelot_index(8, true), 0); // G# minor = 1A
    }

    #[test]
    fn a_chord_reads_as_its_camelot_code() {
        let a_minor = tones(&[220.0, 261.63, 329.63], 2.0);
        let h = harmony(&a_minor, SR, 2000);
        assert_eq!(h.len(), 4000);
        assert_eq!(h[1000 * 2], 14, "A minor = 8A");
        assert!(h[1000 * 2 + 1] > 128, "a clean chord is confident: {}", h[1000 * 2 + 1]);
        let c_major = tones(&[261.63, 329.63, 392.0], 2.0);
        assert_eq!(harmony(&c_major, SR, 2000)[1000 * 2], 15, "C major = 8B");
    }

    #[test]
    fn kicks_do_not_pull_the_chord_away() {
        // an A minor pad with a kick every 500 ms: at the kicks it is still 8A
        let mut pcm = tones(&[220.0, 261.63, 329.63], 3.0);
        for k in 0..6 {
            kick_at(&mut pcm, k * SR as usize / 2);
        }
        let h = harmony(&pcm, SR, 3000);
        for at in [1010, 1020, 1500, 1520, 2010] {
            assert_eq!(h[at * 2], 14, "at {at} ms: {:?}", camelot_name(h[at * 2]));
            assert!(h[at * 2 + 1] > 128, "at {at} ms the chord is still clear: {}", h[at * 2 + 1]);
        }
    }

    #[test]
    fn noise_never_looks_like_a_chord() {
        // deterministic white noise
        let mut x = 12345u32;
        let pcm: Vec<f32> = (0..SR as usize * 3)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                ((x >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.5
            })
            .collect();
        let h = harmony(&pcm, SR, 3000);
        let mut conf: Vec<u8> = (0..3000).map(|m| h[m * 2 + 1]).collect();
        conf.sort();
        assert!(conf[2700] < 100, "90% of noise stays greyish: p90 confidence {}", conf[2700]);
    }

    fn camelot_name(i: u8) -> String {
        if i == NO_CHORD { "none".into() } else { format!("{}{}", i / 2 + 1, if i % 2 == 1 { "B" } else { "A" }) }
    }

    #[test]
    fn silence_has_no_chord_and_nothing_is_empty() {
        let mut pcm = vec![0.0f32; SR as usize];
        pcm.extend(tones(&[220.0, 261.63, 329.63], 1.0));
        let h = harmony(&pcm, SR, 2000);
        assert_eq!(h[200 * 2], NO_CHORD);
        assert_eq!(h[1500 * 2], 14);
        assert!(harmony(&[], SR, 0).is_empty());
        assert!(drums_tonal(&[], SR, 0).is_empty());
    }
}
