//! Builds Rekordbox's `rekordbox.xml` (DJ_PLAYLISTS 1.0.0) from the DJ model.
//! Pure: no I/O. Rekordbox imports this through Preferences → Advanced →
//! rekordbox xml.
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::core::djlib::capabilities::REKORDBOX_TRACK_COLOURS;
use crate::core::djlib::convert::{camelot_to_musical, nearest_colour};
use crate::core::djlib::model::{
    Cue, CueKind, DjLibrary, DjPlaylistNode, DjTrack, Loop, Rgb, TempoSection,
};

/// Escapes an attribute value and drops characters XML 1.0 forbids
/// (control characters other than tab, newline, carriage return).
pub(crate) fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 => {}
            '\u{FFFE}' | '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

/// `D:\Library AIFF\x.aiff` → `file://localhost/D:/Library%20AIFF/x.aiff`.
/// Every byte outside `A-Za-z0-9-._~` is percent-encoded, per path segment;
/// the drive segment ("D:") is kept verbatim.
pub fn location_uri(abs: &Path) -> String {
    let s = abs.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://localhost/");
    for (i, seg) in s.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        let is_drive = i == 0 && seg.len() == 2 && seg.ends_with(':');
        if is_drive {
            out.push_str(seg);
            continue;
        }
        for b in seg.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                out.push(b as char);
            } else {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

fn kind(format: Option<&str>) -> Option<&'static str> {
    match format?.to_ascii_lowercase().as_str() {
        "mp3" => Some("MP3 File"),
        "wav" => Some("WAV File"),
        "aiff" | "aif" => Some("AIFF File"),
        "flac" => Some("FLAC File"),
        "m4a" | "aac" => Some("M4A File"),
        _ => None,
    }
}

fn attr(out: &mut String, name: &str, value: Option<String>) {
    if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
        let _ = write!(out, " {name}=\"{}\"", escape_attr(&v));
    }
}

fn write_track(out: &mut String, id: usize, t: &DjTrack) {
    out.push_str("    <TRACK");
    attr(out, "TrackID", Some(id.to_string()));
    attr(out, "Name", t.title.clone());
    attr(out, "Artist", t.artist.clone());
    attr(out, "Album", t.album.clone());
    attr(out, "Genre", t.genre.clone());
    attr(out, "Kind", kind(t.format.as_deref()).map(str::to_string));
    attr(out, "Size", Some(t.file_size.to_string()));
    attr(out, "TotalTime", t.duration_secs.map(|d| format!("{}", d.round() as i64)));
    attr(out, "Year", t.year.map(|y| y.to_string()));
    attr(out, "AverageBpm", t.bpm.map(|b| format!("{b:.2}")));
    attr(out, "BitRate", t.bitrate_kbps.map(|b| b.to_string()));
    attr(out, "SampleRate", t.sample_rate.map(|s| s.to_string()));
    attr(out, "Comments", t.comment.clone());
    attr(out, "Tonality", t.key_camelot.as_deref().and_then(camelot_to_musical).map(str::to_string));
    attr(out, "Label", t.label.clone());
    attr(out, "Location", Some(location_uri(&t.abs_path)));
    attr(
        out,
        "Colour",
        t.colour.map(|c| {
            let p: Rgb = nearest_colour(c, &REKORDBOX_TRACK_COLOURS);
            format!("0x{:02X}{:02X}{:02X}", p.r, p.g, p.b)
        }),
    );
    out.push_str(">\n");
    for s in &t.grid {
        let _ = writeln!(
            out,
            "      <TEMPO Inizio=\"{:.3}\" Bpm=\"{:.2}\" Metro=\"4/4\" Battito=\"{}\"/>",
            s.start_sec, s.bpm, s.beat_in_bar
        );
    }
    for c in &t.cues {
        let num = match c.kind {
            CueKind::Hot { slot } => slot as i32,
            CueKind::Memory => -1,
        };
        let _ = write!(
            out,
            "      <POSITION_MARK Name=\"{}\" Type=\"0\" Start=\"{:.3}\" Num=\"{num}\"",
            escape_attr(&c.name),
            c.pos_sec
        );
        if let Some(col) = c.colour {
            let _ = write!(out, " Red=\"{}\" Green=\"{}\" Blue=\"{}\"", col.r, col.g, col.b);
        }
        out.push_str("/>\n");
    }
    for l in &t.loops {
        let num = l.slot.map(|s| s as i32).unwrap_or(-1);
        let _ = writeln!(
            out,
            "      <POSITION_MARK Name=\"{}\" Type=\"4\" Start=\"{:.3}\" End=\"{:.3}\" Num=\"{num}\"/>",
            escape_attr(&l.name),
            l.start_sec,
            l.end_sec
        );
    }
    out.push_str("    </TRACK>\n");
}

fn write_node(out: &mut String, n: &DjPlaylistNode, depth: usize) {
    let pad = "  ".repeat(depth);
    match n {
        DjPlaylistNode::Folder { name, children } => {
            let _ = writeln!(
                out,
                "{pad}<NODE Type=\"0\" Name=\"{}\" Count=\"{}\">",
                escape_attr(name),
                children.len()
            );
            for c in children {
                write_node(out, c, depth + 1);
            }
            let _ = writeln!(out, "{pad}</NODE>");
        }
        DjPlaylistNode::Playlist { name, track_refs } => {
            let _ = writeln!(
                out,
                "{pad}<NODE Type=\"1\" Name=\"{}\" KeyType=\"0\" Entries=\"{}\">",
                escape_attr(name),
                track_refs.len()
            );
            for r in track_refs {
                let _ = writeln!(out, "{pad}  <TRACK Key=\"{}\"/>", r + 1);
            }
            let _ = writeln!(out, "{pad}</NODE>");
        }
    }
}

/// The whole document. TrackID = index + 1; playlists reference it by `Key`.
pub fn write_xml(lib: &DjLibrary, product_version: &str) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<DJ_PLAYLISTS Version=\"1.0.0\">\n");
    let _ = writeln!(
        out,
        "  <PRODUCT Name=\"SpinDex\" Version=\"{}\" Company=\"SpinDex\"/>",
        escape_attr(product_version)
    );
    let _ = writeln!(out, "  <COLLECTION Entries=\"{}\">", lib.tracks.len());
    for (i, t) in lib.tracks.iter().enumerate() {
        write_track(&mut out, i + 1, t);
    }
    out.push_str("  </COLLECTION>\n  <PLAYLISTS>\n");
    let _ = writeln!(out, "    <NODE Type=\"0\" Name=\"ROOT\" Count=\"{}\">", lib.playlists.len());
    for n in &lib.playlists {
        write_node(&mut out, n, 3);
    }
    out.push_str("    </NODE>\n  </PLAYLISTS>\n</DJ_PLAYLISTS>\n");
    out
}

// ---------------------------------------------------------------------
// Reading: the playlist tree back out of a rekordbox.xml
// ---------------------------------------------------------------------

/// One NODE of the playlist tree. Ids are assigned in document order from 1;
/// `parent_id` 0 means directly under ROOT (same shape as the export.pdb
/// viewer's nodes, so the sidebar can render both the same way).
#[derive(Debug, Clone, PartialEq)]
pub struct XmlPlaylist {
    pub id: u32,
    pub parent_id: u32,
    pub name: String,
    pub is_folder: bool,
    /// Tracks in playlist order, as stick-relative paths ("MP3/x.mp3").
    pub stick_paths: Vec<String>,
}

struct XmlTag<'a> {
    name: &'a str,
    closing: bool,
    self_closing: bool,
    attrs: Vec<(&'a str, String)>,
}

impl XmlTag<'_> {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str())
    }
}

/// Named entities plus numeric character references (`&#233;`, `&#xE9;`),
/// which real Rekordbox exports may use. One pass, so a decoded `&` never
/// starts a second entity (`&#38;lt;` is the text "&lt;"). Anything that is
/// not a known reference is kept as written.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let decoded = tail.find(';').filter(|&e| e <= 10).and_then(|e| {
            let name = &tail[..e];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => name.strip_prefix('#').and_then(|n| {
                    match n.strip_prefix('x').or_else(|| n.strip_prefix('X')) {
                        Some(h) => u32::from_str_radix(h, 16).ok(),
                        None => n.parse().ok(),
                    }
                    .and_then(char::from_u32)
                }),
            };
            c.map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &tail[e + 1..];
            }
            None => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

fn parse_attrs(s: &str) -> Result<Vec<(&str, String)>> {
    let mut out = Vec::new();
    let mut rest = s.trim_start();
    while !rest.is_empty() {
        let eq = rest.find('=').context("attribute without a value")?;
        let name = rest[..eq].trim();
        let after = rest[eq + 1..].trim_start();
        let value = after.strip_prefix('"').context("unquoted attribute value")?;
        let close = value.find('"').context("unterminated attribute value")?;
        out.push((name, unescape(&value[..close])));
        rest = value[close + 1..].trim_start();
    }
    Ok(out)
}

/// Every element tag in document order. Enough XML for the files this module
/// writes (and Rekordbox's own exports): no CDATA, no text content used.
fn scan_tags(xml: &str) -> Result<Vec<XmlTag<'_>>> {
    let bytes = xml.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = xml[i..].find('<') {
        let start = i + off + 1;
        let mut j = start;
        let mut in_quote = false;
        while j < bytes.len() {
            match bytes[j] {
                b'"' => in_quote = !in_quote,
                b'>' if !in_quote => break,
                _ => {}
            }
            j += 1;
        }
        if j >= bytes.len() {
            bail!("unterminated tag in rekordbox.xml");
        }
        let inner = &xml[start..j];
        i = j + 1;
        if inner.starts_with('?') || inner.starts_with('!') {
            continue;
        }
        let closing = inner.starts_with('/');
        let body = inner.trim_start_matches('/');
        let self_closing = body.ends_with('/');
        let body = body.trim_end_matches('/');
        let name_end = body.find(char::is_whitespace).unwrap_or(body.len());
        out.push(XmlTag {
            name: &body[..name_end],
            closing,
            self_closing,
            attrs: parse_attrs(&body[name_end..])?,
        });
    }
    Ok(out)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        // hex digits are ASCII: reading bytes never splits a character
        let hex = (b[i] == b'%')
            .then(|| b.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(v) => {
                out.push(v);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `file://localhost/D:/Library%20AIFF/x.aiff` → `Library AIFF/x.aiff`.
/// None for anything that is not a local drive path.
pub fn location_to_stick_rel(loc: &str) -> Option<String> {
    let rest = loc.strip_prefix("file://localhost/").or_else(|| loc.strip_prefix("file:///"))?;
    let decoded = percent_decode(rest);
    let b = decoded.as_bytes();
    (b.len() > 3 && b[1] == b':' && b[2] == b'/').then(|| decoded[3..].to_string())
}

/// The playlist tree of a rekordbox.xml, each playlist with its tracks as
/// stick-relative paths. The ROOT node itself is not returned.
pub fn read_playlists(xml: &str) -> Result<Vec<XmlPlaylist>> {
    let mut locations: HashMap<String, String> = HashMap::new();
    let (mut in_collection, mut in_playlists) = (false, false);
    let mut stack: Vec<u32> = Vec::new(); // open NODE ids; 0 = ROOT
    let mut current: Option<usize> = None; // playlist receiving TRACK keys
    let mut out: Vec<XmlPlaylist> = Vec::new();
    for t in scan_tags(xml)? {
        match (t.name, t.closing) {
            ("COLLECTION", false) => in_collection = !t.self_closing,
            ("COLLECTION", true) => in_collection = false,
            ("PLAYLISTS", false) => in_playlists = !t.self_closing,
            ("PLAYLISTS", true) => in_playlists = false,
            ("TRACK", false) if in_collection => {
                if let (Some(id), Some(loc)) = (t.attr("TrackID"), t.attr("Location")) {
                    if let Some(p) = location_to_stick_rel(loc) {
                        locations.insert(id.to_string(), p);
                    }
                }
            }
            ("TRACK", false) if in_playlists => {
                if let (Some(idx), Some(key)) = (current, t.attr("Key")) {
                    if let Some(p) = locations.get(key) {
                        out[idx].stick_paths.push(p.clone());
                    }
                }
            }
            ("NODE", false) if in_playlists => {
                let name = t.attr("Name").unwrap_or_default().to_string();
                if stack.is_empty() && name == "ROOT" {
                    if !t.self_closing {
                        stack.push(0);
                    }
                    continue;
                }
                let id = out.len() as u32 + 1;
                let is_folder = t.attr("Type") == Some("0");
                let parent_id = stack.last().copied().unwrap_or(0);
                out.push(XmlPlaylist { id, parent_id, name, is_folder, stick_paths: Vec::new() });
                if !t.self_closing {
                    stack.push(id);
                    current = (!is_folder).then(|| out.len() - 1);
                }
            }
            ("NODE", true) if in_playlists => {
                stack.pop();
                current = None;
            }
            _ => {}
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------
// Reading: a whole Rekordbox collection into the DJ model (sync SP2)
// ---------------------------------------------------------------------

/// `file://localhost/C:/a%20b/x.mp3` → `C:\a b\x.mp3`. None if not a local
/// drive path in either form. A location that is not a Windows drive path (a
/// Mac export: `file://localhost/Users/...` or `file:///Volumes/...`) keeps its
/// decoded path with forward slashes: it is never on this stick by path, but
/// its file name still matches by name and size.
pub fn location_to_abs(loc: &str) -> Option<std::path::PathBuf> {
    let rest = loc.strip_prefix("file://localhost/").or_else(|| loc.strip_prefix("file:///"))?;
    let decoded = percent_decode(rest);
    let b = decoded.as_bytes();
    if b.len() > 3 && b[1] == b':' && b[2] == b'/' {
        return Some(std::path::PathBuf::from(decoded.replace('/', "\\")));
    }
    (!decoded.is_empty()).then(|| std::path::PathBuf::from(format!("/{decoded}")))
}

fn f64_attr(t: &XmlTag, name: &str) -> Option<f64> {
    t.attr(name).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite())
}

/// `Colour="0xRRGGBB"`.
fn colour_attr(v: &str) -> Option<Rgb> {
    let hex = v.trim().trim_start_matches("0x").trim_start_matches("0X");
    let n = u32::from_str_radix(hex, 16).ok()?;
    Some(Rgb { r: (n >> 16) as u8, g: (n >> 8) as u8, b: n as u8 })
}

/// A mark's `Red`/`Green`/`Blue` (hot cues only; memory marks carry none).
fn mark_colour(t: &XmlTag) -> Option<Rgb> {
    let c = |n: &str| t.attr(n).and_then(|v| v.trim().parse::<u8>().ok());
    Some(Rgb { r: c("Red")?, g: c("Green")?, b: c("Blue")? })
}

/// One open playlist NODE while reading.
struct NodeFrame {
    name: String,
    folder: bool,
    /// "0" = entries are TrackIDs, "1" = entries are locations.
    key_type: String,
    refs: Vec<usize>,
    children: Vec<DjPlaylistNode>,
}

/// A Rekordbox XML collection and playlist tree in model form. Hot cues are
/// `Type="0"` marks with `Num` 0..7, memory cues `Num="-1"`; loops are
/// `Type="4"` marks with an `End`. Fade and load marks carry nothing Serato
/// can use and are skipped.
pub fn read_library(xml: &str) -> Result<DjLibrary> {
    let mut lib = DjLibrary::default();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    let mut by_loc: HashMap<String, usize> = HashMap::new();
    let (mut in_collection, mut in_playlists) = (false, false);
    let mut current: Option<usize> = None;
    let mut stack: Vec<NodeFrame> = Vec::new();
    for t in scan_tags(xml)? {
        match (t.name, t.closing) {
            ("COLLECTION", false) => in_collection = !t.self_closing,
            ("COLLECTION", true) => in_collection = false,
            ("PLAYLISTS", false) => in_playlists = !t.self_closing,
            ("PLAYLISTS", true) => in_playlists = false,
            ("TRACK", false) if in_collection => {
                let loc = t.attr("Location").unwrap_or_default();
                let track = DjTrack {
                    stick_rel_path: location_to_stick_rel(loc).unwrap_or_default(),
                    abs_path: location_to_abs(loc).unwrap_or_default(),
                    title: t.attr("Name").map(str::to_string),
                    artist: t.attr("Artist").map(str::to_string),
                    file_size: t.attr("Size").and_then(|s| s.trim().parse().ok()).unwrap_or(0),
                    duration_secs: f64_attr(&t, "TotalTime"),
                    colour: t.attr("Colour").and_then(colour_attr),
                    ..Default::default()
                };
                let idx = lib.tracks.len();
                if let Some(id) = t.attr("TrackID") {
                    by_id.insert(id.to_string(), idx);
                }
                by_loc.insert(loc.to_string(), idx);
                lib.tracks.push(track);
                current = (!t.self_closing).then_some(idx);
            }
            ("TRACK", true) if in_collection => current = None,
            ("TEMPO", false) => {
                let Some(i) = current else { continue };
                let (Some(start), Some(bpm)) = (f64_attr(&t, "Inizio"), f64_attr(&t, "Bpm")) else {
                    continue;
                };
                let beat = t
                    .attr("Battito")
                    .and_then(|b| b.trim().parse::<u8>().ok())
                    .filter(|b| (1..=4).contains(b))
                    .unwrap_or(1);
                lib.tracks[i].grid.push(TempoSection { start_sec: start, bpm, beat_in_bar: beat });
            }
            ("POSITION_MARK", false) => {
                let Some(i) = current else { continue };
                let Some(start) = f64_attr(&t, "Start") else { continue };
                let num = t.attr("Num").and_then(|n| n.trim().parse::<i32>().ok()).unwrap_or(-1);
                let hot = (0..=7).contains(&num);
                let name = t.attr("Name").unwrap_or_default().to_string();
                let colour = mark_colour(&t);
                match t.attr("Type") {
                    Some("0") => lib.tracks[i].cues.push(Cue {
                        kind: if hot { CueKind::Hot { slot: num as u8 } } else { CueKind::Memory },
                        pos_sec: start,
                        colour,
                        name,
                    }),
                    Some("4") => {
                        if let Some(end) = f64_attr(&t, "End").filter(|e| *e > start) {
                            lib.tracks[i].loops.push(Loop {
                                slot: hot.then_some(num as u8),
                                start_sec: start,
                                end_sec: end,
                                colour,
                                name,
                                locked: false,
                            });
                        }
                    }
                    _ => {} // fade-in/out, load point: nothing to carry
                }
            }
            ("NODE", false) if in_playlists => {
                let name = t.attr("Name").unwrap_or_default().to_string();
                let frame = NodeFrame {
                    folder: t.attr("Type") == Some("0"),
                    key_type: t.attr("KeyType").unwrap_or("0").to_string(),
                    name,
                    refs: Vec::new(),
                    children: Vec::new(),
                };
                if t.self_closing {
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(finish_node(frame));
                    }
                } else {
                    stack.push(frame);
                }
            }
            ("TRACK", false) if in_playlists => {
                if let (Some(f), Some(key)) = (stack.last_mut(), t.attr("Key")) {
                    let hit = if f.key_type == "1" { by_loc.get(key) } else { by_id.get(key) };
                    if let Some(&i) = hit {
                        f.refs.push(i);
                    }
                }
            }
            ("NODE", true) if in_playlists => {
                let Some(f) = stack.pop() else { continue };
                match stack.last_mut() {
                    Some(parent) => parent.children.push(finish_node(f)),
                    // the outermost NODE is ROOT: its children are the tree
                    None => lib.playlists.extend(f.children),
                }
            }
            _ => {}
        }
    }
    Ok(lib)
}

fn finish_node(f: NodeFrame) -> DjPlaylistNode {
    if f.folder {
        DjPlaylistNode::Folder { name: f.name, children: f.children }
    } else {
        DjPlaylistNode::Playlist { name: f.name, track_refs: f.refs }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn lib() -> DjLibrary {
        let t = DjTrack {
            stick_rel_path: "Library AIFF/A & B.aiff".into(),
            abs_path: PathBuf::from("D:\\Library AIFF\\A & B.aiff"),
            title: Some("Rock \"n\" <Roll>".into()),
            artist: Some("Möwe".into()),
            format: Some("aiff".into()),
            file_size: 1234,
            duration_secs: Some(200.6),
            bpm: Some(131.0),
            key_camelot: Some("8A".into()),
            colour: Some(Rgb { r: 0xcc, g: 0, b: 0 }),
            grid: vec![TempoSection { start_sec: 0.0502, bpm: 131.0, beat_in_bar: 1 }],
            cues: vec![Cue {
                kind: CueKind::Hot { slot: 1 },
                pos_sec: 171.243,
                colour: Some(Rgb { r: 0xcc, g: 0x88, b: 0 }),
                name: String::new(),
            }],
            loops: vec![Loop {
                slot: None,
                start_sec: 0.02,
                end_sec: 2.34,
                colour: None,
                name: String::new(),
                locked: false,
            }],
            ..Default::default()
        };
        DjLibrary {
            tracks: vec![t],
            playlists: vec![DjPlaylistNode::Folder {
                name: "House".into(),
                children: vec![DjPlaylistNode::Playlist { name: "Deep".into(), track_refs: vec![0] }],
            }],
        }
    }

    #[test]
    fn writes_collection_track_grid_cues_and_playlists() {
        let x = write_xml(&lib(), "0.3.2");
        assert!(x.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<DJ_PLAYLISTS Version=\"1.0.0\">"));
        assert!(x.contains("<COLLECTION Entries=\"1\">"));
        assert!(x.contains("TrackID=\"1\""));
        assert!(x.contains("AverageBpm=\"131.00\""));
        assert!(x.contains("Tonality=\"Am\""));
        assert!(x.contains("Kind=\"AIFF File\""));
        assert!(x.contains("TotalTime=\"201\""));
        assert!(x.contains("Colour=\"0xFF0000\""));
        assert!(x.contains("<TEMPO Inizio=\"0.050\" Bpm=\"131.00\" Metro=\"4/4\" Battito=\"1\"/>"));
        assert!(x.contains("<POSITION_MARK Name=\"\" Type=\"0\" Start=\"171.243\" Num=\"1\" Red=\"204\" Green=\"136\" Blue=\"0\"/>"));
        assert!(x.contains("<POSITION_MARK Name=\"\" Type=\"4\" Start=\"0.020\" End=\"2.340\" Num=\"-1\"/>"));
        assert!(x.contains("<NODE Type=\"0\" Name=\"ROOT\" Count=\"1\">"));
        assert!(x.contains("<NODE Type=\"0\" Name=\"House\" Count=\"1\">"));
        assert!(x.contains("<NODE Type=\"1\" Name=\"Deep\" KeyType=\"0\" Entries=\"1\">"));
        assert!(x.contains("<TRACK Key=\"1\"/>"));
        assert!(x.trim_end().ends_with("</DJ_PLAYLISTS>"));
    }

    #[test]
    fn escapes_specials_and_strips_control_chars() {
        assert_eq!(escape_attr("a & b <c> \"d\" 'e'"), "a &amp; b &lt;c&gt; &quot;d&quot; &apos;e&apos;");
        assert_eq!(escape_attr("x\u{1}y\tz"), "xy\tz", "XML 1.0 forbids most control chars");
        assert_eq!(escape_attr("a\u{FFFE}b\u{FFFF}c"), "abc", "XML 1.0 forbids U+FFFE and U+FFFF");
        let x = write_xml(&lib(), "0.3.2");
        assert!(x.contains("Name=\"Rock &quot;n&quot; &lt;Roll&gt;\""));
        assert!(x.contains("Artist=\"Möwe\""));
    }

    #[test]
    fn location_uri_percent_encodes_segments() {
        assert_eq!(
            location_uri(Path::new("D:\\Library AIFF\\A & B #1 100%.aiff")),
            "file://localhost/D:/Library%20AIFF/A%20%26%20B%20%231%20100%25.aiff"
        );
        assert_eq!(location_uri(Path::new("D:\\MP3\\Möwe.mp3")), "file://localhost/D:/MP3/M%C3%B6we.mp3");
    }

    #[test]
    fn reads_back_the_playlist_tree_it_wrote() {
        let x = write_xml(&lib(), "0.3.2");
        let p = read_playlists(&x).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].id, p[0].parent_id, p[0].name.as_str(), p[0].is_folder), (1, 0, "House", true));
        assert_eq!((p[1].id, p[1].parent_id, p[1].name.as_str(), p[1].is_folder), (2, 1, "Deep", false));
        assert_eq!(p[1].stick_paths, vec!["Library AIFF/A & B.aiff".to_string()]);
    }

    #[test]
    fn location_decodes_to_a_stick_relative_path() {
        assert_eq!(
            location_to_stick_rel("file://localhost/D:/Library%20AIFF/A%20%26%20B%20%231%20100%25.aiff"),
            Some("Library AIFF/A & B #1 100%.aiff".to_string())
        );
        assert_eq!(location_to_stick_rel("file://localhost/D:/MP3/M%C3%B6we.mp3"), Some("MP3/Möwe.mp3".to_string()));
        assert_eq!(location_to_stick_rel("http://example.com/x.mp3"), None);
    }

    #[test]
    fn malformed_xml_is_an_error_not_a_panic() {
        assert!(read_playlists("<DJ_PLAYLISTS><PLAYLISTS><NODE Name=\"x").is_err());
        assert!(read_playlists("").unwrap().is_empty());
    }

    #[test]
    fn missing_values_are_omitted_not_empty() {
        let mut l = lib();
        l.tracks[0].key_camelot = Some("Abm".into()); // not Camelot
        l.tracks[0].bpm = None;
        l.tracks[0].colour = None;
        let x = write_xml(&l, "0.3.2");
        assert!(!x.contains("Tonality="));
        assert!(!x.contains("AverageBpm="));
        assert!(!x.contains("Colour="));
    }

    const RB_STYLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<DJ_PLAYLISTS Version="1.0.0">
  <PRODUCT Name="rekordbox" Version="6.8.5" Company="AlphaTheta"/>
  <COLLECTION Entries="2">
    <TRACK TrackID="11" Name="Caf&#233; &amp; Bar" Artist="A" Kind="MP3 File" Size="1000" TotalTime="300"
      AverageBpm="124.00" Location="file://localhost/C:/Users/dj/Music/Caf%C3%A9.mp3" Colour="0xFF0000" Rating="0">
      <TEMPO Inizio="0.120" Bpm="124.00" Metro="4/4" Battito="2"/>
      <POSITION_MARK Name="" Type="0" Start="0.120" Num="-1"/>
      <POSITION_MARK Name="Drop" Type="0" Start="60.500" Num="1" Red="40" Green="226" Blue="20"/>
      <POSITION_MARK Name="" Type="4" Start="30.000" End="31.935" Num="0"/>
      <POSITION_MARK Name="" Type="4" Start="90.000" End="92.000" Num="-1"/>
      <POSITION_MARK Name="" Type="3" Start="5.000" Num="-1"/>
    </TRACK>
    <TRACK TrackID="12" Name="Two" Size="2000" Location="file://localhost/D:/Contents/B/two.wav">
      <TEMPO Inizio="0.000" Bpm="120.00" Metro="4/4" Battito="1"/>
      <TEMPO Inizio="64.000" Bpm="122.00" Metro="4/4" Battito="1"/>
    </TRACK>
  </COLLECTION>
  <PLAYLISTS>
    <NODE Type="0" Name="ROOT" Count="2">
      <NODE Type="0" Name="House" Count="1">
        <NODE Type="1" Name="Deep" KeyType="0" Entries="2">
          <TRACK Key="12"/>
          <TRACK Key="11"/>
        </NODE>
      </NODE>
      <NODE Type="1" Name="ByPath" KeyType="1" Entries="1">
        <TRACK Key="file://localhost/D:/Contents/B/two.wav"/>
      </NODE>
    </NODE>
  </PLAYLISTS>
</DJ_PLAYLISTS>"#;

    #[test]
    fn reads_a_rekordbox_style_collection() {
        let lib = read_library(RB_STYLE).unwrap();
        assert_eq!(lib.tracks.len(), 2);
        let a = &lib.tracks[0];
        assert_eq!(a.title.as_deref(), Some("Café & Bar"));
        assert_eq!(a.file_size, 1000);
        assert_eq!(a.stick_rel_path, "Users/dj/Music/Café.mp3");
        assert_eq!(a.abs_path, PathBuf::from(r"C:\Users\dj\Music\Café.mp3"));
        assert_eq!(a.colour, Some(Rgb { r: 0xff, g: 0, b: 0 }));
        assert_eq!(a.grid, vec![TempoSection { start_sec: 0.12, bpm: 124.0, beat_in_bar: 2 }]);
        assert_eq!(a.cues.len(), 2, "hot cue + memory cue; the Type=3 load mark is ignored");
        assert!(a.cues.iter().any(|c| c.kind == CueKind::Memory));
        let hot = a.cues.iter().find(|c| c.kind == CueKind::Hot { slot: 1 }).unwrap();
        assert_eq!((hot.name.as_str(), hot.colour), ("Drop", Some(Rgb { r: 40, g: 226, b: 20 })));
        assert_eq!(a.loops.len(), 2);
        assert_eq!(a.loops.iter().filter(|l| l.slot.is_none()).count(), 1, "one memory loop");
        assert_eq!(lib.tracks[1].grid.len(), 2);
    }

    #[test]
    fn reads_nested_playlists_by_id_and_by_location() {
        let lib = read_library(RB_STYLE).unwrap();
        assert_eq!(lib.playlists.len(), 2);
        match &lib.playlists[0] {
            DjPlaylistNode::Folder { name, children } => {
                assert_eq!(name, "House");
                assert_eq!(
                    children[0],
                    DjPlaylistNode::Playlist { name: "Deep".into(), track_refs: vec![1, 0] }
                );
            }
            other => panic!("expected a folder, got {other:?}"),
        }
        assert_eq!(
            lib.playlists[1],
            DjPlaylistNode::Playlist { name: "ByPath".into(), track_refs: vec![1] }
        );
    }

    #[test]
    fn our_own_export_reads_back() {
        let xml = write_xml(&lib(), "0");
        let back = read_library(&xml).unwrap();
        assert_eq!(back.tracks.len(), lib().tracks.len());
        assert_eq!(back.playlist_count(), lib().playlist_count());
    }

    #[test]
    fn an_escaped_ampersand_is_decoded_once() {
        // `&#38;lt;` is a literal "&lt;" in the document, not a "<"
        assert_eq!(unescape("a &#38;lt; b"), "a &lt; b");
        assert_eq!(unescape("x &amp;amp; y"), "x &amp; y");
        assert_eq!(unescape("Caf&#233; &amp; &lt;Bar&gt; &quot;q&quot; &apos;"), "Caf\u{e9} & <Bar> \"q\" '");
        // unknown or broken references stay as written
        assert_eq!(unescape("A &nbsp; B &#zz; C & D"), "A &nbsp; B &#zz; C & D");
    }

    #[test]
    fn percent_decoding_never_panics_on_a_stray_percent() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("x%2"), "x%2");
        // a multi-byte character right after '%' used to slice mid-character
        assert_eq!(percent_decode("%a\u{e9}t"), "%a\u{e9}t");
        assert_eq!(percent_decode("ab%C3%A9"), "ab\u{e9}");
    }

    #[test]
    fn a_mac_location_keeps_its_path_for_matching_by_name() {
        let xml = r#"<DJ_PLAYLISTS><COLLECTION Entries="2">
<TRACK TrackID="1" Name="Mac" Size="10" Location="file://localhost/Users/dj/Music/Deep%20One.mp3"/>
<TRACK TrackID="2" Name="NoHost" Size="20" Location="file:///Volumes/USB/Contents/two.wav"/>
</COLLECTION><PLAYLISTS/></DJ_PLAYLISTS>"#;
        let lib = read_library(xml).unwrap();
        assert_eq!(lib.tracks[0].stick_rel_path, "");
        assert_eq!(lib.tracks[0].abs_path.file_name().unwrap(), "Deep One.mp3");
        assert_eq!(lib.tracks[1].abs_path.file_name().unwrap(), "two.wav");
    }
}
