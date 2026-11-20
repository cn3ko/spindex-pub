//! Tempo stability and warp planning (pure — no I/O, no ffmpeg).
//!
//! Part A derives a drift figure from a detected beat grid; Part B turns the
//! same grid into a piecewise-constant stretch plan for ffmpeg's `rubberband`
//! filter. Spec: docs/superpowers/specs/2026-07-29-tempo-normalization-design.md

use std::cmp::Ordering;

/// Gaps either side of the centre used to smooth beat-detection jitter
/// (~10 ms), which would otherwise read as ±2% of phantom tempo movement.
pub const SMOOTH_WINDOW: usize = 20;
/// Fewer beats than this and there is not enough signal to say anything.
pub const MIN_BEATS: usize = 16;
/// A new warp segment is emitted only when the required ratio moves at least
/// this much; hundreds of abrupt phase-vocoder changes chatter audibly.
pub const SEGMENT_EPS: f64 = 0.010;
/// Health-panel threshold: ~2 BPM at 128, where beatmatching starts to fight.
pub const UNSTEADY_PCT: f64 = 1.5;
/// Past this the movement is usually deliberate (a transition track) or a
/// detection failure; the dialog warns before flattening it.
pub const WARN_DRIFT_PCT: f64 = 10.0;
/// Musical bounds for a stretch. A plan needing more is refused, not clamped.
pub const MIN_RATIO: f64 = 0.5;
pub const MAX_RATIO: f64 = 2.0;

#[derive(Debug, Clone, PartialEq)]
pub struct WarpSegment {
    /// Input-timeline position where this ratio takes effect.
    pub at_secs: f64,
    /// rubberband tempo scale factor; > 1 plays faster (shortens).
    pub ratio: f64,
}

#[derive(Debug)]
pub enum WarpPlan {
    NotEnoughBeats,
    /// The stretch required falls outside [MIN_RATIO, MAX_RATIO]; carries the
    /// offending ratio so the caller can name it.
    OutOfRange(f64),
    Segments(Vec<WarpSegment>),
}

fn sorted(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    v
}

/// Value at `p` (0-1) of an already-sorted slice. Empty input yields 0.0.
/// Uses nearest-rank method: round to nearest index.
fn percentile(sorted_vals: &[f64], p: f64) -> f64 {
    if sorted_vals.is_empty() {
        return 0.0;
    }
    let idx = ((sorted_vals.len() - 1) as f64 * p).round() as usize;
    sorted_vals[idx.min(sorted_vals.len() - 1)]
}

/// Inter-beat gaps with detection dropouts removed, as (gap index, seconds).
///
/// A gap far from the track's own median is a hole where the model found no
/// beats — a breakdown, an ambient intro, an a cappella outro — not evidence
/// about tempo. Including it would both inflate drift and, in `plan_warp`,
/// demand an absurd stretch across the hole.
fn plausible_gaps(beats: &[f64]) -> Vec<(usize, f64)> {
    let raw: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    if raw.is_empty() {
        return Vec::new();
    }
    let med = percentile(&sorted(raw.clone()), 0.5);
    if med <= 0.0 {
        return Vec::new();
    }
    raw.iter()
        .enumerate()
        .filter(|(_, g)| **g > med * 0.6 && **g < med * 1.6)
        .map(|(i, g)| (i, *g))
        .collect()
}

/// Local tempo at each plausible gap, as the trimmed mean of the surrounding
/// `SMOOTH_WINDOW` plausible gaps. Returns (gap index, interval seconds).
///
/// The trimmed mean (drop bottom/top 25%, average the middle 50%) is robust against
/// outliers without the failure modes of pure median (bimodal jitter) or pure mean
/// (unbounded influence). It smooths on intervals directly, as median-of-intervals
/// does, to preserve the brief's algorithm.
fn local_intervals(beats: &[f64]) -> Vec<(usize, f64)> {
    let gaps = plausible_gaps(beats);
    let radius = SMOOTH_WINDOW / 2;
    gaps.iter()
        .enumerate()
        .map(|(pos, (idx, _))| {
            let lo = pos.saturating_sub(radius);
            let hi = (pos + radius + 1).min(gaps.len());
            let mut window: Vec<f64> = gaps[lo..hi].iter().map(|(_, g)| *g).collect();
            window.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));

            let trim_count = window.len() / 4;
            let trimmed = if window.len() > trim_count * 2 {
                &window[trim_count..window.len() - trim_count]
            } else {
                &window
            };

            let mean = if trimmed.is_empty() {
                0.0
            } else {
                trimmed.iter().sum::<f64>() / trimmed.len() as f64
            };
            (*idx, mean)
        })
        .collect()
}

/// Dominant tempo in BPM: the same line fit as the stored BPM
/// (`dsp::fitted_beat_period`), so a grid, a stabilize target and the table
/// all agree. Deliberately NOT folded into [70, 200): stabilize compares it
/// against local tempo, and a doubled target would demand a 2x stretch.
pub fn dominant_bpm(beats: &[f64]) -> Option<f64> {
    if beats.len() < MIN_BEATS {
        return None;
    }
    crate::core::dsp::fitted_beat_period(beats).map(|p| 60.0 / p)
}

/// How much the tempo wanders, as a percentage of the dominant tempo:
/// `(p95 - p5) / median * 100` over the smoothed local tempi. Percentiles
/// rather than min/max so a single mis-detected beat cannot dominate.
pub fn drift_pct(beats: &[f64]) -> Option<f64> {
    if beats.len() < MIN_BEATS {
        return None;
    }
    let intervals = local_intervals(beats);
    if intervals.len() < 2 {
        return None;
    }
    let tempi = sorted(intervals.iter().map(|(_, v)| 60.0 / *v).collect());
    let med = percentile(&tempi, 0.5);
    if med <= 0.0 {
        return None;
    }
    Some((percentile(&tempi, 0.95) - percentile(&tempi, 0.05)) / med * 100.0)
}

/// Piecewise-constant stretch plan taking `beats` to a constant `target_bpm`.
///
/// The first segment always anchors at 0.0 so audio before the first detected
/// beat is covered. Gaps in the grid emit no segment, so the previous ratio
/// simply continues across them.
pub fn plan_warp(beats: &[f64], target_bpm: f64) -> WarpPlan {
    if beats.len() < MIN_BEATS || target_bpm <= 0.0 {
        return WarpPlan::NotEnoughBeats;
    }
    let target = 60.0 / target_bpm;
    let intervals = local_intervals(beats);
    if intervals.is_empty() {
        return WarpPlan::NotEnoughBeats;
    }

    let mut segments: Vec<WarpSegment> = Vec::new();
    for (idx, interval) in intervals {
        let ratio = interval / target;
        if !(MIN_RATIO..=MAX_RATIO).contains(&ratio) {
            return WarpPlan::OutOfRange(ratio);
        }
        match segments.last() {
            None => segments.push(WarpSegment { at_secs: 0.0, ratio }),
            Some(last) if (ratio - last.ratio).abs() >= SEGMENT_EPS => {
                segments.push(WarpSegment { at_secs: beats[idx], ratio })
            }
            Some(_) => {}
        }
    }
    if segments.is_empty() {
        return WarpPlan::NotEnoughBeats;
    }
    WarpPlan::Segments(segments)
}

/// `asendcmd` command-file text. The first segment's ratio rides on the
/// filter's own `tempo=` argument, so only later changes appear here.
pub fn asendcmd_script(segments: &[WarpSegment]) -> String {
    segments
        .iter()
        .skip(1)
        .map(|s| format!("{:.3} rubberband tempo {:.6};", s.at_secs, s.ratio))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `-filter:a` value. `cmd_file_name` must be a BARE file name, not a
/// path: ffmpeg's filter syntax treats `:` and `\` as separators, so a Windows
/// absolute path cannot be embedded. The worker runs ffmpeg with the temp
/// directory as its working directory instead.
pub fn filter_arg(first_ratio: f64, cmd_file_name: Option<&str>) -> String {
    match cmd_file_name {
        Some(name) => format!(
            "asendcmd=f={name},rubberband=tempo={first_ratio:.6}:transients=crisp"
        ),
        None => format!("rubberband=tempo={first_ratio:.6}:transients=crisp"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Beats exactly 0.5 s apart = a rock-steady 120 BPM.
    fn steady(n: usize, interval: f64) -> Vec<f64> {
        (0..n).map(|i| i as f64 * interval).collect()
    }

    /// Beats whose interval ramps linearly from `from` to `to`.
    fn ramping(n: usize, from: f64, to: f64) -> Vec<f64> {
        let mut t = 0.0;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(t);
            let f = i as f64 / (n - 1) as f64;
            t += from + (to - from) * f;
        }
        out
    }

    #[test]
    fn steady_grid_has_no_drift_and_reports_its_bpm() {
        let beats = steady(64, 0.5);
        let drift = drift_pct(&beats).expect("enough beats");
        assert!(drift < 0.05, "steady grid must read ~0% drift, got {drift}");
        let bpm = dominant_bpm(&beats).expect("enough beats");
        assert!((bpm - 120.0).abs() < 0.01, "got {bpm}");
    }

    #[test]
    fn jitter_alone_is_not_drift() {
        // ±10 ms of detection jitter on a 0.5 s grid is ±2% per-gap noise.
        // The median filter must absorb it — otherwise every track in the
        // library reads as unsteady and the feature is worthless.
        let mut beats = steady(96, 0.5);
        for (i, b) in beats.iter_mut().enumerate() {
            *b += if i % 2 == 0 { 0.010 } else { -0.010 };
        }
        let drift = drift_pct(&beats).expect("enough beats");
        assert!(drift < 1.0, "jitter must not register as drift, got {drift}");
    }

    #[test]
    fn a_real_ramp_registers_as_drift() {
        // 120 BPM (0.5 s) drifting to ~126 BPM (0.476 s) = about 5%.
        let beats = ramping(96, 0.5, 0.476);
        let drift = drift_pct(&beats).expect("enough beats");
        assert!(drift > 3.0, "a real ramp must show drift, got {drift}");
    }

    #[test]
    fn a_breakdown_is_not_mistaken_for_tempo() {
        // A 6 s hole with no detected beats must be excluded, not read as one
        // enormous beat interval (which would both inflate drift and, in
        // plan_warp, demand an absurd stretch ratio).
        let mut beats = steady(40, 0.5);
        let resume = beats[beats.len() - 1] + 6.0;
        beats.extend((0..40).map(|i| resume + i as f64 * 0.5));
        let drift = drift_pct(&beats).expect("enough beats");
        assert!(drift < 0.5, "dropout must be ignored, got {drift}");
    }

    #[test]
    fn dominant_bpm_is_not_quantized_to_whole_frames_and_is_not_folded() {
        // 131 BPM snapped to 20 ms frames: the median of gaps reads 130.435.
        let q = |t: f64| (t / 0.02).round() * 0.02;
        let beats: Vec<f64> = (0..128).map(|k| q(k as f64 * 60.0 / 131.0)).collect();
        let bpm = dominant_bpm(&beats).expect("enough beats");
        assert!((bpm - 131.0).abs() < 0.05, "got {bpm}");
        // 65 BPM stays 65: stabilize uses this as its target and a folded
        // (doubled) target would demand a 2x stretch.
        let slow: Vec<f64> = (0..40).map(|k| k as f64 * 60.0 / 65.0).collect();
        assert!((dominant_bpm(&slow).unwrap() - 65.0).abs() < 0.05);
    }

    #[test]
    fn too_few_beats_is_none() {
        assert!(drift_pct(&steady(MIN_BEATS - 1, 0.5)).is_none());
        assert!(dominant_bpm(&steady(3, 0.5)).is_none());
        assert!(matches!(plan_warp(&steady(4, 0.5), 120.0), WarpPlan::NotEnoughBeats));
    }

    #[test]
    fn steady_track_at_its_own_tempo_is_one_unit_segment() {
        let beats = steady(64, 0.5);
        let WarpPlan::Segments(segs) = plan_warp(&beats, 120.0) else { panic!() };
        assert_eq!(segs.len(), 1, "no tempo change needed");
        assert!((segs[0].ratio - 1.0).abs() < 1e-6);
        assert_eq!(segs[0].at_secs, 0.0, "first segment always anchors at 0");
    }

    #[test]
    fn steady_track_forced_to_a_new_bpm_is_one_constant_segment() {
        // 120 BPM track told to become 128: a single ratio for the whole file.
        let beats = steady(64, 0.5);
        let WarpPlan::Segments(segs) = plan_warp(&beats, 128.0) else { panic!() };
        assert_eq!(segs.len(), 1);
        // target interval 60/128 = 0.46875; ratio = 0.5 / 0.46875
        assert!((segs[0].ratio - (0.5 / (60.0 / 128.0))).abs() < 1e-6);
    }

    #[test]
    fn a_ramp_produces_monotonically_increasing_ratios_but_few_segments() {
        let beats = ramping(200, 0.5, 0.476);
        let WarpPlan::Segments(segs) = plan_warp(&beats, 120.0) else { panic!() };
        assert!(segs.len() > 1, "a drifting track needs more than one segment");
        assert!(
            segs.len() < 40,
            "segments must be smoothed, not one per beat: got {}",
            segs.len()
        );
        for w in segs.windows(2) {
            assert!(w[1].at_secs > w[0].at_secs, "segments must advance in time");
            assert!(w[1].ratio < w[0].ratio, "speeding up = shrinking ratio");
        }
    }

    #[test]
    fn jittery_steady_track_still_collapses_to_one_segment() {
        let mut beats = steady(96, 0.5);
        for (i, b) in beats.iter_mut().enumerate() {
            *b += if i % 2 == 0 { 0.010 } else { -0.010 };
        }
        let WarpPlan::Segments(segs) = plan_warp(&beats, 120.0) else { panic!() };
        assert_eq!(segs.len(), 1, "jitter must not spawn segments: {segs:?}");
    }

    #[test]
    fn absurd_target_is_refused_not_clamped() {
        let beats = steady(64, 0.5); // 120 BPM
        assert!(matches!(plan_warp(&beats, 300.0), WarpPlan::OutOfRange(_)));
        assert!(matches!(plan_warp(&beats, 40.0), WarpPlan::OutOfRange(_)));
    }

    #[test]
    fn script_skips_the_first_segment_and_formats_commands() {
        let segs = vec![
            WarpSegment { at_secs: 0.0, ratio: 1.0 },
            WarpSegment { at_secs: 12.5, ratio: 1.0125 },
        ];
        let script = asendcmd_script(&segs);
        assert!(!script.contains("0.000"), "first ratio rides on the filter arg");
        assert_eq!(script.trim(), "12.500 rubberband tempo 1.012500;");
    }

    #[test]
    fn filter_arg_uses_a_bare_relative_command_file() {
        // ffmpeg filter syntax treats ':' and '\' as separators, so a Windows
        // absolute path cannot be embedded — the worker runs ffmpeg with the
        // temp dir as its working directory and passes a bare file name.
        let with = filter_arg(1.0125, Some("warp.txt"));
        assert_eq!(with, "asendcmd=f=warp.txt,rubberband=tempo=1.012500:transients=crisp");
        assert!(!with.contains(':') || with.contains("tempo="));
        let without = filter_arg(1.02, None);
        assert_eq!(without, "rubberband=tempo=1.020000:transients=crisp");
    }
}
