// src-tauri/tests/rekordbox_track_survey.rs
// Diagnostic only — Task 5's byte-level survey of the tracks table row
// layout (where the id and file-path fields live) and the playlist_entries
// row, against the real, now-populated fixture (commit 8235eb7). Not a
// foundation for the shipped parser by itself; findings are pinned as
// TRACK_ROW_ID_OFFSET and the longest-match path scan in core/rekordbox.rs.
// Run with:
//   cd src-tauri && cargo test --test rekordbox_track_survey -- --ignored --nocapture

use spindex::core::rekordbox::{parse_header, TABLE_TYPE_PLAYLIST_ENTRIES, TABLE_TYPE_TRACKS};
use std::fs;

const GROUP_STRIDE: usize = 0x24;
const MAX_ROWS_PER_GROUP: usize = 16;
const PAGE_HEADER_LEN: usize = 40;

/// Same row-group walking as core::rekordbox::walk_page_rows, reimplemented
/// here (not calling the private fn) so this diagnostic stays independent —
/// if it and the real parser ever disagree, that disagreement is the signal.
fn row_offsets(page: &[u8], num_rows: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut accounted = 0usize;
    let mut g = 0usize;
    while accounted < num_rows {
        let base = page.len() - g * GROUP_STRIDE;
        let present_mask = u16::from_le_bytes(page[base - 4..base - 2].try_into().unwrap());
        let slots_mask = u16::from_le_bytes(page[base - 2..base].try_into().unwrap());
        let n_slots = if slots_mask == 0 {
            0
        } else {
            (16 - slots_mask.leading_zeros() as usize).min(MAX_ROWS_PER_GROUP)
        };
        for i in 0..n_slots {
            if (present_mask >> i) & 1 == 0 {
                continue;
            }
            let off = base - 6 - 2 * i;
            let rel = u16::from_le_bytes(page[off..off + 2].try_into().unwrap()) as usize;
            out.push(PAGE_HEADER_LEN + rel);
        }
        accounted += n_slots;
        g += 1;
        if g > 8 {
            break;
        }
    }
    out.sort_unstable();
    out
}

fn hexdump(bytes: &[u8], base: usize) {
    for (i, chunk) in bytes.chunks(16).enumerate() {
        let addr = base + i * 16;
        let hex: Vec<String> = chunk.iter().map(|b| format!("{:02x}", b)).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' })
            .collect();
        println!("{addr:6}: {}  {}", hex.join(" "), ascii);
    }
}

#[test]
#[ignore]
fn survey_tracks_and_playlist_entries_pages() {
    let bytes = fs::read("tests/fixtures/rekordbox/export.pdb").expect("fixture missing");
    let header = parse_header(&bytes).unwrap();
    let page_len = header.page_len as usize;

    let tracks_table = header.tables.iter().find(|t| t.table_type == TABLE_TYPE_TRACKS).unwrap();
    let entries_table = header
        .tables
        .iter()
        .find(|t| t.table_type == TABLE_TYPE_PLAYLIST_ENTRIES)
        .unwrap();
    println!("tracks table: first_page={} last_page={}", tracks_table.first_page, tracks_table.last_page);
    println!(
        "playlist_entries table: first_page={} last_page={}",
        entries_table.first_page, entries_table.last_page
    );

    for (label, table) in [("tracks", tracks_table), ("playlist_entries", entries_table)] {
        let mut page_index = table.first_page;
        loop {
            let off = page_index as usize * page_len;
            let page = &bytes[off..off + page_len];
            let num_rows_small = page[24];
            let page_flags = page[27];
            let num_rows_large = u16::from_le_bytes(page[32..34].try_into().unwrap());
            let next_page = u32::from_le_bytes(page[12..16].try_into().unwrap());
            println!(
                "{label} page {page_index}: num_rows_small={num_rows_small} page_flags=0x{page_flags:02x} num_rows_large=0x{num_rows_large:04x} next_page={next_page}"
            );
            if page_index >= table.last_page {
                break;
            }
            page_index = next_page;
        }
    }
}

#[test]
#[ignore]
fn dump_track_row_and_masks() {
    let bytes = fs::read("tests/fixtures/rekordbox/export.pdb").expect("fixture missing");
    let page_len = 4096usize;
    let page_index = 2usize;
    let off = page_index * page_len;
    let page = &bytes[off..off + page_len];

    // Row-group masks: confirms 2 slots allocated, only 1 present (a
    // tombstone), same pattern as playlist_tree's page 16 (Task 4).
    let base = page.len();
    let present_mask = u16::from_le_bytes(page[base - 4..base - 2].try_into().unwrap());
    let slots_mask = u16::from_le_bytes(page[base - 2..base].try_into().unwrap());
    println!("g=0 present_mask=0x{present_mask:04x} slots_mask=0x{slots_mask:04x}");

    let offsets = row_offsets(page, 2);
    println!("track row offsets (page-relative): {offsets:?}");
    let row_off = offsets[0];
    println!("=== live track row at page-relative offset {row_off} ===");
    let end = (row_off + 900).min(page.len());
    hexdump(&page[row_off..end], row_off);
}

#[test]
#[ignore]
fn dump_playlist_entries_row() {
    let bytes = fs::read("tests/fixtures/rekordbox/export.pdb").expect("fixture missing");
    let page_len = 4096usize;
    let page_index = 18usize;
    let off = page_index * page_len;
    let page = &bytes[off..off + page_len];
    let offsets = row_offsets(page, 1);
    println!("playlist_entries row offsets: {offsets:?}");
    let row_off = offsets[0];
    let end = (row_off + 64).min(page.len());
    hexdump(&page[row_off..end], row_off);
}

#[test]
#[ignore]
fn verify_id_offset_and_numeric_fields() {
    let bytes = fs::read("tests/fixtures/rekordbox/export.pdb").expect("fixture missing");
    let header = parse_header(&bytes).unwrap();
    let page_len = header.page_len as usize;
    let page_index = 2usize;
    let off = page_index * page_len;
    let page = &bytes[off..off + page_len];
    let offsets = row_offsets(page, 2);
    let row = &page[offsets[0]..];

    let id = u32::from_le_bytes(row[72..76].try_into().unwrap());
    let sample_rate = u32::from_le_bytes(row[8..12].try_into().unwrap());
    let bitrate = u32::from_le_bytes(row[48..52].try_into().unwrap());
    let tempo = u32::from_le_bytes(row[56..60].try_into().unwrap());
    let year = u16::from_le_bytes(row[80..82].try_into().unwrap());
    let sample_depth = u16::from_le_bytes(row[82..84].try_into().unwrap());
    let duration = u16::from_le_bytes(row[84..86].try_into().unwrap());
    println!(
        "id={id} sample_rate={sample_rate} bitrate={bitrate} tempo={tempo} year={year} sample_depth={sample_depth} duration={duration}"
    );
}

// ---------------------------------------------------------------------------
// Recorded findings (2026-08-18), Task 5 of the SP1 plan.
//
// `survey_tracks_and_playlist_entries_pages`: tracks table spans pages 1-2
// (page 1 all-filler/template, page 2 real, num_rows_small=2); playlist_
// entries spans pages 17-18 (page 17 template, page 18 real, num_rows_small
// =1). Same "single real page after a template page" shape Task 4 found
// for playlist_tree/colors/columns.
//
// `dump_track_row_and_masks`: page 2's row group has slots_mask=0x0003 (2
// slots) but present_mask=0x0002 (only slot 1 live) — slot 0 is a
// tombstone, same pattern as playlist_tree's Task 4 finding. The one live
// track row sits at page-relative offset 504 (file offset 8696).
//
// Manual byte-by-byte decode of that row (cross-checked against
// `verify_id_offset_and_numeric_fields`'s output: id=1 sample_rate=44100
// bitrate=1411 tempo=13100 year=2025 sample_depth=16 duration=206):
//   - offset  8 (u32): sample_rate = 44100 Hz — a real, exact AIFF rate.
//   - offset 48 (u32): bitrate = 1411 kbps = 44100 * 16 * 2 / 1000 exactly
//     — the textbook CD-quality PCM bitrate for THIS file's own
//     sample_rate/sample_depth, not a coincidental round number.
//   - offset 56 (u32): tempo = 13100 -> 131.00 BPM (DeviceSQL stores tempo
//     as BPM * 100) — plausible for a dance track.
//   - offset 80 (u16): year = 2025; offset 82 (u16): sample_depth = 16;
//     offset 84 (u16): duration = 206 seconds (3:26) — all independently
//     plausible values landing exactly where a recalled DeviceSQL
//     track_row field ordering (unknown1, index_shift, bitmask,
//     sample_rate, composer_id, file_size, unknown2, unknown3, unknown4,
//     artwork_id, key_id, orig_artist_id, label_id, remixer_id, bitrate,
//     track_number, tempo, genre_id, album_id, artist_id, id, disc_number,
//     play_count, year, sample_depth, duration, ...) predicts them, each
//     spaced correctly (4 bytes per u32, 2 per u16) from the last.
//   - offset 72 (u32): id = 1, matching playlist_entries' one row's
//     track_id = 1.
// Five independently-plausible values landing at exactly the predicted
// offsets is strong corroboration, not coincidence — hence
// TRACK_ROW_ID_OFFSET = 72 in core/rekordbox.rs. CAVEAT: because this
// fixture has exactly one live track, several neighbouring FK fields
// (artwork_id, key_id, label_id, track_number, genre_id, album_id,
// artist_id) also happen to read 1 (first/only row in each linked table
// too), so id's VALUE alone could not disambiguate its offset from those
// neighbours — the offset was pinned by the field-spacing pattern plus the
// four unrelated (non-id) value matches, not by "reads 1" alone. A second,
// differently-valued track in a future fixture would let this be checked
// even more directly.
//
// The row's string-offset table (a run of monotonically increasing u16
// values found around row-relative offset 80-140) matches the tag-byte
// positions of the row's DeviceSQL strings exactly where checked (e.g.
// table value 271 = row-relative offset of the title string's tag byte,
// 350 = the full-path string's tag byte) — confirming the row DOES use an
// offset table for strings, but it was NOT fully decoded (some early
// entries don't fit a simple "offset into this row" reading, and the last
// entry looks like a sentinel/terminator, not a real offset). Not needed
// for Task 5: `find_path_string_in_track_row`'s longest-match scan finds
// the same field without decoding the table, and was verified against
// this exact row.
//
// The one live track row contains TWO distinct DeviceSQL strings ending in
// ".aiff": a bare filename ("...(Extended).aiff", 52 chars) and a full
// stick-relative path with leading slash and folder segment
// ("/Library AIFF/Tinashe, Disco Lines - No Broke Boys (Extended).aiff",
// 66 chars) — the filename is a literal suffix of the path, confirming
// which is which. `find_path_string_in_track_row` picks the LONGEST
// extension-suffixed match specifically to get the full path, not the
// bare filename, without needing to decode the offset table.
//
// `dump_playlist_entries_row`: the fixture's one playlist_entries row
// decodes as three identical u32 fields, all = 1 (entry_index, track_id,
// playlist_id — all coincide since there is exactly one of each). Field
// ORDER could not be disambiguated by value from this fixture alone; the
// order used in core/rekordbox.rs (entry_index, track_id, playlist_id)
// matches the publicly documented DeviceSQL playlist_entries row shape.
// ---------------------------------------------------------------------------
