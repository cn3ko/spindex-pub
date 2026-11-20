//! Parser and editor for Serato subcrate files (`_Serato_/Subcrates/*.crate`).
//!
//! Format: a flat sequence of chunks `[4-byte ASCII tag][u32 BE length][payload]`.
//! Track entries are `otrk` chunks whose payload nests further chunks; the
//! `ptrk` chunk inside holds the track path (UTF-16 BE, relative to the
//! stick root, forward slashes, e.g. "MP3/track.mp3").
//!
//! Editing rules (the safety contract): chunks we don't understand (version,
//! sort config, column layout, …) are preserved BYTE-FOR-BYTE — edits only
//! append or drop whole `otrk` chunks. Callers back up the original file
//! before the first write and write atomically. Audio files are never touched
//! through this module.

use std::collections::HashMap;

fn utf16be(bytes: &[u8]) -> Option<String> {
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

/// Extracts the track paths of one .crate file, in crate order. Unparseable
/// chunks are skipped — a truncated or exotic file yields fewer tracks, not
/// an error.
pub fn parse_crate(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 8 <= bytes.len() {
        let tag = &bytes[i..i + 4];
        let len = u32::from_be_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]])
            as usize;
        let end = (i + 8).saturating_add(len).min(bytes.len());
        let payload = &bytes[i + 8..end];
        if tag == b"otrk" {
            let mut j = 0;
            while j + 8 <= payload.len() {
                let tag2 = &payload[j..j + 4];
                let len2 = u32::from_be_bytes([
                    payload[j + 4],
                    payload[j + 5],
                    payload[j + 6],
                    payload[j + 7],
                ]) as usize;
                let end2 = (j + 8).saturating_add(len2).min(payload.len());
                if tag2 == b"ptrk" {
                    if let Some(path) = utf16be(&payload[j + 8..end2]) {
                        if !path.is_empty() {
                            out.push(path);
                        }
                    }
                }
                j = end2.max(j + 8);
            }
        }
        i = end.max(i + 8);
    }
    out
}

/// "Summer-Mix.crate" -> "Summer-Mix"; Serato encodes subcrate nesting in the
/// filename as "Parent%%Child.crate" -> "Parent / Child".
pub fn crate_display_name(file_name: &str) -> String {
    file_name
        .strip_suffix(".crate")
        .unwrap_or(file_name)
        .replace("%%", " / ")
}

/// One top-level chunk, payload kept verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub tag: [u8; 4],
    pub payload: Vec<u8>,
}

/// Splits a .crate file into its top-level chunks without interpreting them.
/// A trailing truncated chunk is dropped (same tolerance as `parse_crate`).
pub fn read_chunks(bytes: &[u8]) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 8 <= bytes.len() {
        let mut tag = [0u8; 4];
        tag.copy_from_slice(&bytes[i..i + 4]);
        let len = u32::from_be_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]])
            as usize;
        let end = (i + 8).saturating_add(len);
        if end > bytes.len() {
            break;
        }
        out.push(Chunk { tag, payload: bytes[i + 8..end].to_vec() });
        i = end;
    }
    out
}

pub fn write_chunks(chunks: &[Chunk]) -> Vec<u8> {
    let mut out = Vec::new();
    for c in chunks {
        out.extend_from_slice(&c.tag);
        out.extend_from_slice(&(c.payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&c.payload);
    }
    out
}

fn to_utf16be(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
}

/// The stick-relative path of an `otrk` chunk, if it has one.
pub fn otrk_path(chunk: &Chunk) -> Option<String> {
    if &chunk.tag != b"otrk" {
        return None;
    }
    let payload = &chunk.payload;
    let mut j = 0;
    while j + 8 <= payload.len() {
        let len = u32::from_be_bytes([
            payload[j + 4],
            payload[j + 5],
            payload[j + 6],
            payload[j + 7],
        ]) as usize;
        let end = (j + 8).saturating_add(len).min(payload.len());
        if &payload[j..j + 4] == b"ptrk" {
            return utf16be(&payload[j + 8..end]).filter(|p| !p.is_empty());
        }
        j = end.max(j + 8);
    }
    None
}

/// A fresh track entry referencing a stick-relative path.
pub fn make_otrk(path: &str) -> Chunk {
    let ptrk = to_utf16be(path);
    let mut payload = Vec::with_capacity(ptrk.len() + 8);
    payload.extend_from_slice(b"ptrk");
    payload.extend_from_slice(&(ptrk.len() as u32).to_be_bytes());
    payload.extend_from_slice(&ptrk);
    Chunk { tag: *b"otrk", payload }
}

fn sub_chunk(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(tag);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Content of a brand-new empty crate, mimicking byte-for-byte what Serato
/// DJ itself writes (verified against a Serato-authored crate): the version
/// header, a sort config, and the default column layout. Serato refuses the
/// contents of crates whose header it doesn't recognize — a bare `vrsn`
/// chunk with the wrong version string shows up as an empty crate and gets
/// deleted on Serato's next exit.
pub fn empty_crate() -> Vec<u8> {
    let mut chunks = vec![Chunk {
        tag: *b"vrsn",
        payload: to_utf16be("1.0/Serato ScratchLive Crate"),
    }];
    let mut osrt = sub_chunk(b"tvcn", &to_utf16be("key"));
    osrt.extend(sub_chunk(b"brev", &[0]));
    chunks.push(Chunk { tag: *b"osrt", payload: osrt });
    const COLUMNS: &[(&str, &str)] = &[
        ("filename", "425"),
        ("playCount", "0"),
        ("artist", "131"),
        ("bpm", "0"),
        ("bitrate", "0"),
        ("samplerate", "0"),
        ("length", "0"),
        ("key", "69"),
        ("added", "0"),
        ("song", "428"),
        ("album", "0"),
        ("comment", "0"),
        ("year", "0"),
    ];
    for (name, width) in COLUMNS {
        let mut p = sub_chunk(b"tvcn", &to_utf16be(name));
        p.extend(sub_chunk(b"tvcw", &to_utf16be(width)));
        chunks.push(Chunk { tag: *b"ovct", payload: p });
    }
    write_chunks(&chunks)
}

/// Appends track entries (paths already stick-relative, deduped against the
/// crate: an entry that is already present is not added twice).
pub fn add_tracks(chunks: &mut Vec<Chunk>, paths: &[String]) -> usize {
    add_tracks_at(chunks, paths, None)
}

/// Like `add_tracks`, but the new entries go in before chunk index `at`
/// (in the order given) instead of at the end; `None` or an index past the
/// last chunk appends.
pub fn add_tracks_at(chunks: &mut Vec<Chunk>, paths: &[String], at: Option<usize>) -> usize {
    let mut existing: Vec<String> = chunks
        .iter()
        .filter_map(otrk_path)
        .map(|p| p.to_lowercase())
        .collect();
    let mut new = Vec::new();
    for p in paths {
        let low = p.to_lowercase();
        if existing.contains(&low) {
            continue;
        }
        existing.push(low);
        new.push(make_otrk(p));
    }
    let added = new.len();
    let at = at.unwrap_or(chunks.len()).min(chunks.len());
    chunks.splice(at..at, new);
    added
}

/// Drops the track entries whose path matches one of `paths`
/// (case-insensitive — FAT/NTFS sticks don't distinguish case). All other
/// chunks stay untouched. Returns the number of removed entries.
pub fn remove_tracks(chunks: &mut Vec<Chunk>, paths: &[String]) -> usize {
    let victims: Vec<String> = paths.iter().map(|p| p.to_lowercase()).collect();
    let before = chunks.len();
    chunks.retain(|c| match otrk_path(c) {
        Some(p) => !victims.contains(&p.to_lowercase()),
        None => true,
    });
    before - chunks.len()
}

/// Replaces every `otrk` whose path equals `old` (case-insensitive) with an
/// entry for `new`. Used for cross-extension replaces (e.g. an mp3 enhanced
/// in place to flac): the crate keeps pointing at the same logical track,
/// now under its new filename. Returns the number of replacements.
pub fn replace_track_path(chunks: &mut Vec<Chunk>, old: &str, new: &str) -> usize {
    let old_lower = old.to_lowercase();
    let mut replaced = 0;
    for c in chunks.iter_mut() {
        if otrk_path(c).map(|p| p.to_lowercase()) == Some(old_lower.clone()) {
            *c = make_otrk(new);
            replaced += 1;
        }
    }
    replaced
}

/// Converts a library-relative path (as stored in `track.rel_path`, forward
/// slashes) into the stick-relative form Serato's `.crate` `ptrk` entries use
/// — rooted at the DRIVE, including the library's own folder segment (e.g.
/// library root `D:\Library MP3`, rel `track.mp3` -> `Library MP3/track.mp3`;
/// a root-of-the-stick library with no folder segment yields just the rel
/// path unchanged). `library_root` is the absolute library root as stored in
/// `library.root_path`. Returns `None` when the root isn't drive-letter-rooted
/// (no `X:` prefix).
///
/// Tolerates the root with or without a trailing separator (device-folder
/// libraries are always created with one — see `add_device_libraries` — but
/// this stays robust either way rather than depending on that convention).
pub fn stick_relative_path(library_root: &str, rel_path: &str) -> Option<String> {
    let letter = library_root
        .get(0..2)
        .filter(|s| s.as_bytes().get(1) == Some(&b':'))?;
    let drive_root = format!("{letter}\\");
    let folder = library_root.strip_prefix(&drive_root)?.replace('\\', "/");
    let folder = folder.trim_end_matches('/');
    if folder.is_empty() {
        Some(rel_path.to_string())
    } else {
        Some(format!("{folder}/{rel_path}"))
    }
}

/// Permutes the resolvable `otrk` entries of a crate into `new_order`.
///
/// `resolved` runs parallel to `chunks`: `Some(track_id)` marks an `otrk` this
/// SpinDex database can identify, `None` marks everything else — header chunks
/// and, importantly, `otrk` links whose file we cannot resolve (renamed, moved,
/// not indexed). Those dangling links are invisible in the UI, so they keep the
/// exact index they already occupy and the visible tracks reorder around them;
/// nothing the user never saw is silently moved.
///
/// Ids in `new_order` that match no entry are ignored, and resolvable entries
/// the caller did not mention keep their relative order after the ones it did —
/// so a stale id list can never drop a link from the crate.
pub fn reorder_otrk(chunks: &[Chunk], resolved: &[Option<i64>], new_order: &[i64]) -> Vec<Chunk> {
    // the slots that participate, in file order
    let slots: Vec<usize> = (0..chunks.len())
        .filter(|i| resolved.get(*i).copied().flatten().is_some())
        .collect();
    // track id -> its slot positions, descending so pop() yields the earliest
    // remaining one: duplicate links to one track are consumed in file order
    let mut by_id: HashMap<i64, Vec<usize>> = HashMap::new();
    for (pos, &i) in slots.iter().enumerate() {
        if let Some(Some(id)) = resolved.get(i).copied() {
            by_id.entry(id).or_default().push(pos);
        }
    }
    for queue in by_id.values_mut() {
        queue.reverse();
    }
    let mut taken = vec![false; slots.len()];
    let mut seq: Vec<usize> = Vec::with_capacity(slots.len());
    for id in new_order {
        if let Some(pos) = by_id.get_mut(id).and_then(|q| q.pop()) {
            taken[pos] = true;
            seq.push(pos);
        }
    }
    for (pos, done) in taken.iter().enumerate() {
        if !done {
            seq.push(pos);
        }
    }
    let mut out = chunks.to_vec();
    for (&slot, &pos) in slots.iter().zip(seq.iter()) {
        out[slot] = chunks[slots[pos]].clone();
    }
    out
}

/// The byte length `write_chunks` would produce: tag + u32 length + payload.
fn encoded_len(chunks: &[Chunk]) -> usize {
    chunks.iter().map(|c| 8 + c.payload.len()).sum()
}

/// Guard for the reorder write path: a reorder is a pure permutation, so the
/// output must hold exactly the same chunks — same count, same total bytes —
/// as the input, and re-encode to exactly the source file's length.
///
/// The last part is the one that matters in practice: `read_chunks` stops at
/// the first chunk whose declared length runs past the buffer, so a crate that
/// was truncated (stick yanked mid-write) or corrupted parses to FEWER chunks
/// than the file holds. `parse_crate`, which the viewer uses, clamps instead
/// and keeps going — so the table can show entries the writer never saw, and
/// writing the parsed chunks back would delete them permanently. Comparing the
/// re-encoded length against the source length turns that silent loss into an
/// error the user sees.
pub fn permutation_is_intact(before: &[Chunk], after: &[Chunk], src_len: usize) -> bool {
    before.len() == after.len()
        && encoded_len(before) == encoded_len(after)
        && encoded_len(after) == src_len
}

/// Whether `read_chunks` reads the whole file. It stops at the first chunk
/// that runs past the end, so writing a truncated crate back would drop its
/// tail links; any edit that only adds links must refuse such a crate.
pub fn reads_to_the_end(bytes: &[u8]) -> bool {
    encoded_len(&read_chunks(bytes)) == bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_truncated_crate_does_not_read_to_the_end() {
        let whole = write_chunks(&[make_otrk("A/one.mp3"), make_otrk("A/two.mp3")]);
        assert!(reads_to_the_end(&whole));
        // a stick yanked mid-write: the last entry is cut short
        assert!(!reads_to_the_end(&whole[..whole.len() - 3]));
    }

    fn chunk(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(tag);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn utf16be_bytes(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
    }

    #[test]
    fn parses_track_paths_and_skips_other_chunks() {
        let mut file = chunk(b"vrsn", &utf16be_bytes("81.0/Serato ScratchLive Crate"));
        let otrk1 = chunk(b"ptrk", &utf16be_bytes("MP3/ZALE\u{d8}N - Track.mp3"));
        file.extend(chunk(b"otrk", &otrk1));
        file.extend(chunk(b"osrt", &utf16be_bytes("song"))); // sort config, ignored
        let otrk2 = chunk(b"ptrk", &utf16be_bytes("Library WAV/b.wav"));
        file.extend(chunk(b"otrk", &otrk2));

        assert_eq!(
            parse_crate(&file),
            vec!["MP3/ZALE\u{d8}N - Track.mp3".to_string(), "Library WAV/b.wav".to_string()]
        );
    }

    #[test]
    fn truncated_file_degrades_to_fewer_tracks() {
        let otrk = chunk(b"otrk", &chunk(b"ptrk", &utf16be_bytes("MP3/a.mp3")));
        let mut file = otrk.clone();
        file.extend_from_slice(&otrk[..otrk.len() / 2]); // second entry cut off
        assert_eq!(parse_crate(&file), vec!["MP3/a.mp3".to_string()]);
        assert!(parse_crate(&[0x00, 0x01]).is_empty(), "garbage yields nothing");
    }

    #[test]
    fn display_names() {
        assert_eq!(crate_display_name("Summer-Mix.crate"), "Summer-Mix");
        assert_eq!(crate_display_name("House%%Deep.crate"), "House / Deep");
    }

    #[test]
    fn chunks_round_trip_byte_for_byte() {
        let mut file = chunk(b"vrsn", &utf16be_bytes("81.0/Serato ScratchLive Crate"));
        file.extend(chunk(b"osrt", &utf16be_bytes("song"))); // opaque, must survive
        file.extend(chunk(b"ovct", &[1, 2, 3, 4, 0xFF])); // binary, must survive
        file.extend(chunk(b"otrk", &chunk(b"ptrk", &utf16be_bytes("MP3/a.mp3"))));

        let chunks = read_chunks(&file);
        assert_eq!(write_chunks(&chunks), file, "parse+write is the identity");
    }

    #[test]
    fn add_tracks_at_inserts_before_the_given_chunk_in_drag_order() {
        let mut file = chunk(b"vrsn", &utf16be_bytes("1.0/Serato ScratchLive Crate"));
        file.extend(chunk(b"otrk", &chunk(b"ptrk", &utf16be_bytes("MP3/a.mp3"))));
        file.extend(chunk(b"otrk", &chunk(b"ptrk", &utf16be_bytes("MP3/b.mp3"))));
        let mut chunks = read_chunks(&file);
        // chunk 2 is b.mp3: the new links land between a and b, in drag order,
        // and a link already in the crate is not added twice
        let added = add_tracks_at(
            &mut chunks,
            &["MP3/y.mp3".to_string(), "MP3/x.mp3".to_string(), "mp3/A.mp3".to_string()],
            Some(2),
        );
        assert_eq!(added, 2);
        let paths: Vec<String> = chunks.iter().filter_map(otrk_path).collect();
        assert_eq!(paths, vec!["MP3/a.mp3", "MP3/y.mp3", "MP3/x.mp3", "MP3/b.mp3"]);
        assert_eq!(&chunks[0].tag, b"vrsn");
        // past the end (or None) appends
        add_tracks_at(&mut chunks, &["MP3/z.mp3".to_string()], Some(99));
        assert_eq!(chunks.iter().filter_map(otrk_path).last().as_deref(), Some("MP3/z.mp3"));
    }

    #[test]
    fn add_and_remove_edit_only_otrk_chunks() {
        let mut file = chunk(b"vrsn", &utf16be_bytes("81.0/Serato ScratchLive Crate"));
        file.extend(chunk(b"otrk", &chunk(b"ptrk", &utf16be_bytes("MP3/a.mp3"))));
        let mut chunks = read_chunks(&file);

        // add: appended + deduped (case-insensitive)
        let added = add_tracks(
            &mut chunks,
            &["MP3/b.mp3".to_string(), "mp3/A.MP3".to_string()],
        );
        assert_eq!(added, 1, "existing entry not duplicated");
        let paths: Vec<String> = chunks.iter().filter_map(otrk_path).collect();
        assert_eq!(paths, vec!["MP3/a.mp3", "MP3/b.mp3"]);

        // remove: only the matching otrk goes; vrsn survives verbatim
        let removed = remove_tracks(&mut chunks, &["mp3/a.mp3".to_string()]);
        assert_eq!(removed, 1);
        let paths: Vec<String> = chunks.iter().filter_map(otrk_path).collect();
        assert_eq!(paths, vec!["MP3/b.mp3"]);
        assert_eq!(&chunks[0].tag, b"vrsn");

        // a new otrk we wrote parses back through the reader
        assert_eq!(
            parse_crate(&write_chunks(&chunks)),
            vec!["MP3/b.mp3".to_string()]
        );
    }

    #[test]
    fn replace_track_path_swaps_matching_otrks() {
        let mut chunks = Vec::new();
        chunks.push(make_otrk("Library MP3\\a.mp3"));
        chunks.push(make_otrk("Library MP3\\b.mp3"));
        let n = replace_track_path(&mut chunks, "library mp3\\A.MP3", "Library MP3\\a.flac");
        assert_eq!(n, 1, "case-insensitive match");
        let paths: Vec<String> = chunks.iter().filter_map(otrk_path).collect();
        assert_eq!(paths, vec!["Library MP3\\a.flac", "Library MP3\\b.mp3"]);
        // round-trips through the writer
        let bytes = write_chunks(&chunks);
        let reread = read_chunks(&bytes);
        assert_eq!(reread.iter().filter_map(otrk_path).count(), 2);
    }

    #[test]
    fn stick_relative_path_prefixes_the_folder_segment() {
        // brief's example: no trailing separator on the root.
        assert_eq!(
            stick_relative_path("D:\\Library MP3", "track.mp3").as_deref(),
            Some("Library MP3/track.mp3")
        );
        // device-folder libraries are actually stored WITH a trailing
        // separator (see add_device_libraries) — same result either way.
        assert_eq!(
            stick_relative_path("D:\\Library MP3\\", "track.mp3").as_deref(),
            Some("Library MP3/track.mp3")
        );
        // nested rel_path segments pass through unchanged.
        assert_eq!(
            stick_relative_path("D:\\Library MP3\\", "House/track.mp3").as_deref(),
            Some("Library MP3/House/track.mp3")
        );
        // a root-of-the-stick library (no folder segment) yields the rel
        // path unchanged.
        assert_eq!(
            stick_relative_path("D:\\", "track.mp3").as_deref(),
            Some("track.mp3")
        );
        // not drive-letter-rooted -> None
        assert_eq!(stick_relative_path("\\\\server\\share", "track.mp3"), None);
    }

    #[test]
    fn empty_crate_matches_serato_header_exactly() {
        let bytes = empty_crate();
        assert!(parse_crate(&bytes).is_empty());
        let chunks = read_chunks(&bytes);
        assert_eq!(&chunks[0].tag, b"vrsn");
        assert_eq!(
            utf16be(&chunks[0].payload).as_deref(),
            Some("1.0/Serato ScratchLive Crate"),
            "the version string Serato requires — '81.0' was a mis-read length byte"
        );
        assert_eq!(&chunks[1].tag, b"osrt");
        assert_eq!(chunks.iter().filter(|c| &c.tag == b"ovct").count(), 13);
        // byte length verified against a crate authored by Serato DJ itself
        assert_eq!(bytes.len(), 601);
    }

    /// A chunk whose tag is otrk and whose payload is just a marker byte, so
    /// tests can identify which entry moved without building real ptrk data.
    fn otrk(marker: u8) -> Chunk {
        Chunk { tag: *b"otrk", payload: vec![marker] }
    }

    fn other(tag: &[u8; 4]) -> Chunk {
        Chunk { tag: *tag, payload: vec![0xAA] }
    }

    fn markers(chunks: &[Chunk]) -> Vec<u8> {
        chunks.iter().map(|c| c.payload[0]).collect()
    }

    #[test]
    fn reorder_moves_a_single_track() {
        let chunks = vec![otrk(1), otrk(2), otrk(3)];
        let resolved = vec![Some(10), Some(20), Some(30)];
        let out = reorder_otrk(&chunks, &resolved, &[30, 10, 20]);
        assert_eq!(markers(&out), vec![3, 1, 2]);
    }

    #[test]
    fn reorder_keeps_non_otrk_chunks_in_place() {
        // a real crate starts with vrsn/osrt/ovct headers
        let chunks = vec![other(b"vrsn"), other(b"osrt"), otrk(1), otrk(2)];
        let resolved = vec![None, None, Some(10), Some(20)];
        let out = reorder_otrk(&chunks, &resolved, &[20, 10]);
        assert_eq!(out[0].tag, *b"vrsn");
        assert_eq!(out[1].tag, *b"osrt");
        assert_eq!(markers(&out[2..]), vec![2, 1]);
    }

    #[test]
    fn dangling_entry_between_moved_tracks_keeps_its_index() {
        // index 1 is an otrk SpinDex cannot resolve: it must not move
        let chunks = vec![otrk(1), otrk(9), otrk(2), otrk(3)];
        let resolved = vec![Some(10), None, Some(20), Some(30)];
        let out = reorder_otrk(&chunks, &resolved, &[30, 20, 10]);
        assert_eq!(markers(&out), vec![3, 9, 2, 1]);
    }

    #[test]
    fn dangling_entries_at_head_and_tail_keep_their_indices() {
        let chunks = vec![otrk(9), otrk(1), otrk(2), otrk(8)];
        let resolved = vec![None, Some(10), Some(20), None];
        let out = reorder_otrk(&chunks, &resolved, &[20, 10]);
        assert_eq!(markers(&out), vec![9, 2, 1, 8]);
    }

    #[test]
    fn unknown_id_in_new_order_is_ignored() {
        let chunks = vec![otrk(1), otrk(2)];
        let resolved = vec![Some(10), Some(20)];
        let out = reorder_otrk(&chunks, &resolved, &[999, 20, 10]);
        assert_eq!(markers(&out), vec![2, 1]);
    }

    #[test]
    fn entries_missing_from_new_order_keep_their_relative_order() {
        let chunks = vec![otrk(1), otrk(2), otrk(3), otrk(4)];
        let resolved = vec![Some(10), Some(20), Some(30), Some(40)];
        // only 40 is named: it leads, the rest follow in their original order
        let out = reorder_otrk(&chunks, &resolved, &[40]);
        assert_eq!(markers(&out), vec![4, 1, 2, 3]);
    }

    #[test]
    fn duplicate_links_to_one_track_are_consumed_in_file_order() {
        // two entries pointing at the same track row
        let chunks = vec![otrk(1), otrk(2), otrk(3)];
        let resolved = vec![Some(10), Some(10), Some(30)];
        let out = reorder_otrk(&chunks, &resolved, &[30, 10, 10]);
        assert_eq!(markers(&out), vec![3, 1, 2]);
    }

    #[test]
    fn permutation_guard_accepts_a_real_permutation() {
        let chunks = vec![other(b"vrsn"), otrk(1), otrk(2), otrk(3)];
        let src_len = write_chunks(&chunks).len();
        let resolved = vec![None, Some(10), Some(20), Some(30)];
        let out = reorder_otrk(&chunks, &resolved, &[30, 10, 20]);
        assert!(permutation_is_intact(&chunks, &out, src_len));
    }

    #[test]
    fn permutation_guard_rejects_a_dropped_chunk() {
        let chunks = vec![other(b"vrsn"), otrk(1), otrk(2), otrk(3)];
        let src_len = write_chunks(&chunks).len();
        let mut out = chunks.clone();
        out.pop(); // the tail link would be deleted by this write
        assert!(!permutation_is_intact(&chunks, &out, src_len));
    }

    #[test]
    fn permutation_guard_rejects_a_truncated_source() {
        // a crate whose last chunk declares more bytes than the file holds:
        // read_chunks drops it, so the parsed chunks re-encode shorter than
        // the file and writing them back would lose that entry for good.
        let mut bytes = write_chunks(&[other(b"vrsn"), otrk(1)]);
        bytes.extend_from_slice(b"otrk");
        bytes.extend_from_slice(&99u32.to_be_bytes());
        bytes.extend_from_slice(b"cut");
        let chunks = read_chunks(&bytes);
        assert_eq!(chunks.len(), 2, "the truncated tail chunk is dropped");
        // even an identity "reorder" must be refused on such a file
        assert!(!permutation_is_intact(&chunks, &chunks, bytes.len()));
    }

    #[test]
    fn reorder_round_trips_through_write_and_read() {
        let chunks = vec![other(b"vrsn"), otrk(1), otrk(2)];
        let resolved = vec![None, Some(10), Some(20)];
        let out = reorder_otrk(&chunks, &resolved, &[20, 10]);
        let bytes = write_chunks(&out);
        assert_eq!(read_chunks(&bytes), out, "reordered chunks survive a write/read cycle");
    }
}
