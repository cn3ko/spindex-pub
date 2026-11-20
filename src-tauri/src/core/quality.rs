//! Pure quality-assessment DSP: integrated loudness (EBU R128 / BS.1770-4),
//! clipping detection, spectral cutoff, and the compact spectrogram for the
//! quality panel. All functions take full-band PCM — the analysis sidecar's
//! 22 050 Hz feed cannot see a 16 kHz cutoff, so callers decode separately
//! at QUALITY_SAMPLE_RATE.

use rustfft::{num_complex::Complex, FftPlanner};

pub const QUALITY_SAMPLE_RATE: u32 = 44_100;

/// One biquad section, direct form II transposed.
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

/// Stage 1 of the K-weighting filter: high shelf (+4 dB above ~1.5 kHz).
/// BS.1770 fixes the response, not the 48 kHz coefficients — both stages are
/// re-derived for the actual sample rate via the RBJ cookbook (the pyloudnorm
/// parameterization: f0 = 1681.97 Hz, G = +3.9998 dB, Q = 0.7072).
fn k_shelf(fs: f64) -> Biquad {
    let (f0, g, q) = (1_681.974_450_955_533, 3.999_843_853_973_347, 0.707_175_2);
    let a = 10f64.powf(g / 40.0);
    let w0 = std::f64::consts::TAU * f0 / fs;
    let alpha = w0.sin() / (2.0 * q);
    let (cw, sa) = (w0.cos(), 2.0 * a.sqrt() * alpha);
    let a0 = (a + 1.0) - (a - 1.0) * cw + sa;
    Biquad {
        b0: (a * ((a + 1.0) + (a - 1.0) * cw + sa)) / a0,
        b1: (-2.0 * a * ((a - 1.0) + (a + 1.0) * cw)) / a0,
        b2: (a * ((a + 1.0) + (a - 1.0) * cw - sa)) / a0,
        a1: (2.0 * ((a - 1.0) - (a + 1.0) * cw)) / a0,
        a2: ((a + 1.0) - (a - 1.0) * cw - sa) / a0,
        z1: 0.0, z2: 0.0,
    }
}

/// Stage 2: RLB high-pass (f0 = 38.135 Hz, Q = 0.5003).
fn k_highpass(fs: f64) -> Biquad {
    let (f0, q) = (38.135_470_876_002_09, 0.500_327_05);
    let w0 = std::f64::consts::TAU * f0 / fs;
    let alpha = w0.sin() / (2.0 * q);
    let cw = w0.cos();
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

/// Streaming per-channel state for the BS.1770-4 meter: K-weighting filters
/// plus a running sum of k-weighted x^2 for the current 100 ms quarter-hop.
struct ChannelState {
    shelf: Biquad,
    hp: Biquad,
    quarter_energy: f64,
}

/// BS.1770-4 integrated loudness over N channels (channel weight 1.0 each,
/// summed — correlated stereo reads ~3 LU above its mono downmix, matching
/// ffmpeg loudnorm and hardware meters). K-weighting, 400 ms blocks with 75%
/// overlap, −70 LUFS absolute gate, then a −10 LU relative gate. None for
/// silence or less than one block of audio.
///
/// Streams the samples through the biquads once, accumulating a per-channel
/// running sum for each 100 ms quarter-hop and combining four consecutive
/// quarters into one 400 ms block — no full K-weighted signal is ever
/// materialized, only one energy-sum-per-quarter per channel.
pub fn integrated_lufs(channels: &[&[f32]], sample_rate: u32) -> Option<f64> {
    let n = channels.first()?.len();
    if channels.is_empty() || channels.iter().any(|c| c.len() != n) {
        return None;
    }
    let fs = sample_rate as f64;
    let block = (0.4 * fs) as usize;
    let quarter = (block / 4).max(1);
    if n < block || block == 0 {
        return None;
    }

    let mut states: Vec<ChannelState> = channels
        .iter()
        .map(|_| ChannelState { shelf: k_shelf(fs), hp: k_highpass(fs), quarter_energy: 0.0 })
        .collect();
    // Mean-square-per-sample (channels summed) of each completed quarter-hop.
    let mut quarter_sums: Vec<f64> = Vec::with_capacity(n / quarter + 1);
    let mut in_quarter = 0usize;

    for i in 0..n {
        for (c, ch) in channels.iter().enumerate() {
            let x = ch[i] as f64;
            let st = &mut states[c];
            let y = st.hp.process(st.shelf.process(x));
            st.quarter_energy += y * y;
        }
        in_quarter += 1;
        if in_quarter >= quarter {
            let sum: f64 = states.iter_mut().map(|s| std::mem::replace(&mut s.quarter_energy, 0.0)).sum();
            quarter_sums.push(sum / quarter as f64);
            in_quarter = 0;
        }
    }

    if quarter_sums.len() < 4 {
        return None;
    }
    let mut block_ms = Vec::with_capacity(quarter_sums.len() - 3);
    for i in 0..=quarter_sums.len() - 4 {
        block_ms.push(quarter_sums[i..i + 4].iter().sum::<f64>() / 4.0);
    }
    let loudness = |ms: f64| -0.691 + 10.0 * ms.max(1e-12).log10();

    // absolute gate at −70 LUFS
    let above_abs: Vec<f64> = block_ms.iter().copied().filter(|&ms| loudness(ms) > -70.0).collect();
    if above_abs.is_empty() {
        return None;
    }
    // relative gate at −10 LU under the mean of the survivors
    let rel_threshold = loudness(above_abs.iter().sum::<f64>() / above_abs.len() as f64) - 10.0;
    let gated: Vec<f64> = above_abs.into_iter().filter(|&ms| loudness(ms) > rel_threshold).collect();
    if gated.is_empty() {
        return None;
    }
    Some(loudness(gated.iter().sum::<f64>() / gated.len() as f64))
}

/// Fraction of samples belonging to runs of >= 3 consecutive samples at the
/// digital ceiling (|x| >= 0.999). Isolated full-scale peaks are legal
/// (limiters produce them); sustained runs mean the waveform was flattened.
pub fn clip_ratio(pcm: &[f32]) -> f64 {
    if pcm.is_empty() {
        return 0.0;
    }
    let mut clipped = 0usize;
    let mut run = 0usize;
    for &s in pcm {
        if s.abs() >= 0.999 {
            run += 1;
        } else {
            if run >= 3 {
                clipped += run;
            }
            run = 0;
        }
    }
    if run >= 3 {
        clipped += run;
    }
    clipped as f64 / pcm.len() as f64
}

pub const SPEC_TIME: usize = 256;
pub const SPEC_FREQ: usize = 128;
pub const SPEC_FMIN: f64 = 50.0;
const QN_FFT: usize = 2048;

pub struct SpectralStats {
    /// Onset of a detected lossy band-limit edge (the last frequency still at
    /// full strength before the cliff). None = no edge found: full-bandwidth
    /// content, silence, or too-short input.
    pub cutoff_hz: Option<u32>,
    /// SPEC_TIME x SPEC_FREQ u8 dB-scaled, row-major [time][band], band 0 lowest.
    /// Always exactly SPEC_TIME*SPEC_FREQ bytes (zeros when input too short).
    pub spectrogram: Vec<u8>,
}

/// SPEC_TIME evenly spaced Hann-windowed power spectra: each becomes one
/// spectrogram column (128 log-spaced bands, dB scaled -80..0 -> 0..255) and
/// all are averaged for cutoff detection (median of 1-8 kHz bins minus 40 dB
/// is the noise-floor reference; the highest bin above it is the cutoff).
pub fn spectral_stats(pcm: &[f32], sample_rate: u32) -> SpectralStats {
    let empty = SpectralStats { cutoff_hz: None, spectrogram: vec![0; SPEC_TIME * SPEC_FREQ] };
    if pcm.len() < QN_FFT {
        return empty;
    }
    let fs = sample_rate as f64;
    let n_bins = QN_FFT / 2 + 1;
    let window: Vec<f32> = (0..QN_FFT)
        .map(|i| {
            let x = std::f64::consts::TAU * i as f64 / QN_FFT as f64;
            (0.5 * (1.0 - x.cos())) as f32
        })
        .collect();

    // log-spaced band edges SPEC_FMIN .. fs/2, as FFT-bin indices
    let f_hi = fs / 2.0;
    let band_edges: Vec<usize> = (0..=SPEC_FREQ)
        .map(|b| {
            let f = SPEC_FMIN * (f_hi / SPEC_FMIN).powf(b as f64 / SPEC_FREQ as f64);
            ((f * QN_FFT as f64 / fs) as usize).min(n_bins - 1)
        })
        .collect();

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(QN_FFT);
    let mut buf = vec![Complex::<f32>::default(); QN_FFT];
    let mut mean_power = vec![0f64; n_bins];
    let mut spectrogram = vec![0u8; SPEC_TIME * SPEC_FREQ];
    let max_start = pcm.len() - QN_FFT;

    for t in 0..SPEC_TIME {
        let start = max_start * t / (SPEC_TIME - 1).max(1);
        for i in 0..QN_FFT {
            buf[i] = Complex::new(pcm[start + i] * window[i], 0.0);
        }
        fft.process(&mut buf);
        let power: Vec<f64> = (0..n_bins)
            .map(|k| {
                let m = buf[k].norm() as f64 / QN_FFT as f64;
                m * m
            })
            .collect();
        for (k, p) in power.iter().enumerate() {
            mean_power[k] += p;
        }
        for b in 0..SPEC_FREQ {
            let (lo, hi) = (band_edges[b], band_edges[b + 1].max(band_edges[b] + 1));
            let hi = hi.min(n_bins);
            let avg = power[lo..hi].iter().sum::<f64>() / (hi - lo) as f64;
            let db = 10.0 * avg.max(1e-16).log10(); // power dB; -80..0 usable
            let v = ((db + 80.0) / 80.0 * 255.0).clamp(0.0, 255.0);
            spectrogram[t * SPEC_FREQ + b] = v as u8;
        }
    }

    for p in &mut mean_power {
        *p /= SPEC_TIME as f64;
    }
    let db_of = |p: f64| 10.0 * p.max(1e-16).log10();
    // reference: median dB over 1-8 kHz
    let bin_of = |f: f64| ((f * QN_FFT as f64 / fs) as usize).min(n_bins - 1);
    let mut ref_bins: Vec<f64> =
        mean_power[bin_of(1_000.0)..bin_of(8_000.0)].iter().map(|&p| db_of(p)).collect();
    if ref_bins.is_empty() {
        return SpectralStats { cutoff_hz: None, spectrogram };
    }
    // Uses the mean spectrum across all frames rather than the spec's per-frame 80%-persistence rule — equivalent for real transcodes, biased toward the loudest content on mixed material.
    ref_bins.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let reference = ref_bins[ref_bins.len() / 2];
    if reference <= -150.0 {
        return SpectralStats { cutoff_hz: None, spectrogram }; // digital silence
    }

    // Real lossy encoders leave a sharp spectral EDGE with a noise shelf
    // above it that is typically only 20-35 dB down, not gone — an absolute
    // "reference - 40 dB" threshold never fires on them (live bug: every
    // real file read 22 050 Hz). Detect the edge instead: the strongest drop
    // between 2 kHz windows either side of a candidate frequency (350 Hz
    // guard band, 5-bin smoothing), calibrated against real 128 kbps-class
    // transcodes and genuine full-band masters. Reported cutoff = the edge's
    // ONSET (last bin still within 4 dB of the pre-edge level), because the
    // score tiers act on where content stops, not the cliff's midpoint.
    let db: Vec<f64> = mean_power.iter().map(|&p| db_of(p)).collect();
    let sm: Vec<f64> = (0..n_bins)
        .map(|k| {
            let lo = k.saturating_sub(2);
            let hi = (k + 3).min(n_bins);
            db[lo..hi].iter().sum::<f64>() / (hi - lo) as f64
        })
        .collect();
    let (w, g) = (bin_of(2_000.0).max(1), bin_of(350.0).max(1));
    let (scan_lo, scan_hi) = (bin_of(10_000.0), bin_of(21_500.0).min(n_bins - g - 1));
    let mut best: Option<(usize, f64, f64)> = None; // (bin, edge dB, pre-edge level)
    for c in scan_lo..scan_hi {
        let b0 = c.saturating_sub(g + w);
        let below = sm[b0..c - g].iter().sum::<f64>() / (c - g - b0) as f64;
        let a1 = (c + g + w).min(n_bins);
        let above = sm[c + g..a1].iter().sum::<f64>() / (a1 - c - g) as f64;
        let edge = below - above;
        if best.is_none_or(|(_, e, _)| edge > e) {
            best = Some((c, edge, below));
        }
    }
    const MIN_EDGE_DB: f64 = 10.0;
    const ONSET_TOLERANCE_DB: f64 = 4.0;
    let cutoff_hz = best.filter(|&(_, e, _)| e >= MIN_EDGE_DB).map(|(c, _, below)| {
        let mut j = c;
        while j > 0 && sm[j] < below - ONSET_TOLERANCE_DB {
            j -= 1;
        }
        (j as f64 * fs / QN_FFT as f64) as u32
    });
    SpectralStats { cutoff_hz, spectrogram }
}

pub struct QualityResult {
    pub lufs: Option<f64>,
    pub clip_ratio: f64,
    pub cutoff_hz: Option<u32>,
    pub spectrogram: Vec<u8>,
}

/// All quality measurements in one call, on 44.1 kHz mono PCM. Prefer
/// `measure_stereo` when a true stereo decode is available — BS.1770
/// loudness on a mono downmix reads ~3 LU low on stereo masters.
pub fn measure(pcm: &[f32]) -> QualityResult {
    let spectral = spectral_stats(pcm, QUALITY_SAMPLE_RATE);
    QualityResult {
        lufs: integrated_lufs(&[pcm], QUALITY_SAMPLE_RATE),
        clip_ratio: clip_ratio(pcm),
        cutoff_hz: spectral.cutoff_hz,
        spectrogram: spectral.spectrogram,
    }
}

/// All quality measurements from interleaved (L R L R…) 44.1 kHz stereo PCM.
/// LUFS is measured on the real two channels per BS.1770-4; clip ratio and
/// spectral cutoff run on the mono (L+R)/2 downmix, same as before — neither
/// is channel-count sensitive the way loudness is.
pub fn measure_stereo(interleaved: &[f32]) -> QualityResult {
    let n = interleaved.len() / 2;
    let mut l = Vec::with_capacity(n);
    let mut r = Vec::with_capacity(n);
    for frame in interleaved.chunks_exact(2) {
        l.push(frame[0]);
        r.push(frame[1]);
    }
    let lufs = integrated_lufs(&[&l, &r], QUALITY_SAMPLE_RATE);

    // Mono downmix, reusing `l`'s buffer.
    for i in 0..n {
        l[i] = (l[i] + r[i]) * 0.5;
    }
    let mono = l;
    let spectral = spectral_stats(&mono, QUALITY_SAMPLE_RATE);
    QualityResult {
        lufs,
        clip_ratio: clip_ratio(&mono),
        cutoff_hz: spectral.cutoff_hz,
        spectrogram: spectral.spectrogram,
    }
}

/// 0-100 quality score. Loudness is deliberately excluded: a quiet master is
/// fixable, not damaged. Cutoff dominates (detects transcodes); a low bitrate
/// caps the score even when the cutoff looks fine; sustained clipping deducts.
pub fn quality_score(
    cutoff_hz: Option<u32>,
    clip_ratio: f64,
    format: Option<&str>,
    bitrate_kbps: Option<i64>,
) -> i64 {
    let mut score: i64 = 100;
    if let Some(c) = cutoff_hz {
        score -= match c {
            c if c >= 19_000 => 0,
            c if c >= 16_500 => 15,
            c if c >= 14_000 => 40,
            _ => 60,
        };
    }
    let lossless = matches!(format, Some("flac") | Some("wav") | Some("aiff"));
    if !lossless {
        if let Some(b) = bitrate_kbps {
            if b <= 128 {
                score = score.min(30);
            } else if b < 160 {
                score = score.min(40);
            }
        }
    }
    if clip_ratio > 0.01 {
        score -= 25;
    } else if clip_ratio > 0.001 {
        score -= 10;
    }
    score.clamp(0, 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, amp: f64, secs: f64, sr: u32) -> Vec<f32> {
        (0..(secs * sr as f64) as usize)
            .map(|i| (std::f64::consts::TAU * freq * i as f64 / sr as f64).sin() as f32 * amp as f32)
            .collect()
    }

    // BS.1770's -0.691 offset makes a 997 Hz sine measure exactly
    // 20*log10(rms): amp 1.0 -> -3.01 LUFS, amp 0.1 -> -23.01 LUFS.
    // Single-channel input, per the (unchanged-intent) mono calibration.
    #[test]
    fn lufs_of_997hz_sine_is_calibrated() {
        let l = integrated_lufs(&[&sine(997.0, 1.0, 5.0, 44_100)], 44_100).unwrap();
        assert!((l - (-3.01)).abs() < 0.3, "got {l}");
        let l = integrated_lufs(&[&sine(997.0, 0.1, 5.0, 44_100)], 44_100).unwrap();
        assert!((l - (-23.01)).abs() < 0.3, "got {l}");
    }

    // Identical L/R (dual-mono) sums per-channel energy per BS.1770-4, so it
    // reads ~3.01 LU above the single-channel measurement of the same tone:
    // -0.691 + 10*log10(1.0) ~= 0.0 LUFS vs -3.01 LUFS mono.
    #[test]
    fn lufs_stereo_sums_channel_energy() {
        let mono = sine(997.0, 1.0, 5.0, 44_100);
        let l = integrated_lufs(&[&mono, &mono], 44_100).unwrap();
        assert!((l - 0.0).abs() < 0.3, "got {l}");
    }

    #[test]
    fn lufs_gating_ignores_appended_silence() {
        let mut pcm = sine(997.0, 0.25, 5.0, 44_100);
        let loud = integrated_lufs(&[&pcm], 44_100).unwrap();
        pcm.extend(vec![0.0f32; 44_100 * 5]); // 5 s of digital silence
        let padded = integrated_lufs(&[&pcm], 44_100).unwrap();
        assert!((loud - padded).abs() < 0.2, "gate must drop silence: {loud} vs {padded}");
    }

    #[test]
    fn lufs_of_silence_is_none() {
        assert!(integrated_lufs(&[&vec![0.0f32; 44_100]], 44_100).is_none());
        let empty: &[f32] = &[];
        assert!(integrated_lufs(&[empty], 44_100).is_none());
        assert!(
            integrated_lufs(&[&[0.5f32; 100][..]], 44_100).is_none(),
            "shorter than one 400 ms block"
        );
    }

    #[test]
    fn lufs_mismatched_channel_lengths_is_none() {
        let a = vec![0.5f32; 44_100];
        let b = vec![0.5f32; 44_099];
        assert!(integrated_lufs(&[&a, &b], 44_100).is_none());
    }

    #[test]
    fn clip_ratio_counts_only_sustained_runs() {
        assert_eq!(clip_ratio(&[]), 0.0);
        assert_eq!(clip_ratio(&sine(440.0, 0.9, 1.0, 44_100)), 0.0, "clean signal");
        // two isolated full-scale samples: not a run of 3
        let mut pcm = vec![0.0f32; 100];
        pcm[10] = 1.0;
        pcm[50] = -1.0;
        assert_eq!(clip_ratio(&pcm), 0.0);
        // a run of 5 clipped samples out of 100
        let mut pcm = vec![0.0f32; 100];
        for i in 20..25 { pcm[i] = 1.0; }
        assert!((clip_ratio(&pcm) - 0.05).abs() < 1e-9);
        // negative rail clips too
        let mut pcm = vec![0.0f32; 100];
        for i in 0..3 { pcm[i] = -0.9995; }
        assert!((clip_ratio(&pcm) - 0.03).abs() < 1e-9);
    }

    // xorshift PRNG: deterministic white noise without a rand dependency
    fn noise(n: usize) -> Vec<f32> {
        let mut state = 0x2545F491_4F6CDD1Du64;
        (0..n)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                ((state >> 40) as f64 / (1u64 << 24) as f64 * 2.0 - 1.0) as f32 * 0.5
            })
            .collect()
    }

    // brickwall low-pass via FFT (test-only): zero all bins above cutoff_hz
    fn lowpass(pcm: &[f32], cutoff_hz: f64, sr: u32) -> Vec<f32> {
        use rustfft::{num_complex::Complex, FftPlanner};
        let n = pcm.len();
        let mut planner = FftPlanner::<f32>::new();
        let mut buf: Vec<Complex<f32>> = pcm.iter().map(|&x| Complex::new(x, 0.0)).collect();
        planner.plan_fft_forward(n).process(&mut buf);
        let k_cut = (cutoff_hz * n as f64 / sr as f64) as usize;
        for k in 0..n {
            let freq_bin = k.min(n - k); // mirror for negative frequencies
            if freq_bin > k_cut {
                buf[k] = Complex::new(0.0, 0.0);
            }
        }
        planner.plan_fft_inverse(n).process(&mut buf);
        buf.iter().map(|c| c.re / n as f32).collect()
    }

    #[test]
    fn full_band_noise_has_no_cutoff() {
        // no lossy edge anywhere -> None, which the score treats as full band
        let stats = spectral_stats(&noise(44_100 * 4), 44_100);
        assert!(stats.cutoff_hz.is_none(), "got {:?}", stats.cutoff_hz);
    }

    #[test]
    fn cutoff_detects_16k_lowpass() {
        let filtered = lowpass(&noise(44_100 * 4), 16_000.0, 44_100);
        let c = spectral_stats(&filtered, 44_100).cutoff_hz.unwrap();
        assert!((15_000..=17_000).contains(&c), "16 kHz brickwall: got {c}");
    }

    /// The live-bug shape: real lossy encoders leave a noise shelf above the
    /// cutoff that is only ~20-35 dB down, not gone. An absolute
    /// "reference - 40 dB" threshold never fires on it (every real file read
    /// 22 050 Hz / score 100); the edge detector must still find ~16 kHz.
    #[test]
    fn cutoff_detects_lowpass_with_noise_shelf() {
        let n = 44_100 * 4;
        let filtered = lowpass(&noise(n), 16_000.0, 44_100);
        let shelf = noise(n);
        // shelf 25 dB below the passband (amplitude x 0.056)
        let mixed: Vec<f32> = filtered
            .iter()
            .zip(&shelf)
            .map(|(&a, &b)| a + b * 0.056)
            .collect();
        let c = spectral_stats(&mixed, 44_100).cutoff_hz.unwrap();
        assert!((14_500..=17_000).contains(&c), "16 kHz shelf edge: got {c}");
    }

    #[test]
    fn spectral_stats_of_silence_and_short_input() {
        let s = spectral_stats(&vec![0.0f32; 44_100], 44_100);
        assert!(s.cutoff_hz.is_none());
        assert_eq!(s.spectrogram.len(), SPEC_TIME * SPEC_FREQ);
        let s = spectral_stats(&[0.1; 100], 44_100); // shorter than one FFT frame
        assert!(s.cutoff_hz.is_none());
        assert_eq!(s.spectrogram.len(), SPEC_TIME * SPEC_FREQ);
    }

    #[test]
    fn spectrogram_puts_low_sine_in_low_bands() {
        let s = spectral_stats(&sine(200.0, 0.8, 4.0, 44_100), 44_100);
        let col = &s.spectrogram[128 * SPEC_FREQ..129 * SPEC_FREQ]; // middle column
        let low_max = *col[..32].iter().max().unwrap();
        let high_max = *col[96..].iter().max().unwrap();
        assert!(low_max > 150, "200 Hz energy must land in the low bands: {low_max}");
        assert!(high_max < 60, "no energy in the top bands: {high_max}");
    }

    #[test]
    fn quality_score_table() {
        // full-band lossless, clean -> 100
        assert_eq!(quality_score(Some(21_000), 0.0, Some("flac"), Some(1411)), 100);
        // honest 320 kbps mp3 (~20 kHz cutoff) -> green
        assert_eq!(quality_score(Some(20_100), 0.0, Some("mp3"), Some(320)), 100);
        // 128 kbps-class cutoff inside a "lossless" flac: transcode -> amber/red boundary
        assert_eq!(quality_score(Some(15_800), 0.0, Some("flac"), Some(1411)), 60);
        // genuine 128 kbps mp3: cutoff penalty 40 AND bitrate cap 30
        assert_eq!(quality_score(Some(15_800), 0.0, Some("mp3"), Some(128)), 30);
        // 192 kbps-class cutoff -> amber
        assert_eq!(quality_score(Some(17_500), 0.0, Some("mp3"), Some(192)), 85);
        // heavy clipping on an otherwise clean file
        assert_eq!(quality_score(Some(21_000), 0.02, Some("wav"), Some(1411)), 75);
        // mild clipping
        assert_eq!(quality_score(Some(21_000), 0.005, Some("wav"), Some(1411)), 90);
        // sub-14k cutoff: 100-60=40, capped to 30 by the 64 kbps bitrate
        assert_eq!(quality_score(Some(11_000), 0.0, Some("mp3"), Some(64)), 30);
        // unknown cutoff (silence): format/bitrate only
        assert_eq!(quality_score(None, 0.0, Some("mp3"), Some(96)), 30);
        assert_eq!(quality_score(None, 0.0, None, None), 100);
    }

    #[test]
    fn measure_bundles_all_metrics() {
        let q = measure(&noise(44_100 * 2)); // 2 seconds of white noise
        assert!(q.lufs.is_some());
        assert!(q.clip_ratio >= 0.0); // noise will have some clipping depending on RNG
        assert!(q.cutoff_hz.is_none(), "full-band noise has no lossy edge");
        assert_eq!(q.spectrogram.len(), SPEC_TIME * SPEC_FREQ);
    }

    #[test]
    fn measure_stereo_bundles_all_metrics() {
        let mono = noise(44_100 * 2);
        let interleaved: Vec<f32> = mono.iter().flat_map(|&x| [x, x]).collect();
        let q = measure_stereo(&interleaved);
        assert!(q.lufs.is_some());
        // dual-mono reads ~3 LU above the same-signal mono measurement.
        let mono_lufs = measure(&mono).lufs.unwrap();
        assert!((q.lufs.unwrap() - mono_lufs - 3.01).abs() < 0.3);
        assert!(q.clip_ratio >= 0.0);
        assert!(q.cutoff_hz.is_none(), "full-band noise has no lossy edge");
        assert_eq!(q.spectrogram.len(), SPEC_TIME * SPEC_FREQ);
    }
}
