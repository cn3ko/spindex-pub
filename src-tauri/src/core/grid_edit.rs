//! The deck's beat-grid editor: which grid it starts from and which values a
//! save may write. Pure — the command layer does the file and DB work.

use std::path::Path;

use anyhow::{bail, Result};
use rusqlite::Connection;

use crate::core::serato_beatgrid::Beatgrid;

pub const MIN_BPM: f64 = 40.0;
pub const MAX_BPM: f64 = 250.0;
/// Serato stores anchors before 0 s when the first downbeat precedes the audio.
const MIN_ANCHOR_SEC: f64 = -10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GridSource {
    File,
    Detected,
    Bpm,
    None,
}

/// One stretch of a grid at one tempo: beat lines every 60/bpm seconds from
/// `start_sec` up to the next section (the first section also reaches back
/// before its start, the last one on to the end). A section's start is a
/// beat line; the first one's is a bar 1.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GridSection {
    pub start_sec: f64,
    pub bpm: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StartingGrid {
    /// At least one; more when the tempo changes along the track.
    pub sections: Vec<GridSection>,
    pub source: GridSource,
    /// The file's grid could not be read, so a fallback was used.
    pub file_unreadable: bool,
}

/// A Serato grid as editor sections. A tempo-changing grid keeps every
/// marker: each marker's section has the tempo its whole beats span up to the
/// next marker, and the terminal marker carries its own tempo.
fn sections_of(g: &Beatgrid) -> Vec<GridSection> {
    match g {
        Beatgrid::Constant { anchor_sec, bpm } => {
            vec![GridSection { start_sec: *anchor_sec as f64, bpm: *bpm as f64 }]
        }
        Beatgrid::Dynamic { markers, terminal_sec, terminal_bpm } => {
            let mut out: Vec<GridSection> = markers
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let next = markers.get(i + 1).map(|n| n.pos_sec).unwrap_or(*terminal_sec) as f64;
                    let span = next - m.pos_sec as f64;
                    let bpm = if m.beats_to_next > 0 && span > 0.0 {
                        60.0 * m.beats_to_next as f64 / span
                    } else {
                        *terminal_bpm as f64
                    };
                    GridSection { start_sec: m.pos_sec as f64, bpm }
                })
                .collect();
            out.push(GridSection { start_sec: *terminal_sec as f64, bpm: *terminal_bpm as f64 });
            out
        }
    }
}

/// The grid the editor opens with: the file's own grid, else SpinDex's
/// detected grid, else the stored BPM with bar 1 on the first detected beat
/// (or 0 s), else 120 BPM at 0 s. `file` is `Err(())` when the file's grid
/// exists but could not be read.
pub fn starting_grid(
    file: std::result::Result<Option<Beatgrid>, ()>,
    detected: Option<Beatgrid>,
    stored_bpm: Option<f64>,
    first_beat: Option<f64>,
) -> StartingGrid {
    let file_unreadable = file.is_err();
    let one = |start_sec: f64, bpm: f64| vec![GridSection { start_sec, bpm }];
    let (sections, source) = if let Ok(Some(g)) = &file {
        (sections_of(g), GridSource::File)
    } else if let Some(g) = &detected {
        (sections_of(g), GridSource::Detected)
    } else if let Some(bpm) = stored_bpm.filter(|b| b.is_finite() && *b > 0.0) {
        (one(first_beat.unwrap_or(0.0), bpm), GridSource::Bpm)
    } else {
        (one(0.0, 120.0), GridSource::None)
    };
    StartingGrid { sections, source, file_unreadable }
}

/// Values a save may write. Checked before any file is touched.
pub fn validate(anchor_sec: f64, bpm: f64, duration: Option<f64>) -> Result<()> {
    if !bpm.is_finite() || !(MIN_BPM..=MAX_BPM).contains(&bpm) {
        bail!("BPM must be between {MIN_BPM} and {MAX_BPM}");
    }
    if !anchor_sec.is_finite() || anchor_sec < MIN_ANCHOR_SEC {
        bail!("the grid start is out of range");
    }
    if let Some(d) = duration.filter(|d| d.is_finite() && *d > 0.0) {
        if anchor_sec > d {
            bail!("the grid starts after the end of the track");
        }
    }
    Ok(())
}

/// Whole beats from section `k` to the next one, at section `k`'s tempo.
fn beats_to_next(sections: &[GridSection], k: usize) -> i64 {
    let span = sections[k + 1].start_sec - sections[k].start_sec;
    (span * sections[k].bpm / 60.0).round() as i64
}

/// The sections a save may write: at least one; every tempo in range; starts
/// in order, at least one beat apart, the first not before -10 s and none
/// past the end. Checked before any file is touched.
pub fn validate_sections(sections: &[GridSection], duration: Option<f64>) -> Result<()> {
    let Some(first) = sections.first() else { bail!("the grid has no sections") };
    validate(first.start_sec, first.bpm, duration)?;
    for (k, s) in sections.iter().enumerate().skip(1) {
        validate(s.start_sec, s.bpm, duration)?;
        if s.start_sec <= sections[k - 1].start_sec || beats_to_next(sections, k - 1) < 1 {
            bail!("tempo markers must be in order and at least one beat apart");
        }
    }
    Ok(())
}

/// The sections as a Serato grid: one section is a constant grid (its anchor
/// moved by whole bars into the first bar), several a tempo-changing grid,
/// one marker per section with its whole beats to the next.
fn to_beatgrid(sections: &[GridSection]) -> Beatgrid {
    if let [only] = sections {
        let anchor = first_bar_anchor(only.start_sec, only.bpm);
        return Beatgrid::Constant { anchor_sec: anchor as f32, bpm: only.bpm as f32 };
    }
    let last = sections[sections.len() - 1];
    let markers = (0..sections.len() - 1)
        .map(|k| crate::core::serato_beatgrid::Marker {
            pos_sec: sections[k].start_sec as f32,
            beats_to_next: beats_to_next(sections, k).max(1) as u32,
        })
        .collect();
    Beatgrid::Dynamic { markers, terminal_sec: last.start_sec as f32, terminal_bpm: last.bpm as f32 }
}

/// The tempo the track list shows for a grid: that of its longest section.
fn main_bpm(sections: &[GridSection], duration: Option<f64>) -> f64 {
    let end = duration.filter(|d| d.is_finite() && *d > 0.0).unwrap_or(f64::MAX);
    let len = |k: usize| sections.get(k + 1).map(|n| n.start_sec).unwrap_or(end) - sections[k].start_sec;
    (0..sections.len())
        .max_by(|&i, &j| len(i).total_cmp(&len(j)))
        .map(|k| sections[k].bpm)
        .unwrap_or(120.0)
}

/// The same grid with its anchor moved by whole bars into the track's first
/// bar, [0, bar length). Values `validate` would reject pass through unchanged.
fn first_bar_anchor(anchor_sec: f64, bpm: f64) -> f64 {
    if !anchor_sec.is_finite() || !bpm.is_finite() || bpm <= 0.0 {
        return anchor_sec;
    }
    let bar = 4.0 * 60.0 / bpm;
    anchor_sec.rem_euclid(bar)
}

/// Writes a hand-set grid (one section, or several for a tempo change) into
/// the file and records it: validation first (nothing touched on bad values),
/// then the first-touch backup, then the file, then the row, so a failed
/// write leaves the DB as it was.
pub fn save_grid(
    conn: &Connection,
    track_id: i64,
    root: &Path,
    rel: &str,
    abs: &Path,
    sections: &[GridSection],
) -> Result<()> {
    let duration: Option<f64> = conn.query_row(
        "SELECT duration_secs FROM track WHERE id = ?1",
        rusqlite::params![track_id],
        |r| r.get(0),
    )?;
    // One section: a tempo change far into the track, or "bar 1 here" near its
    // end, leaves the anchor far from the start. Moving it by whole bars
    // changes no line, so check it where it will be written.
    let mut checked = sections.to_vec();
    if let [only] = checked.as_mut_slice() {
        only.start_sec = first_bar_anchor(only.start_sec, only.bpm);
    }
    validate_sections(&checked, duration)?;
    let backup = crate::core::enhance::backup_path_for(root, rel);
    crate::core::enhance::ensure_backup(abs, &backup)?;
    crate::core::metadata::write_serato_beatgrid(abs, &to_beatgrid(&checked))?;
    crate::core::db::stamp_serato_grid_written(conn, track_id)?;
    crate::core::db::set_serato_grid_source(conn, track_id, "manual")?;
    conn.execute(
        "UPDATE track SET bpm = ?2 WHERE id = ?1",
        rusqlite::params![track_id, main_bpm(&checked, duration)],
    )?;
    Ok(())
}

/// Decode rate for the editor's waveform: 11 samples per millisecond is enough
/// to see a kick's attack, and keeps a two-hour mix's decode under 320 MB.
pub const DETAIL_SAMPLE_RATE: u32 = 11_025;

/// One peak per millisecond (max |sample|), scaled so the loudest millisecond
/// of the track is 255. The deck's stored 400-point overview is far too coarse
/// to place a grid line at 1 ms precision.
pub fn detail_peaks(pcm: &[f32], sample_rate: u32) -> Vec<u8> {
    if pcm.is_empty() || sample_rate == 0 {
        return Vec::new();
    }
    let per_ms = sample_rate as f64 / 1000.0;
    let buckets = (pcm.len() as f64 / per_ms).ceil() as usize;
    let mut raw = vec![0.0f32; buckets];
    for (i, s) in pcm.iter().enumerate() {
        let b = ((i as f64 / per_ms) as usize).min(buckets - 1);
        let a = s.abs();
        if a > raw[b] {
            raw[b] = a;
        }
    }
    let max = raw.iter().cloned().fold(0.0f32, f32::max);
    if max <= 0.0 {
        return vec![0; buckets];
    }
    raw.iter().map(|v| ((v / max) * 255.0).round() as u8).collect()
}

/// The window a band's energy is averaged over, in ms before and after: a
/// kick's 50-60 Hz fundamental is 17-20 ms long, so an unsmoothed millisecond
/// would flicker between full and empty within one cycle. The window looks
/// mostly back, so a hit's colour starts with the hit and never ahead of it
/// (the grid is lined up against exactly that edge).
const BAND_SMOOTH_BACK_MS: usize = 7;
const BAND_SMOOTH_AHEAD_MS: usize = 1;

/// Waveform-shape buckets per millisecond: a quarter millisecond, about three
/// samples at the editor's decode rate.
pub const SHAPE_PER_MS: usize = 4;

/// The grid editor's waveform, as one byte buffer for the webview: a
/// little-endian u32 `n` (milliseconds), then `n × [peak, low, mid, high]`
/// (`detail_waveform`) and `4n × [min, max]` (`detail_shape`): 12 bytes per
/// millisecond. Fast; the colourings come separately (`colour_payload`), so
/// the waveform never waits for them.
pub fn detail_payload(pcm: &[f32], sample_rate: u32) -> Vec<u8> {
    let bands = detail_waveform(pcm, sample_rate);
    let n = bands.len() / 4;
    let mut out = Vec::with_capacity(4 + n * 12);
    out.extend((n as u32).to_le_bytes());
    out.extend(bands);
    out.extend(detail_shape(pcm, sample_rate));
    out
}

/// The grid editor's extra colourings: a little-endian u32 `n` (the same
/// milliseconds as `detail_payload`), then `n × [kick, hats, tonal]`
/// (`wave_colour::drums_tonal`) and `n × [camelot, confidence]`
/// (`wave_colour::harmony`). Several times slower than the waveform.
pub fn colour_payload(pcm: &[f32], sample_rate: u32) -> Vec<u8> {
    let n = detail_peaks(pcm, sample_rate).len();
    let mut out = Vec::with_capacity(4 + n * 5);
    out.extend((n as u32).to_le_bytes());
    out.extend(crate::core::wave_colour::drums_tonal(pcm, sample_rate, n));
    out.extend(crate::core::wave_colour::harmony(pcm, sample_rate, n));
    out
}

/// The waveform's real shape for the zoomed view: per quarter millisecond the
/// lowest and highest sample, as signed bytes `[min, max]`, on a square-root
/// scale of the track's loudest sample (a quarter of it reads as half), so
/// quiet attacks stay visible beside a loud kick. `4 × detail_peaks().len()`
/// buckets, so the frontend can line both up by index.
pub fn detail_shape(pcm: &[f32], sample_rate: u32) -> Vec<u8> {
    let n = detail_peaks(pcm, sample_rate).len() * SHAPE_PER_MS;
    if n == 0 {
        return Vec::new();
    }
    let per_bucket = sample_rate as f64 / (1000.0 * SHAPE_PER_MS as f64);
    // start empty, not at 0: a bucket's range is its own samples only, so a
    // stretch that stays on one side of zero is drawn as a curve, not a lobe
    let mut lo = vec![f32::INFINITY; n];
    let mut hi = vec![f32::NEG_INFINITY; n];
    for (i, &s) in pcm.iter().enumerate() {
        let q = ((i as f64 / per_bucket) as usize).min(n - 1);
        lo[q] = lo[q].min(s);
        hi[q] = hi[q].max(s);
    }
    for q in 0..n {
        if lo[q] > hi[q] {
            (lo[q], hi[q]) = (0.0, 0.0); // a bucket no sample fell into
        }
    }
    let max = pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let byte = |v: f32| -> u8 {
        if max <= 0.0 {
            return 0;
        }
        let c = (v / max).abs().sqrt() * v.signum() * 127.0;
        (c.round() as i8) as u8
    };
    let mut out = Vec::with_capacity(n * 2);
    for q in 0..n {
        out.extend([byte(lo[q]), byte(hi[q])]);
    }
    out
}

/// The editor's waveform: per millisecond `[peak, low, mid, high]`. `peak` is
/// `detail_peaks` (the bar's height); the three bands are the same
/// 200 Hz / 2 kHz split the deck's overview uses, each a smoothed RMS scaled
/// to its own loudest millisecond, so a column says how busy that band is
/// relative to the rest of the track (what the colour blend wants).
pub fn detail_waveform(pcm: &[f32], sample_rate: u32) -> Vec<u8> {
    let peaks = detail_peaks(pcm, sample_rate);
    let n = peaks.len();
    if n == 0 {
        return Vec::new();
    }
    let per_ms = sample_rate as f64 / 1000.0;
    let mut filters = crate::core::dsp::band_filters(sample_rate as f64);
    let mut energy = [vec![0.0f32; n], vec![0.0f32; n], vec![0.0f32; n]];
    let mut count = vec![0u32; n];
    for (i, &s) in pcm.iter().enumerate() {
        let b = ((i as f64 / per_ms) as usize).min(n - 1);
        for (k, f) in filters.iter_mut().enumerate() {
            let y = f.process(s as f64);
            energy[k][b] += (y * y) as f32;
        }
        count[b] += 1;
    }
    let bands: Vec<Vec<u8>> = energy
        .iter()
        .map(|e| {
            // mean square per ms, then a box average over the smoothing window
            let mut prefix = vec![0.0f64; n + 1];
            for i in 0..n {
                prefix[i + 1] = prefix[i] + (e[i] / count[i].max(1) as f32) as f64;
            }
            let rms: Vec<f64> = (0..n)
                .map(|i| {
                    let (a, b) = (i.saturating_sub(BAND_SMOOTH_BACK_MS), (i + BAND_SMOOTH_AHEAD_MS + 1).min(n));
                    ((prefix[b] - prefix[a]) / (b - a) as f64).sqrt()
                })
                .collect();
            let max = rms.iter().cloned().fold(0.0f64, f64::max);
            if max <= 1e-9 {
                return vec![0u8; n];
            }
            rms.iter().map(|v| ((v / max) * 255.0).round() as u8).collect()
        })
        .collect();
    let mut out = Vec::with_capacity(n * 4);
    for i in 0..n {
        out.extend([peaks[i], bands[0][i], bands[1][i], bands[2][i]]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::serato_beatgrid::{Beatgrid, Marker};

    fn c(anchor: f32, bpm: f32) -> Beatgrid {
        Beatgrid::Constant { anchor_sec: anchor, bpm }
    }

    fn sec(start_sec: f64, bpm: f64) -> GridSection {
        GridSection { start_sec, bpm }
    }

    /// The single section of a one-tempo starting grid.
    fn only(g: &StartingGrid) -> GridSection {
        assert_eq!(g.sections.len(), 1, "one section expected: {:?}", g.sections);
        g.sections[0]
    }

    #[test]
    fn the_files_own_grid_wins() {
        let g = starting_grid(Ok(Some(c(0.05, 128.0))), Some(c(0.3, 127.0)), Some(126.0), Some(0.2));
        assert_eq!(g.source, GridSource::File);
        let s = only(&g);
        assert!((s.start_sec - 0.05).abs() < 1e-6 && (s.bpm - 128.0).abs() < 1e-6);
        assert!(!g.file_unreadable);
    }

    #[test]
    fn falls_back_to_detected_then_bpm_then_nothing() {
        let d = starting_grid(Ok(None), Some(c(0.3, 127.0)), Some(126.0), Some(0.2));
        assert_eq!(d.source, GridSource::Detected);
        let b = starting_grid(Ok(None), None, Some(126.0), Some(0.2));
        assert_eq!(b.source, GridSource::Bpm);
        assert!((only(&b).start_sec - 0.2).abs() < 1e-9 && (only(&b).bpm - 126.0).abs() < 1e-9);
        let b0 = starting_grid(Ok(None), None, Some(126.0), None);
        assert_eq!(only(&b0).start_sec, 0.0);
        let n = starting_grid(Ok(None), None, None, None);
        assert_eq!(n.source, GridSource::None);
        assert_eq!(only(&n), sec(0.0, 120.0));
    }

    #[test]
    fn an_unreadable_file_grid_falls_back_and_says_so() {
        let g = starting_grid(Err(()), Some(c(0.3, 127.0)), None, None);
        assert_eq!(g.source, GridSource::Detected);
        assert!(g.file_unreadable);
    }

    #[test]
    fn a_tempo_changing_file_grid_keeps_its_sections() {
        // two 4-beat markers 2 s apart (120 BPM each), then the terminal at 4.0 s
        // with its own 125 BPM: three sections, nothing averaged away
        let dynamic = Beatgrid::Dynamic {
            markers: vec![
                Marker { pos_sec: 0.0, beats_to_next: 4 },
                Marker { pos_sec: 2.0, beats_to_next: 4 },
            ],
            terminal_sec: 4.0,
            terminal_bpm: 125.0,
        };
        let g = starting_grid(Ok(Some(dynamic)), None, None, None);
        assert_eq!(g.source, GridSource::File);
        assert_eq!(g.sections.len(), 3);
        for (got, want) in g.sections.iter().zip([sec(0.0, 120.0), sec(2.0, 120.0), sec(4.0, 125.0)]) {
            assert!((got.start_sec - want.start_sec).abs() < 1e-6, "{got:?} vs {want:?}");
            assert!((got.bpm - want.bpm).abs() < 1e-6, "{got:?} vs {want:?}");
        }
    }

    #[test]
    fn sections_are_checked_before_any_write() {
        let ok = [sec(0.1, 128.0), sec(30.1, 124.0)];
        assert!(validate_sections(&ok, Some(300.0)).is_ok());
        assert!(validate_sections(&[], Some(300.0)).is_err(), "no section");
        assert!(validate_sections(&[sec(30.0, 128.0), sec(10.0, 124.0)], None).is_err(), "out of order");
        assert!(validate_sections(&[sec(0.0, 128.0), sec(0.2, 124.0)], None).is_err(), "under one beat apart");
        assert!(validate_sections(&[sec(0.0, 128.0), sec(30.0, 900.0)], None).is_err(), "a section out of range");
        assert!(validate_sections(&[sec(0.0, 128.0), sec(301.0, 124.0)], Some(300.0)).is_err(), "past the end");
    }

    #[test]
    fn validate_rejects_nonsense_before_any_write() {
        assert!(validate(0.05, 128.0, Some(300.0)).is_ok());
        assert!(validate(-9.0, 40.0, Some(300.0)).is_ok());
        assert!(validate(0.0, 250.0, None).is_ok());
        for (a, b) in [
            (f64::NAN, 128.0),
            (0.0, f64::NAN),
            (0.0, 0.0),
            (0.0, 39.99),
            (0.0, 250.01),
            (-10.5, 128.0),
            (f64::INFINITY, 128.0),
        ] {
            assert!(validate(a, b, Some(300.0)).is_err(), "{a} {b}");
        }
        assert!(validate(301.0, 128.0, Some(300.0)).is_err(), "anchor past the end");
    }
    #[test]
    fn detail_peaks_is_one_value_per_millisecond_scaled_to_the_loudest() {
        // 22 050 Hz: 22.05 samples per ms -> 100 ms = 2205 samples = 100 buckets
        let mut pcm = vec![0.0f32; 2205];
        pcm[0] = 0.5; // bucket 0
        pcm[1102] = -1.0; // mid-track, the loudest
        pcm[2204] = 0.25; // last bucket
        let p = detail_peaks(&pcm, 22_050);
        assert_eq!(p.len(), 100);
        assert_eq!(*p.iter().max().unwrap(), 255);
        assert!((p[0] as i32 - 128).abs() <= 1, "0.5 of the max -> ~128, got {}", p[0]);
        assert!((p[99] as i32 - 64).abs() <= 1, "0.25 -> ~64, got {}", p[99]);
    }

    #[test]
    fn detail_peaks_of_silence_or_nothing() {
        assert!(detail_peaks(&[], 22_050).is_empty());
        assert!(detail_peaks(&[0.0; 441], 22_050).iter().all(|&v| v == 0));
    }

    fn tone(freq: f64, secs: f64) -> Vec<f32> {
        let fs = DETAIL_SAMPLE_RATE as f64;
        (0..(fs * secs) as usize)
            .map(|i| (std::f64::consts::TAU * freq * i as f64 / fs).sin() as f32 * 0.5)
            .collect()
    }

    /// Mean of one band column over the middle half of the ms range `a..b`
    /// (past the filters' settling), as a share of the three bands together.
    fn band_share(w: &[u8], a: usize, b: usize, band: usize) -> f64 {
        let (a, b) = (a + (b - a) / 4, a + (b - a) * 3 / 4);
        let sum = |k: usize| (a..b).map(|i| w[i * 4 + k] as f64).sum::<f64>();
        sum(band + 1) / (sum(1) + sum(2) + sum(3))
    }

    #[test]
    fn detail_waveform_is_peak_plus_three_bands_per_millisecond() {
        let pcm = tone(60.0, 0.5);
        let w = detail_waveform(&pcm, DETAIL_SAMPLE_RATE);
        let peaks = detail_peaks(&pcm, DETAIL_SAMPLE_RATE);
        assert_eq!(w.len(), peaks.len() * 4);
        let col: Vec<u8> = w.chunks(4).map(|c| c[0]).collect();
        assert_eq!(col, peaks, "the height column is the old peak waveform");
    }

    #[test]
    fn detail_waveform_puts_kick_mid_and_air_in_their_bands() {
        // one second each of bass, mids and air, back to back
        let mut pcm = tone(60.0, 1.0);
        pcm.extend(tone(800.0, 1.0));
        pcm.extend(tone(4_000.0, 1.0));
        let w = detail_waveform(&pcm, DETAIL_SAMPLE_RATE);
        let low = band_share(&w, 0, 1000, 0);
        let mid = band_share(&w, 1000, 2000, 1);
        let high = band_share(&w, 2000, 3000, 2);
        assert!(low > 0.8 && mid > 0.8 && high > 0.8, "low {low:.2} mid {mid:.2} high {high:.2}");
    }

    #[test]
    fn detail_waveform_of_nothing_is_empty() {
        assert!(detail_waveform(&[], DETAIL_SAMPLE_RATE).is_empty());
        assert!(detail_waveform(&[0.0; 110], DETAIL_SAMPLE_RATE).iter().all(|&v| v == 0));
    }

    #[test]
    fn the_bass_colour_does_not_start_before_the_kick() {
        // silence, then a 60 Hz burst from 500 ms on
        let mut pcm = vec![0.0f32; DETAIL_SAMPLE_RATE as usize / 2];
        pcm.extend(tone(60.0, 0.5));
        let w = detail_waveform(&pcm, DETAIL_SAMPLE_RATE);
        let low = |ms: usize| w[ms * 4 + 1];
        assert!(low(497) < 20, "3 ms before the hit the bass is still dark: {}", low(497));
        assert!(low(560) > 200, "inside the burst it is lit: {}", low(560));
    }

    #[test]
    fn detail_shape_is_signed_min_max_per_quarter_millisecond() {
        let mut pcm = vec![0.0f32; DETAIL_SAMPLE_RATE as usize / 10]; // 100 ms
        pcm[441] = 1.0; // 40 ms → quarter-ms bucket 160
        pcm[882] = -0.25; // 80 ms → bucket 320
        let s = detail_shape(&pcm, DETAIL_SAMPLE_RATE);
        let n = detail_peaks(&pcm, DETAIL_SAMPLE_RATE).len();
        assert_eq!(s.len(), n * 8, "4 buckets per ms, a min and a max each");
        let mx = |q: usize| s[q * 2 + 1] as i8;
        let mn = |q: usize| s[q * 2] as i8;
        assert_eq!(mx(160), 127);
        assert_eq!(mn(160), 0);
        // square-root scale: a quarter of the loudest reads as half
        assert_eq!(mn(320), -64);
        assert_eq!(mx(320), 0);
        assert!((0..400).filter(|&q| q != 160 && q != 320).all(|q| mx(q) == 0 && mn(q) == 0));
    }

    #[test]
    fn a_bucket_holds_only_its_own_samples_not_zero() {
        // a stretch that stays positive: its buckets must not reach down to 0,
        // or a zoomed-in curve is drawn as lobes filled to the centre line
        let pcm = vec![0.5f32; 110]; // 10 ms
        let s = detail_shape(&pcm, DETAIL_SAMPLE_RATE);
        let (mn, mx) = (s[8] as i8, s[9] as i8); // bucket 4
        assert_eq!((mn, mx), (127, 127), "min {mn} max {mx}");
    }

    #[test]
    fn the_waveform_and_its_colours_come_as_two_payloads() {
        let pcm = tone(220.0, 0.5);
        let n = detail_peaks(&pcm, DETAIL_SAMPLE_RATE).len();
        // the waveform: length header, bands, shape — no colourings, so it
        // never waits for them
        let p = detail_payload(&pcm, DETAIL_SAMPLE_RATE);
        assert_eq!(u32::from_le_bytes([p[0], p[1], p[2], p[3]]) as usize, n);
        assert_eq!(p.len(), 4 + n * (4 + 8));
        assert_eq!(&p[4..4 + n * 4], detail_waveform(&pcm, DETAIL_SAMPLE_RATE).as_slice());
        assert_eq!(&p[4 + n * 4..], detail_shape(&pcm, DETAIL_SAMPLE_RATE).as_slice());
        // the colourings: length header, drums, harmony
        let c = colour_payload(&pcm, DETAIL_SAMPLE_RATE);
        assert_eq!(u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as usize, n);
        assert_eq!(c.len(), 4 + n * (3 + 2));
        let rest = &c[4..];
        assert_eq!(&rest[..n * 3], crate::core::wave_colour::drums_tonal(&pcm, DETAIL_SAMPLE_RATE, n).as_slice());
        assert_eq!(&rest[n * 3..], crate::core::wave_colour::harmony(&pcm, DETAIL_SAMPLE_RATE, n).as_slice());
    }

    #[test]
    fn detail_shape_of_nothing_is_empty_or_flat() {
        assert!(detail_shape(&[], DETAIL_SAMPLE_RATE).is_empty());
        assert!(detail_shape(&[0.0; 110], DETAIL_SAMPLE_RATE).iter().all(|&v| v == 0));
    }
    /// A minimal valid 16-bit mono PCM WAV (0.1 s of silence) lofty can tag.
    fn minimal_wav(path: &std::path::Path) {
        let samples = 4410u32;
        let data_len = samples * 2;
        let mut b = Vec::new();
        b.extend(b"RIFF");
        b.extend((36 + data_len).to_le_bytes());
        b.extend(b"WAVEfmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes()); // PCM
        b.extend(1u16.to_le_bytes()); // mono
        b.extend(44_100u32.to_le_bytes());
        b.extend((44_100u32 * 2).to_le_bytes());
        b.extend(2u16.to_le_bytes());
        b.extend(16u16.to_le_bytes());
        b.extend(b"data");
        b.extend(data_len.to_le_bytes());
        b.extend(vec![0u8; data_len as usize]);
        std::fs::write(path, b).unwrap();
    }

    fn save_fixture(name: &str) -> (std::path::PathBuf, rusqlite::Connection, i64) {
        let dir = std::env::temp_dir()
            .join(format!("spindex-grid-edit-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        minimal_wav(&dir.join("a.wav"));
        let conn = crate::core::db::open(&dir.join("t.db")).unwrap();
        let lib = crate::core::db::ensure_library(&conn, "L", &dir.to_string_lossy()).unwrap();
        conn.execute(
            "INSERT INTO track (library_id, rel_path, file_size, modified_at, bpm, duration_secs)
             VALUES (?1, 'a.wav', 1, '0', 120.0, 0.1)",
            rusqlite::params![lib],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        (dir, conn, id)
    }

    #[test]
    fn save_grid_writes_the_file_then_marks_the_row_manual() {
        let (dir, conn, id) = save_fixture("ok");
        let abs = dir.join("a.wav");
        save_grid(&conn, id, &dir, "a.wav", &abs, &[sec(0.012, 128.5)]).unwrap();

        let Some(Beatgrid::Constant { anchor_sec, bpm }) =
            crate::core::metadata::read_serato_beatgrid(&abs).unwrap()
        else {
            panic!("no constant grid in the file");
        };
        assert!((anchor_sec as f64 - 0.012).abs() < 1e-4);
        assert!((bpm as f64 - 128.5).abs() < 1e-3);
        let (row_bpm, src, stamp): (f64, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT bpm, serato_grid_source, serato_grid_written_at FROM track WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!((row_bpm - 128.5).abs() < 1e-9);
        assert_eq!(src.as_deref(), Some("manual"));
        assert!(stamp.is_some());
        assert!(
            crate::core::enhance::backup_path_for(&dir, "a.wav").exists(),
            "first-touch backup made before the write"
        );
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn save_grid_rejects_bad_values_without_touching_file_or_row() {
        let (dir, conn, id) = save_fixture("bad");
        let abs = dir.join("a.wav");
        let before = std::fs::read(&abs).unwrap();
        assert!(save_grid(&conn, id, &dir, "a.wav", &abs, &[sec(0.0, 900.0)]).is_err());
        assert!(
            save_grid(&conn, id, &dir, "a.wav", &abs, &[sec(5.0, 128.0)]).is_err(),
            "anchor past the 0.1 s end"
        );
        assert_eq!(std::fs::read(&abs).unwrap(), before, "file untouched");
        let src: Option<String> = conn
            .query_row("SELECT serato_grid_source FROM track WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        assert!(src.is_none(), "row untouched");
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }
    /// A BPM change far into a track leaves the anchor far before (or after)
    /// the start; the grid itself is fine, so the save moves the anchor by whole
    /// bars instead of refusing it.
    #[test]
    fn save_grid_moves_a_far_anchor_by_whole_bars() {
        let (dir, conn, id) = save_fixture("far");
        conn.execute("UPDATE track SET duration_secs = 300.0 WHERE id = ?1", [id]).unwrap();
        let abs = dir.join("a.wav");
        // 120 BPM: a bar is 2 s; -19.5 s is 10 bars before 0.5 s
        save_grid(&conn, id, &dir, "a.wav", &abs, &[sec(-19.5, 120.0)]).unwrap();
        let Some(Beatgrid::Constant { anchor_sec, .. }) =
            crate::core::metadata::read_serato_beatgrid(&abs).unwrap()
        else {
            panic!("no constant grid in the file");
        };
        assert!((anchor_sec as f64 - 0.5).abs() < 1e-4, "anchor {anchor_sec}");
        // and a late anchor (bar 1 set near the end) comes back to the first bar
        save_grid(&conn, id, &dir, "a.wav", &abs, &[sec(250.25, 120.0)]).unwrap();
        let Some(Beatgrid::Constant { anchor_sec, .. }) =
            crate::core::metadata::read_serato_beatgrid(&abs).unwrap()
        else {
            panic!("no constant grid in the file");
        };
        assert!((anchor_sec as f64 - 0.25).abs() < 1e-4, "anchor {anchor_sec}");
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn save_grid_writes_a_tempo_change_as_serato_markers() {
        let (dir, conn, id) = save_fixture("dyn");
        conn.execute("UPDATE track SET duration_secs = 300.0 WHERE id = ?1", [id]).unwrap();
        let abs = dir.join("a.wav");
        // 128 BPM for 64 beats from 0.1 s (30 s), then 124 BPM to the end
        save_grid(&conn, id, &dir, "a.wav", &abs, &[sec(0.1, 128.0), sec(30.1, 124.0)]).unwrap();
        let Some(Beatgrid::Dynamic { markers, terminal_sec, terminal_bpm }) =
            crate::core::metadata::read_serato_beatgrid(&abs).unwrap()
        else {
            panic!("no tempo-changing grid in the file");
        };
        assert_eq!(markers.len(), 1);
        assert!((markers[0].pos_sec as f64 - 0.1).abs() < 1e-4);
        assert_eq!(markers[0].beats_to_next, 64, "whole beats to the next marker");
        assert!((terminal_sec as f64 - 30.1).abs() < 1e-4);
        assert!((terminal_bpm as f64 - 124.0).abs() < 1e-3);
        // the row's BPM is the tempo of the longest section: 124 runs 270 s
        let row_bpm: f64 = conn.query_row("SELECT bpm FROM track WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert!((row_bpm - 124.0).abs() < 1e-9, "row bpm {row_bpm}");
        // and it opens again as the same two sections
        let back = starting_grid(Ok(crate::core::metadata::read_serato_beatgrid(&abs).unwrap()), None, None, None);
        assert_eq!(back.sections.len(), 2);
        assert!((back.sections[0].bpm - 128.0).abs() < 1e-3, "{:?}", back.sections);
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_detail_waveform_decodes_at_the_specs_rate() {
        // spec: 11,025 Hz is plenty for 1 ms buckets and halves the memory of a
        // long mix against 22,050
        assert_eq!(DETAIL_SAMPLE_RATE, 11_025);
    }
}
