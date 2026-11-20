//! Serato's cue points, saved loops, track colour and grid lock, stored in a
//! `Serato Markers2` GEOB frame (MP3/WAV/AIFF) or a base64 `SERATO_MARKERS_V2`
//! Vorbis comment (FLAC). Layout verified against the acceptance stick.
use anyhow::{bail, Context, Result};

use crate::core::serato_beatgrid::b64_decode;

pub const DESCRIPTOR: &str = "Serato Markers2";
pub const VORBIS_KEY: &str = "SERATO_MARKERS_V2";

#[derive(Debug, Clone, PartialEq)]
pub struct SeratoCue {
    pub slot: u8,
    pub pos_ms: u32,
    pub rgb: [u8; 3],
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeratoLoop {
    pub slot: u8,
    pub start_ms: u32,
    pub end_ms: u32,
    pub rgb: [u8; 3],
    pub locked: bool,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SeratoMarkers {
    pub cues: Vec<SeratoCue>,
    pub loops: Vec<SeratoLoop>,
    /// None when Serato's "no colour" white (ffffff) or no COLOR entry.
    pub colour: Option<[u8; 3]>,
    pub bpm_locked: bool,
}

fn u32be(b: &[u8], at: usize) -> Result<u32> {
    let s = b.get(at..at + 4).context("truncated marker entry")?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn name_at(b: &[u8], at: usize) -> String {
    let rest = b.get(at..).unwrap_or_default();
    let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
    String::from_utf8_lossy(&rest[..end]).into_owned()
}

/// `[0]=00 [1]=slot [2..6]=pos_ms BE [6]=00 [7..10]=RGB [10..12]=00 00 [12..]=name\0`
pub(crate) fn parse_cue(b: &[u8]) -> Result<SeratoCue> {
    if b.len() < 12 {
        bail!("CUE entry is {} bytes, need at least 12", b.len());
    }
    Ok(SeratoCue { slot: b[1], pos_ms: u32be(b, 2)?, rgb: [b[7], b[8], b[9]], name: name_at(b, 12) })
}

/// `[0]=00 [1]=slot [2..6]=start_ms [6..10]=end_ms [10..14]=ff ff ff ff
///  [14..18]=A R G B [18]=00 [19]=locked [20..]=name\0`
pub(crate) fn parse_loop(b: &[u8]) -> Result<SeratoLoop> {
    if b.len() < 20 {
        bail!("LOOP entry is {} bytes, need at least 20", b.len());
    }
    Ok(SeratoLoop {
        slot: b[1],
        start_ms: u32be(b, 2)?,
        end_ms: u32be(b, 6)?,
        rgb: [b[15], b[16], b[17]],
        locked: b[19] != 0,
        name: name_at(b, 20),
    })
}

/// Parses the base64-decoded body (starts with `01 01`).
pub(crate) fn parse_decoded(d: &[u8]) -> Result<SeratoMarkers> {
    if d.get(..2) != Some(&[0x01, 0x01][..]) {
        bail!("unexpected Serato Markers2 version");
    }
    let mut m = SeratoMarkers::default();
    let mut i = 2;
    while i < d.len() {
        let nul = d[i..].iter().position(|&c| c == 0).context("unterminated entry name")?;
        let name = &d[i..i + nul];
        if name.is_empty() {
            break;
        }
        let len = u32be(d, i + nul + 1)? as usize;
        let start = i + nul + 5;
        let body = d.get(start..start + len).context("marker entry runs past the data")?;
        match name {
            b"CUE" => m.cues.push(parse_cue(body)?),
            b"LOOP" => m.loops.push(parse_loop(body)?),
            b"COLOR" if body.len() >= 4 => {
                let rgb = [body[1], body[2], body[3]];
                m.colour = (rgb != [0xff, 0xff, 0xff]).then_some(rgb);
            }
            b"BPMLOCK" if !body.is_empty() => m.bpm_locked = body[0] != 0,
            _ => {} // unknown or short entry: skipped by its length
        }
        i = start + len;
    }
    Ok(m)
}

/// Parses a `Serato Markers2` GEOB payload: `01 01` + base64 text (NUL-ended).
pub fn parse_markers2(payload: &[u8]) -> Result<SeratoMarkers> {
    if payload.get(..2) != Some(&[0x01, 0x01][..]) {
        bail!("unexpected Serato Markers2 frame version");
    }
    let text = &payload[2..];
    let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    let text = std::str::from_utf8(&text[..end]).context("Serato Markers2 text is not ASCII")?;
    parse_decoded(&b64_decode(text)?)
}

/// FLAC: the Vorbis value is base64(mime\0\0descriptor\0 + GEOB-style payload).
pub fn markers_from_vorbis_value(v: &str) -> Result<SeratoMarkers> {
    let raw = b64_decode(v)?;
    let mut it = raw.splitn(3, |&c| c == 0);
    let _mime = it.next().context("no mime")?;
    let _empty = it.next().context("no separator")?;
    let after = it.next().context("no payload section")?;
    let nul = after.iter().position(|&c| c == 0).context("no descriptor terminator")?;
    parse_markers2(&after[nul + 1..])
}

// ---------------------------------------------------------------------
// Raw entries: the writing side. Every entry is kept as bytes, including
// ones SpinDex does not understand, so a rewrite changes only what was
// added. Encoding rules derived 2026-09-30 from 91 Serato-written frames on
// the acceptance stick: the decoded data is `01 01` + entries + one `00`,
// always a multiple of 3 bytes; the frame is `01 01` + base64 without `=`
// and a `\n` after every 72 characters, then `00`, zero-padded to 470 bytes.
// ---------------------------------------------------------------------

/// One Markers2 entry: its name ("CUE", "LOOP", "COLOR", …) and body bytes.
pub type Entry = (String, Vec<u8>);

/// Every entry of the base64-decoded data, in file order.
pub fn decoded_entries(d: &[u8]) -> Result<Vec<Entry>> {
    if d.get(..2) != Some(&[0x01, 0x01][..]) {
        bail!("unexpected Serato Markers2 version");
    }
    let mut out = Vec::new();
    let mut i = 2;
    while i < d.len() {
        let nul = d[i..].iter().position(|&c| c == 0).context("unterminated entry name")?;
        if nul == 0 {
            break;
        }
        let name = String::from_utf8_lossy(&d[i..i + nul]).into_owned();
        let len = u32be(d, i + nul + 1)? as usize;
        let start = i + nul + 5;
        let body = d.get(start..start + len).context("marker entry runs past the data")?;
        out.push((name, body.to_vec()));
        i = start + len;
    }
    Ok(out)
}

/// The entries of a `Serato Markers2` GEOB payload.
pub fn entries_from_payload(payload: &[u8]) -> Result<Vec<Entry>> {
    if payload.get(..2) != Some(&[0x01, 0x01][..]) {
        bail!("unexpected Serato Markers2 frame version");
    }
    let text = &payload[2..];
    let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    let text = std::str::from_utf8(&text[..end]).context("Serato Markers2 text is not ASCII")?;
    decoded_entries(&b64_decode(text)?)
}

fn decoded_bytes(entries: &[Entry]) -> Vec<u8> {
    let mut d = vec![0x01, 0x01];
    for (name, body) in entries {
        d.extend_from_slice(name.as_bytes());
        d.push(0);
        d.extend_from_slice(&(body.len() as u32).to_be_bytes());
        d.extend_from_slice(body);
    }
    d.push(0);
    while d.len() % 3 != 0 {
        d.push(0);
    }
    d
}

/// Serato never writes a Markers2 GEOB payload shorter than this.
const MIN_PAYLOAD: usize = 470;
/// Inside a FLAC value the same payload is zero-padded to at least this many
/// bytes. Observed on every Serato-written FLAC on the acceptance stick
/// (2026-09-30); all of them carried a short payload, so how Serato pads a
/// longer one is not known.
const MIN_FLAC_PAYLOAD: usize = 515;

/// The GEOB payload for `entries`, byte-identical to Serato's own.
pub fn encode_payload(entries: &[Entry]) -> Vec<u8> {
    payload_with_min(entries, MIN_PAYLOAD)
}

fn payload_with_min(entries: &[Entry], min: usize) -> Vec<u8> {
    let text = crate::core::serato_beatgrid::b64_encode_truncated(&decoded_bytes(entries));
    let mut p = vec![0x01, 0x01];
    for (i, ch) in text.bytes().enumerate() {
        if i > 0 && i % 72 == 0 {
            p.push(b'\n');
        }
        p.push(ch);
    }
    p.push(0);
    if p.len() < min {
        p.resize(min, 0);
    }
    p
}

const MIME: &[u8] = b"application/octet-stream";

/// The FLAC `SERATO_MARKERS_V2` value: base64 of `mime\0\0descriptor\0` plus
/// the payload (padded to `MIN_FLAC_PAYLOAD`), without `=`, a `\n` after
/// every 72 characters — byte-identical to the stick's FLAC files.
pub fn vorbis_value(entries: &[Entry]) -> String {
    let mut raw = Vec::new();
    raw.extend_from_slice(MIME);
    raw.push(0);
    raw.push(0);
    raw.extend_from_slice(DESCRIPTOR.as_bytes());
    raw.push(0);
    raw.extend_from_slice(&payload_with_min(entries, MIN_FLAC_PAYLOAD));
    let chars = crate::core::serato_beatgrid::b64_encode_truncated(&raw);
    chars
        .as_bytes()
        .chunks(72)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The entries of a FLAC `SERATO_MARKERS_V2` value.
pub fn entries_from_vorbis_value(v: &str) -> Result<Vec<Entry>> {
    let raw = b64_decode(v)?;
    let mut it = raw.splitn(3, |&c| c == 0);
    let _mime = it.next().context("no mime")?;
    let _empty = it.next().context("no separator")?;
    let after = it.next().context("no payload section")?;
    let nul = after.iter().position(|&c| c == 0).context("no descriptor terminator")?;
    entries_from_payload(&after[nul + 1..])
}

/// Body of a CUE entry (the inverse of `parse_cue`).
pub fn cue_body(c: &SeratoCue) -> Vec<u8> {
    let mut b = vec![0x00, c.slot];
    b.extend_from_slice(&c.pos_ms.to_be_bytes());
    b.push(0x00);
    b.extend_from_slice(&c.rgb);
    b.extend_from_slice(&[0x00, 0x00]);
    b.extend_from_slice(c.name.as_bytes());
    b.push(0x00);
    b
}

/// Body of a LOOP entry (the inverse of `parse_loop`).
pub fn loop_body(l: &SeratoLoop) -> Vec<u8> {
    let mut b = vec![0x00, l.slot];
    b.extend_from_slice(&l.start_ms.to_be_bytes());
    b.extend_from_slice(&l.end_ms.to_be_bytes());
    b.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0x00]);
    b.extend_from_slice(&l.rgb);
    b.push(0x00);
    b.push(l.locked as u8);
    b.extend_from_slice(l.name.as_bytes());
    b.push(0x00);
    b
}

/// Body of the COLOR (track colour) entry.
pub fn colour_body(rgb: [u8; 3]) -> Vec<u8> {
    vec![0x00, rgb[0], rgb[1], rgb[2]]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geob_payload(text: &str) -> Vec<u8> {
        let mut p = vec![0x01, 0x01];
        p.extend_from_slice(text.as_bytes());
        p.push(0);
        p.extend(std::iter::repeat(0).take(40)); // Serato pads the frame
        p
    }

    // all things break - blackout blackout (Original Mix).aiff
    const ONE_CUE: &str = "AQFDT0xPUgAAAAAEAP///0NVRQAAAAANAAAAAHF4AMwAAAAAAEJQTUxPQ0sAAAAAAQAA";
    // MK - Chemical (Extended Mix).aiff: note the line break inside the text
    const ONE_LOOP: &str = "AQFDT0xPUgAAAAAEAP///0xPT1AAAAAAFQAAAAAAFAAACST/////ACeq4QAAAEJQTUxPQ0sA\nAAAAAQAA";

    #[test]
    fn parses_a_real_hot_cue() {
        let m = parse_markers2(&geob_payload(ONE_CUE)).unwrap();
        assert_eq!(
            m.cues,
            vec![SeratoCue { slot: 0, pos_ms: 0x7178, rgb: [0xcc, 0, 0], name: String::new() }]
        );
        assert!(m.loops.is_empty());
        assert_eq!(m.colour, None, "ffffff means no colour");
        assert!(!m.bpm_locked);
    }

    #[test]
    fn parses_a_real_loop_across_a_line_break() {
        let m = parse_markers2(&geob_payload(ONE_LOOP)).unwrap();
        assert_eq!(
            m.loops,
            vec![SeratoLoop {
                slot: 0,
                start_ms: 0x14,
                end_ms: 0x924,
                rgb: [0x27, 0xaa, 0xe1],
                locked: false,
                name: String::new(),
            }]
        );
    }

    #[test]
    fn parses_a_real_named_cue_body() {
        // End Of Beginning Remix Mixed5.mp3: slot 0, 1.935 s, red, "Cue 1"
        let body = [0x00, 0x00, 0x00, 0x00, 0x07, 0x8f, 0x00, 0xcc, 0x00, 0x00, 0x00, 0x00, b'C', b'u', b'e', b' ', b'1', 0x00];
        let c = parse_cue(&body).unwrap();
        assert_eq!(c, SeratoCue { slot: 0, pos_ms: 0x78f, rgb: [0xcc, 0, 0], name: "Cue 1".into() });
    }

    #[test]
    fn reads_colour_and_bpm_lock() {
        let mut decoded = vec![0x01, 0x01];
        decoded.extend_from_slice(b"COLOR\0");
        decoded.extend_from_slice(&4u32.to_be_bytes());
        decoded.extend_from_slice(&[0x00, 0xcc, 0x00, 0x00]);
        decoded.extend_from_slice(b"BPMLOCK\0");
        decoded.extend_from_slice(&1u32.to_be_bytes());
        decoded.push(1);
        decoded.extend_from_slice(b"FUTURE\0"); // unknown entries are skipped
        decoded.extend_from_slice(&2u32.to_be_bytes());
        decoded.extend_from_slice(&[9, 9]);
        decoded.push(0);
        let m = parse_decoded(&decoded).unwrap();
        assert_eq!(m.colour, Some([0xcc, 0, 0]));
        assert!(m.bpm_locked);
    }

    #[test]
    fn truncated_data_is_an_error_not_a_panic() {
        let mut decoded = vec![0x01, 0x01];
        decoded.extend_from_slice(b"CUE\0");
        decoded.extend_from_slice(&13u32.to_be_bytes());
        decoded.extend_from_slice(&[0, 0, 0]); // body cut short
        assert!(parse_decoded(&decoded).is_err());
        assert!(parse_markers2(&[0x01]).is_err());
        assert!(parse_markers2(&[0x02, 0x01, b'A']).is_err());
        assert!(parse_cue(&[0, 1, 2]).is_err());
        assert!(parse_loop(&[0; 10]).is_err());
    }

    #[test]
    fn flac_value_decodes_through_the_outer_envelope() {
        // FLAC stores base64("application/octet-stream\0\0Serato Markers2\0" + payload)
        let mut raw = b"application/octet-stream\0\0Serato Markers2\0".to_vec();
        raw.extend(geob_payload(ONE_CUE));
        let v = b64_encode_for_test(&raw);
        let m = markers_from_vorbis_value(&v).unwrap();
        assert_eq!(m.cues.len(), 1);
    }

    fn b64_encode_for_test(data: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in data.chunks(3) {
            let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            s.push(T[(n >> 18) as usize & 63] as char);
            s.push(T[(n >> 12) as usize & 63] as char);
            s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
            s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
        }
        s
    }

    /// A GEOB payload exactly as Serato writes it: `01 01` + the base64 text
    /// (with its own line breaks) + one NUL, zero-padded to 470 bytes.
    fn serato_frame(text: &str) -> Vec<u8> {
        let mut p = vec![0x01, 0x01];
        p.extend_from_slice(text.as_bytes());
        p.push(0);
        p.resize(470, 0);
        p
    }

    #[test]
    fn re_encodes_real_serato_frames_byte_for_byte() {
        for text in [ONE_CUE, ONE_LOOP] {
            let frame = serato_frame(text);
            let entries = entries_from_payload(&frame).unwrap();
            assert_eq!(encode_payload(&entries), frame, "frame for {text:?}");
        }
    }

    #[test]
    fn keeps_unknown_entries_and_their_order() {
        let entries: Vec<Entry> = vec![
            ("COLOR".into(), colour_body([0xff, 0xff, 0xff])),
            ("FLIP".into(), vec![1, 2, 3, 4, 5]),
            ("BPMLOCK".into(), vec![0]),
        ];
        let back = entries_from_payload(&encode_payload(&entries)).unwrap();
        assert_eq!(back, entries);
    }

    #[test]
    fn long_text_wraps_after_72_characters() {
        let entries: Vec<Entry> =
            (0..8u8).map(|i| ("CUE".to_string(), cue_body(&SeratoCue { slot: i, pos_ms: 1000 * i as u32, rgb: [0xcc, 0, 0], name: String::new() }))).collect();
        let p = encode_payload(&entries);
        assert_eq!(p[2 + 72], b'\n', "a line break after the first 72 characters");
        assert!(p.len() > 470 || p[p.len() - 1] == 0);
        assert_eq!(entries_from_payload(&p).unwrap(), entries);
    }

    #[test]
    fn bodies_match_what_serato_writes() {
        let named = [0x00, 0x00, 0x00, 0x00, 0x07, 0x8f, 0x00, 0xcc, 0x00, 0x00, 0x00, 0x00, b'C', b'u', b'e', b' ', b'1', 0x00];
        let cue = parse_cue(&named).unwrap();
        assert_eq!(cue_body(&cue), named.to_vec());
        let entries = entries_from_payload(&serato_frame(ONE_LOOP)).unwrap();
        let (_, raw_loop) = entries.iter().find(|(n, _)| n == "LOOP").unwrap();
        assert_eq!(&loop_body(&parse_loop(raw_loop).unwrap()), raw_loop);
        let (_, raw_colour) = entries.iter().find(|(n, _)| n == "COLOR").unwrap();
        assert_eq!(&colour_body([0xff, 0xff, 0xff]), raw_colour);
    }

    #[test]
    fn flac_value_round_trips_through_entries() {
        let entries = entries_from_payload(&serato_frame(ONE_CUE)).unwrap();
        let v = vorbis_value(&entries);
        assert_eq!(entries_from_vorbis_value(&v).unwrap(), entries);
    }
}
