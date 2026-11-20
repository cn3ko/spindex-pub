//! Reads and writes Serato's beat grid. Serato stores the grid IN the audio
//! file — an ID3 `GEOB` frame `Serato BeatGrid` for MP3/AIFF/WAV, a base64
//! `SERATO_BEATGRID` Vorbis comment for FLAC. Payload layout (big-endian):
//!   0x01 0x00 | u32 marker count N (terminal INCLUDED)
//!             | (N-1) x {f32 pos, u32 beats_to_next}
//!             | terminal {f32 pos, f32 bpm} | 1 footer byte (any value)
//! So `len == 7 + 8 * N`, checked against all 317 Serato-written grids on the
//! acceptance stick. A constant grid is N = 1 (just the terminal); N = 0 is
//! Serato's "no grid", which it also writes in place of a grid it rejects.
//! Positions may be negative: Serato puts the first marker before 0 s when
//! the downbeat precedes the first sample.

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub pos_sec: f32,
    pub beats_to_next: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Beatgrid {
    Constant { anchor_sec: f32, bpm: f32 },
    Dynamic { markers: Vec<Marker>, terminal_sec: f32, terminal_bpm: f32 },
}

const MAGIC: [u8; 2] = [0x01, 0x00];

pub fn serialize_payload(grid: &Beatgrid) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    match grid {
        Beatgrid::Constant { anchor_sec, bpm } => {
            out.extend_from_slice(&1u32.to_be_bytes());
            out.extend_from_slice(&anchor_sec.to_be_bytes());
            out.extend_from_slice(&bpm.to_be_bytes());
        }
        Beatgrid::Dynamic { markers, terminal_sec, terminal_bpm } => {
            out.extend_from_slice(&(markers.len() as u32 + 1).to_be_bytes());
            for m in markers {
                out.extend_from_slice(&m.pos_sec.to_be_bytes());
                out.extend_from_slice(&m.beats_to_next.to_be_bytes());
            }
            out.extend_from_slice(&terminal_sec.to_be_bytes());
            out.extend_from_slice(&terminal_bpm.to_be_bytes());
        }
    }
    out.push(0); // footer
    out
}

/// `Ok(None)` for Serato's empty "no grid" payload (count 0), which is a valid
/// file state, not a parse error.
pub fn parse_payload(bytes: &[u8]) -> Result<Option<Beatgrid>> {
    if bytes.len() < 7 {
        bail!("beat grid payload too short ({} bytes)", bytes.len());
    }
    if bytes[..2] != MAGIC {
        bail!("unexpected beat grid version {:?}", &bytes[..2]);
    }
    let count = u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]) as usize;
    if count == 0 {
        return Ok(None);
    }
    let need = count.checked_mul(8).and_then(|n| n.checked_add(7));
    if need != Some(bytes.len()) {
        bail!("beat grid payload is {} bytes, expected 7 + 8 x {count}", bytes.len());
    }
    let f32be = |b: &[u8]| f32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let u32be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let mut pos = 6usize;
    let mut markers = Vec::with_capacity(count - 1);
    for _ in 0..count - 1 {
        let p = f32be(&bytes[pos..]);
        let b = u32be(&bytes[pos + 4..]);
        if !p.is_finite() {
            bail!("non-finite marker position");
        }
        markers.push(Marker { pos_sec: p, beats_to_next: b });
        pos += 8;
    }
    let tpos = f32be(&bytes[pos..]);
    let tbpm = f32be(&bytes[pos + 4..]);
    if !tpos.is_finite() || !tbpm.is_finite() || tbpm <= 0.0 {
        bail!("bad terminal marker");
    }
    Ok(Some(if markers.is_empty() {
        Beatgrid::Constant { anchor_sec: tpos, bpm: tbpm }
    } else {
        Beatgrid::Dynamic { markers, terminal_sec: tpos, terminal_bpm: tbpm }
    }))
}

pub const DESCRIPTOR: &str = "Serato BeatGrid";
pub const VORBIS_KEY: &str = "SERATO_BEATGRID";
const MIME: &[u8] = b"application/octet-stream";

/// GEOB frame body: encoding, mime\0, filename\0, descriptor\0, payload.
pub fn geob_body(grid: &Beatgrid) -> Vec<u8> {
    let mut b = vec![0u8]; // latin1
    b.extend_from_slice(MIME);
    b.push(0);
    b.push(0); // empty filename
    b.extend_from_slice(DESCRIPTOR.as_bytes());
    b.push(0);
    b.extend_from_slice(&serialize_payload(grid));
    b
}

pub fn grid_from_geob_body(body: &[u8]) -> Result<Option<Beatgrid>> {
    // encoding(1) + mime\0 + filename\0 + descriptor\0 + payload
    let rest = body.get(1..).context("empty GEOB body")?;
    let mut it = rest.splitn(4, |&c| c == 0);
    let _mime = it.next().context("no mime")?;
    let _fname = it.next().context("no filename")?;
    let _desc = it.next().context("no descriptor")?;
    let payload = it.next().context("no payload")?;
    parse_payload(payload)
}

/// The FLAC form, byte-identical to Serato's own (captured from the stick):
/// base64 of the GEOB-style body plus one 0x00, without '=' padding, cut to
/// whole 6-bit characters, with a '\n' after every 72 characters.
pub fn vorbis_value(grid: &Beatgrid) -> String {
    let mut raw = Vec::new();
    raw.extend_from_slice(MIME);
    raw.push(0);
    raw.push(0);
    raw.extend_from_slice(DESCRIPTOR.as_bytes());
    raw.push(0);
    raw.extend_from_slice(&serialize_payload(grid));
    raw.push(0);
    let chars = b64_encode_truncated(&raw);
    let lines: Vec<&str> = chars
        .as_bytes()
        .chunks(72)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect();
    lines.join("\n")
}

pub fn grid_from_vorbis_value(v: &str) -> Result<Option<Beatgrid>> {
    let raw = b64_decode(v)?;
    // strip mime\0\0descriptor\0
    let mut it = raw.splitn(3, |&c| c == 0);
    let _mime = it.next().context("no mime")?;
    let _empty = it.next().context("no separator")?;
    let after = it.next().context("no payload section")?;
    // after == "Serato BeatGrid\0<payload>[\0]"
    let nul = after.iter().position(|&c| c == 0).context("no descriptor terminator")?;
    let mut payload = &after[nul + 1..];
    // Serato appends one 0x00 before encoding; it survives decoding whenever
    // the byte count is a multiple of 3. Drop it when it is exactly surplus.
    if payload.len() >= 6 {
        let count = u32::from_be_bytes([payload[2], payload[3], payload[4], payload[5]]) as usize;
        let want = count.saturating_mul(8).saturating_add(7);
        if payload.len() == want + 1 && payload.last() == Some(&0) {
            payload = &payload[..want];
        }
    }
    parse_payload(payload)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64 without padding, emitting only characters whose 6 bits are all
/// data: floor(8n / 6) characters, the way Serato writes it.
pub(crate) fn b64_encode_truncated(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 4 / 3 + 1);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &byte in data {
        acc = (acc << 8) | byte as u32;
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            s.push(B64[((acc >> bits) & 63) as usize] as char);
        }
        acc &= (1 << bits) - 1;
    }
    s
}

/// Lenient base64: skips whitespace and '=' padding, keeps whole bytes and
/// drops leftover bits, so Serato's unpadded, truncated form decodes.
pub(crate) fn b64_decode(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        let v = B64.iter().position(|&x| x == c).context("invalid base64 character")?;
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
        acc &= (1 << bits) - 1;
    }
    Ok(out)
}

use crate::core::tempo;

/// Minimum detected downbeats to build a grid from.
pub const MIN_DOWNBEATS: usize = 8;

/// Systematic lead of Serato's grid over the beat model on lossless audio:
/// Beat This! places a beat about 18 ms after the transient Serato puts its
/// grid line on. Measured 2026-09-28 against Serato's own grids on 20
/// WAV/AIFF/FLAC files (`tests/grid_phase_vs_serato_live.rs`, median +18 ms,
/// consistent from the first beat to the last, so it is an offset, not drift).
pub const MODEL_OFFSET_SEC: f64 = -0.018;

/// Timing correction for a file's grid. Lossless files get
/// `MODEL_OFFSET_SEC`. Lossy files get none: MP3 beats already came out about
/// 12 ms EARLY against Serato (decoder/encoder delay that ffmpeg skips and
/// Serato apparently does not), too scattered across 8 files to calibrate —
/// the same measurement, left open until there is more evidence.
pub fn timing_offset(path: &std::path::Path) -> f64 {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(ext.as_str(), "wav" | "aif" | "aiff" | "flac") {
        MODEL_OFFSET_SEC
    } else {
        0.0
    }
}

/// Builds a Serato grid from detected downbeats.
///
/// * Steady (`drift_pct` below `tempo::UNSTEADY_PCT`, or unknown) → a constant
///   grid from one beat line through every detected beat
///   (`dsp::fitted_beat_line`), anchored on the line position that most
///   detected downbeats fall on — a vote, so one misdetected intro downbeat
///   cannot shift the bar.
/// * Drifting → a dynamic grid: one marker at every downbeat but the last,
///   `beats_to_next = 4` (4/4 assumed), terminal marker at the last downbeat
///   with the local end-of-track BPM.
///
/// `offset_sec` (see `timing_offset`) shifts every position of either grid.
pub fn build(downbeats: &[f64], beats: &[f64], drift_pct: Option<f64>, offset_sec: f64) -> Result<Beatgrid> {
    if downbeats.len() < MIN_DOWNBEATS {
        bail!("only {} downbeats — need {MIN_DOWNBEATS} to build a grid", downbeats.len());
    }
    let steady = drift_pct.map_or(true, |d| d < tempo::UNSTEADY_PCT);
    if steady {
        let (start, period) = crate::core::dsp::fitted_beat_line(beats)
            .context("not enough beats to establish a tempo")?;
        let mut votes = [0usize; 4];
        for &d in downbeats {
            let k = ((d - start) / period).round() as i64;
            votes[k.rem_euclid(4) as usize] += 1;
        }
        // ties keep the earliest position, so the result is deterministic
        let bar = (0..4).max_by_key(|&i| (votes[i], std::cmp::Reverse(i))).unwrap_or(0);
        let anchor = start + bar as f64 * period + offset_sec;
        return Ok(Beatgrid::Constant { anchor_sec: anchor as f32, bpm: (60.0 / period) as f32 });
    }
    let downbeats: Vec<f64> = downbeats.iter().map(|t| t + offset_sec).collect();
    let markers: Vec<Marker> = downbeats[..downbeats.len() - 1]
        .iter()
        .map(|&t| Marker { pos_sec: t as f32, beats_to_next: 4 })
        .collect();
    let last = downbeats[downbeats.len() - 1];
    let prev = downbeats[downbeats.len() - 2];
    let local_bar = (last - prev).max(1e-3);
    let terminal_bpm = (4.0 * 60.0 / local_bar) as f32;
    Ok(Beatgrid::Dynamic {
        markers,
        terminal_sec: last as f32,
        terminal_bpm: terminal_bpm.clamp(60.0, 200.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steady_downbeats(n: usize, bar: f64) -> Vec<f64> {
        (0..n).map(|i| 0.05 + i as f64 * bar).collect()
    }

    #[test]
    fn steady_track_builds_a_constant_grid() {
        let db = steady_downbeats(40, 2.0); // 2 s/bar -> 120 BPM
        let beats: Vec<f64> = (0..160).map(|i| 0.05 + i as f64 * 0.5).collect();
        let g = build(&db, &beats, Some(0.2), 0.0).unwrap();
        match g {
            Beatgrid::Constant { anchor_sec, bpm } => {
                assert!((anchor_sec - 0.05).abs() < 1e-3);
                assert!((bpm - 120.0).abs() < 1.0, "bpm {bpm}");
            }
            _ => panic!("expected constant, got {g:?}"),
        }
    }

    #[test]
    fn drifting_track_builds_one_marker_per_downbeat() {
        let db = steady_downbeats(20, 2.0);
        let beats: Vec<f64> = (0..80).map(|i| 0.05 + i as f64 * 0.5).collect();
        let g = build(&db, &beats, Some(4.0), 0.0).unwrap();
        match g {
            Beatgrid::Dynamic { markers, .. } => {
                assert_eq!(markers.len(), 19, "n-1 non-terminal markers");
                assert!(markers.iter().all(|m| m.beats_to_next == 4));
            }
            _ => panic!("expected dynamic, got {g:?}"),
        }
    }

    /// Beat times the way the model reports them: on its 20 ms frame grid.
    fn quantized(t: f64) -> f64 {
        (t / 0.02).round() * 0.02
    }

    #[test]
    fn a_misdetected_first_downbeat_does_not_move_the_anchor() {
        // 120 BPM, bar 1 on 0.05 s; the intro's first downbeat was detected
        // half a beat late, every later one is right
        let beats: Vec<f64> = (0..160).map(|i| 0.05 + i as f64 * 0.5).collect();
        let mut db = steady_downbeats(40, 2.0);
        db[0] = 0.30;
        let Beatgrid::Constant { anchor_sec, .. } = build(&db, &beats, Some(0.2), 0.0).unwrap() else {
            panic!("expected constant")
        };
        assert!((anchor_sec as f64 - 0.05).abs() < 0.005, "anchor {anchor_sec}");
    }

    #[test]
    fn sampled_quantized_beats_keep_the_grid_on_the_beat_to_the_end() {
        // 131 BPM, four sampled 30 s windows as a normal Analyze pass stores
        // them, every time snapped to the 20 ms frame grid
        let p = 60.0 / 131.0;
        let mut beats = Vec::new();
        for start in [10.0_f64, 80.0, 150.0, 220.0] {
            let k0 = ((start - 0.1) / p).ceil() as i64;
            beats.extend((k0..k0 + 64).map(|k| quantized(0.1 + k as f64 * p)));
        }
        let downs: Vec<f64> = beats
            .iter()
            .copied()
            .filter(|t| ((((t - 0.1) / p).round() as i64) % 4) == 0)
            .collect();
        let Beatgrid::Constant { anchor_sec, bpm } = build(&downs, &beats, Some(0.2), 0.0).unwrap() else {
            panic!("expected constant")
        };
        let (a, gp) = (anchor_sec as f64, 60.0 / bpm as f64);
        let last = *beats.last().unwrap();
        let true_last = 0.1 + ((last - 0.1) / p).round() * p;
        let grid_last = a + ((true_last - a) / gp).round() * gp;
        assert!((grid_last - true_last).abs() < 0.005, "off by {} s at the end", grid_last - true_last);
        assert!((bpm - 131.0).abs() < 0.01, "bpm {bpm}");
    }

    #[test]
    fn the_timing_offset_moves_the_whole_grid() {
        let beats: Vec<f64> = (0..160).map(|i| 0.05 + i as f64 * 0.5).collect();
        let db = steady_downbeats(40, 2.0);
        let Beatgrid::Constant { anchor_sec, .. } = build(&db, &beats, Some(0.2), -0.018).unwrap() else {
            panic!("expected constant")
        };
        assert!((anchor_sec as f64 - 0.032).abs() < 0.002, "anchor {anchor_sec}");
        let Beatgrid::Dynamic { markers, .. } = build(&db, &beats, Some(4.0), -0.018).unwrap() else {
            panic!("expected dynamic")
        };
        assert!((markers[0].pos_sec as f64 - 0.032).abs() < 0.002);
    }

    #[test]
    fn only_lossless_files_get_the_model_offset() {
        use std::path::Path;
        for f in ["a.wav", "a.WAV", "a.aif", "a.aiff", "a.flac"] {
            assert_eq!(timing_offset(Path::new(f)), MODEL_OFFSET_SEC, "{f}");
        }
        for f in ["a.mp3", "a.m4a", "a"] {
            assert_eq!(timing_offset(Path::new(f)), 0.0, "{f}");
        }
    }

    #[test]
    fn too_few_downbeats_is_an_error() {
        assert!(build(&[0.5, 2.5], &[0.5, 1.0], Some(0.1), 0.0).is_err());
    }

    #[test]
    fn geob_body_round_trips_and_has_the_serato_descriptor() {
        let g = Beatgrid::Constant { anchor_sec: 0.01, bpm: 174.0 };
        let body = geob_body(&g);
        assert_eq!(body[0], 0, "latin1 encoding byte");
        let s = String::from_utf8_lossy(&body);
        assert!(s.contains("application/octet-stream"));
        assert!(s.contains("Serato BeatGrid"));
        assert_eq!(grid_from_geob_body(&body).unwrap(), Some(g));
    }

    #[test]
    fn vorbis_value_round_trips() {
        let g = Beatgrid::Dynamic {
            markers: vec![Marker { pos_sec: 0.2, beats_to_next: 4 }],
            terminal_sec: 1.9, terminal_bpm: 120.0,
        };
        let v = vorbis_value(&g);
        assert_eq!(grid_from_vorbis_value(&v).unwrap(), Some(g));
    }

    #[test]
    fn b64_matches_known_vectors() {
        assert_eq!(b64_encode_truncated(b"Man"), "TWFu");
        assert_eq!(b64_encode_truncated(b"Ma"), "TW", "16 bits: floor(16/6) = 2 chars, no padding");
        assert_eq!(b64_encode_truncated(b"M"), "T");
        assert_eq!(b64_decode("TWFu").unwrap(), b"Man");
        assert_eq!(b64_decode("TWE=").unwrap(), b"Ma");
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    // Every payload below was captured from a Serato-written file on the
    // acceptance stick (2026-09-24). 317 of 317 grids there obey
    // `len == 7 + 8 * count`: the count INCLUDES the terminal marker.

    #[test]
    fn parses_a_serato_constant_grid() {
        // FEZZO - Million Dollar.mp3, before SpinDex touched it.
        let g = parse_payload(&hex("0100000000013d4d899843030000d0")).unwrap().unwrap();
        let Beatgrid::Constant { anchor_sec, bpm } = g else { panic!("expected constant: {g:?}") };
        assert!((anchor_sec - 0.0502).abs() < 1e-3, "{anchor_sec}");
        assert_eq!(bpm, 131.0);
    }

    #[test]
    fn parses_a_serato_constant_grid_anchored_before_zero() {
        let g = parse_payload(&hex("010000000001b88af3c542fc0000c0")).unwrap().unwrap();
        let Beatgrid::Constant { anchor_sec, bpm } = g else { panic!("expected constant: {g:?}") };
        assert!(anchor_sec < 0.0 && anchor_sec > -0.001, "{anchor_sec}");
        assert_eq!(bpm, 126.0);
    }

    #[test]
    fn parses_a_serato_grid_with_a_negative_marker() {
        // Serato places the first marker before 0 s when the downbeat
        // precedes the audio's first sample.
        let g = parse_payload(&hex("010000000002bddd626c000000043fef956042f79f282e")).unwrap().unwrap();
        let Beatgrid::Dynamic { markers, terminal_sec, terminal_bpm } = g else {
            panic!("expected dynamic: {g:?}")
        };
        assert_eq!(markers.len(), 1);
        assert!((markers[0].pos_sec + 0.108).abs() < 1e-3, "{}", markers[0].pos_sec);
        assert_eq!(markers[0].beats_to_next, 4);
        assert!((terminal_sec - 1.872).abs() < 1e-3, "{terminal_sec}");
        assert!((terminal_bpm - 123.81).abs() < 0.01, "{terminal_bpm}");
    }

    // Serato's FLAC form, captured from the stick: base64 of (data + one 0x00),
    // no '=' padding, cut to whole 6-bit characters, '\n' every 72 characters.
    const SERATO_FLAC_CONSTANT: &str =
        "YXBwbGljYXRpb24vb2N0ZXQtc3RyZWFtAABTZXJhdG8gQmVhdEdyaWQAAQAAAAABPIjnpEL4\nAAAAA";
    const SERATO_FLAC_TWO_MARKERS: &str =
        "YXBwbGljYXRpb24vb2N0ZXQtc3RyZWFtAABTZXJhdG8gQmVhdEdyaWQAAQAAAAACPp5RVgAA\nAVxDKf0MQvXzjwAA";

    #[test]
    fn parses_serato_flac_value_with_a_dangling_character() {
        // 77 characters: length mod 4 == 1, which strict base64 rejects.
        let g = grid_from_vorbis_value(SERATO_FLAC_CONSTANT).unwrap().unwrap();
        assert_eq!(g, Beatgrid::Constant { anchor_sec: f32::from_bits(0x3c88_e7a4), bpm: 124.0 });
    }

    #[test]
    fn parses_serato_flac_value_with_the_trailing_zero_byte() {
        // 88 characters decode to whole bytes, so Serato's appended 0x00
        // survives after the footer.
        let g = grid_from_vorbis_value(SERATO_FLAC_TWO_MARKERS).unwrap().unwrap();
        let Beatgrid::Dynamic { markers, terminal_bpm, .. } = g else { panic!("{g:?}") };
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].beats_to_next, 348);
        assert!((terminal_bpm - 122.98).abs() < 0.01, "{terminal_bpm}");
    }

    #[test]
    fn vorbis_value_is_byte_identical_to_seratos_own() {
        let g = Beatgrid::Constant { anchor_sec: f32::from_bits(0x3c88_e7a4), bpm: 124.0 };
        assert_eq!(vorbis_value(&g), SERATO_FLAC_CONSTANT);
    }

    #[test]
    fn a_serato_empty_grid_is_no_grid() {
        // Count 0 is how Serato writes "no grid" (it wrote exactly this after
        // rejecting SpinDex's first-generation grid): a valid "no grid", not an error.
        assert_eq!(parse_payload(&hex("01000000000030")).unwrap(), None);
    }

    #[test]
    fn constant_grid_serializes_like_serato() {
        let g = Beatgrid::Constant { anchor_sec: 0.0502, bpm: 131.0 };
        let bytes = serialize_payload(&g);
        assert_eq!(&bytes[..2], &[0x01, 0x00]);
        assert_eq!(&bytes[2..6], &[0, 0, 0, 1], "one marker: the terminal");
        assert_eq!(bytes.len(), 15);
        assert_eq!(parse_payload(&bytes).unwrap(), Some(g));
    }

    #[test]
    fn dynamic_grid_count_includes_the_terminal() {
        let g = Beatgrid::Dynamic {
            markers: vec![Marker { pos_sec: 0.1, beats_to_next: 4 }],
            terminal_sec: 1.9,
            terminal_bpm: 126.0,
        };
        let bytes = serialize_payload(&g);
        assert_eq!(&bytes[2..6], &[0, 0, 0, 2]);
        assert_eq!(bytes.len(), 7 + 8 * 2);
    }

    #[test]
    fn dynamic_grid_round_trips() {
        let g = Beatgrid::Dynamic {
            markers: vec![
                Marker { pos_sec: 0.1, beats_to_next: 4 },
                Marker { pos_sec: 2.0, beats_to_next: 4 },
            ],
            terminal_sec: 3.9,
            terminal_bpm: 126.0,
        };
        assert_eq!(parse_payload(&serialize_payload(&g)).unwrap(), Some(g));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_payload(&[]).is_err());
        assert!(parse_payload(&[0x02, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_err()); // bad version
        assert!(parse_payload(&[0x01, 0x00, 0, 0, 0, 9, 1]).is_err()); // truncated
    }
}
