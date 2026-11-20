// Read-only Rekordbox export.pdb parsing (SP1: read-only USB crate viewer).
//
// This module parses the DeviceSQL PDB header, the table-pointer array, and
// (as of Task 4) the playlist_tree table's row-group page layout, which is
// shared machinery every later table (playlist_entries, tracks) will reuse.

use anyhow::{anyhow, bail, Context, Result};

pub const TABLE_TYPE_TRACKS: u32 = 0;
pub const TABLE_TYPE_PLAYLIST_TREE: u32 = 7;
pub const TABLE_TYPE_PLAYLIST_ENTRIES: u32 = 8;

#[derive(Debug, Clone)]
pub struct TablePointer {
    pub table_type: u32,
    pub first_page: u32,
    pub last_page: u32,
}

#[derive(Debug, Clone)]
pub struct PdbHeader {
    pub page_len: u32,
    pub num_tables: u32,
    pub tables: Vec<TablePointer>,
}

fn u32le(bytes: &[u8], off: usize) -> Result<u32> {
    let slice = bytes
        .get(off..off + 4)
        .ok_or_else(|| anyhow!("truncated file: offset {off} out of range"))?;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

pub fn parse_header(bytes: &[u8]) -> Result<PdbHeader> {
    if bytes.len() < 28 {
        bail!("file too short to contain a PDB header");
    }
    let page_len = u32le(bytes, 4).context("reading page_len")?;
    let num_tables = u32le(bytes, 8).context("reading num_tables")?;
    if num_tables == 0 || num_tables > 64 {
        bail!("implausible num_tables: {num_tables}");
    }

    let mut tables = Vec::with_capacity(num_tables as usize);
    for i in 0..num_tables {
        let base = 28 + (i as usize) * 16;
        let table_type = u32le(bytes, base).context("reading table_type")?;
        let first_page = u32le(bytes, base + 8).context("reading first_page")?;
        let last_page = u32le(bytes, base + 12).context("reading last_page")?;
        tables.push(TablePointer {
            table_type,
            first_page,
            last_page,
        });
    }

    Ok(PdbHeader {
        page_len,
        num_tables,
        tables,
    })
}

/// Fixed size, in bytes, of every content page's header in this format.
/// Confirmed identical across every page inspected in the real fixture
/// (tracks/colors/playlist_tree/playlist_entries pages all share this exact
/// 40-byte header shape — gap, page_index, table_type, next_page, two more
/// u4 unknowns, then num_rows_small at byte 24, page_flags at byte 27,
/// free_size/used_size/num_rows_large/two more u2 unknowns filling the rest).
const PAGE_HEADER_LEN: usize = 40;

/// Byte offset of the `page_flags` field within a page header. Bit `0x40`
/// marks a page as a non-data ("template"/free) page — confirmed against
/// the fixture's "empty template" pages (e.g. playlist_tree's own first
/// page, page 15): both of the fixture's known template pages have this
/// bit set (`page_flags = 0x64`), while every real data page checked
/// (colors, playlist_tree's second page, playlist_entries, columns) does
/// not. This is the RELIABLE non-data-page signal — `num_rows_large`'s
/// documented sentinel (`0x1fff`) is NOT reliable for this: the fixture's
/// two template pages disagree with each other on that field (one reads
/// `0x0001`, the other `0x1fff`), so it cannot be trusted to mean
/// "unused" on its own.
const PAGE_FLAGS_OFFSET: usize = 27;
const PAGE_FLAGS_NON_DATA: u8 = 0x40;

/// Byte offset of `num_rows_small` (u8) and `num_rows_large` (u16) within a
/// page header. `num_rows_small` overflows for pages with 256+ rows; when
/// that happens the real count lives in `num_rows_large` instead. On pages
/// with fewer rows, `num_rows_large` sometimes holds a `0x1fff` sentinel
/// meaning "not applicable, trust num_rows_small" — but as noted above,
/// that sentinel is not reliably present on every non-data page, so
/// `page_flags` is checked FIRST and independently.
///
/// `num_rows_large` lives at 0x22 (34), NOT 0x20 (32). This code read 32
/// until a whole-branch review caught it, and the mistake was invisible on
/// the fixture because offset 32 happens to MIRROR `num_rows_small` on
/// every page there (survey: pages 2/16/40 read 2/2/1 at both 24 and 32,
/// page 34 reads 27 at both). Offset 34 is the real field: it reads 0 on
/// every data page in the fixture and the 0x1fff sentinel on every
/// template page — which is what the field is supposed to look like when
/// a page has far fewer than 256 rows.
const NUM_ROWS_SMALL_OFFSET: usize = 24;
const NUM_ROWS_LARGE_OFFSET: usize = 34;
const NUM_ROWS_LARGE_SENTINEL: u16 = 0x1fff;

/// Row-group layout constants, verified against the real fixture in two
/// independent ways: the 2-row playlist_tree table (a single group) and,
/// critically, the 27-row multi-group `columns` system table (page 34,
/// table_type=16) — see `tests/rekordbox_group_survey.rs`. A page's rows
/// are stored in groups of up to `MAX_ROWS_PER_GROUP`, addressed
/// DESCENDING from the page end: group `g`'s base is
/// `page_len - g * GROUP_STRIDE_BYTES` (g = 0 is the group closest to the
/// page end). Within a group's base:
///   - `base - 2`: "slots" bitmask, `(1 << n) - 1` for `n` slots allocated
///     to this group (used to learn how many of the group's up-to-16 row
///     offsets are meaningful, NOT how many rows are actually present).
///   - `base - 4`: "present" bitmask — which of those `n` slots hold a
///     LIVE row. A clear bit means a deleted/tombstoned row: verified on
///     playlist_tree's own page 16, where slots-mask 0x0003 (2 allocated)
///     disagrees with present-mask 0x0002 (only slot 1 live) — slot 0
///     ("Untitled Playlist") is a tombstone of the same playlist before it
///     was renamed to "summer-hits" (slot 1), not a second live playlist.
///   - `base - 6 - 2*i` for `i` in `0..n`: the row's offset, RELATIVE to
///     the end of the `PAGE_HEADER_LEN`-byte page header, i.e.
///     `row_start = PAGE_HEADER_LEN + relative_value`.
const GROUP_STRIDE_BYTES: usize = 0x24;
const MAX_ROWS_PER_GROUP: usize = 16;

fn page_offset(header: &PdbHeader, page_index: u32) -> Result<usize> {
    (page_index as usize)
        .checked_mul(header.page_len as usize)
        .ok_or_else(|| anyhow!("page offset overflow: page {page_index} * page_len {}", header.page_len))
}

/// Reads the real row count for a page: 0 for a non-data page
/// (`page_flags & PAGE_FLAGS_NON_DATA` set), else `num_rows_large` only
/// when it is BOTH not the `0x1fff` sentinel AND actually greater than
/// `num_rows_small` — the canonical consumption rule, since the large
/// field is meaningful only where the small one has overflowed. An earlier
/// version preferred the large field whenever it was not the sentinel,
/// which is backwards and would have read 0 rows off every data page once
/// the offset was corrected to 34.
fn page_row_count(page: &[u8]) -> Result<usize> {
    let flags = *page
        .get(PAGE_FLAGS_OFFSET)
        .ok_or_else(|| anyhow!("page too short for page_flags"))?;
    if flags & PAGE_FLAGS_NON_DATA != 0 {
        return Ok(0);
    }
    let small = *page
        .get(NUM_ROWS_SMALL_OFFSET)
        .ok_or_else(|| anyhow!("page too short for num_rows_small"))? as usize;
    let large = u16::from_le_bytes(
        page.get(NUM_ROWS_LARGE_OFFSET..NUM_ROWS_LARGE_OFFSET + 2)
            .ok_or_else(|| anyhow!("page too short for num_rows_large"))?
            .try_into()
            .unwrap(),
    );
    if large != NUM_ROWS_LARGE_SENTINEL && (large as usize) > small {
        Ok(large as usize)
    } else {
        Ok(small)
    }
}

/// Walks every LIVE row on one page via the row-group layout documented on
/// `GROUP_STRIDE_BYTES` above, calling `on_row` with each row's bytes (from
/// its start to the end of the page — callers must bound their own field
/// reads, since this does not know where the next row starts). Rows are
/// visited in ascending file/offset order, not the descending group-address
/// order they are stored in.
fn walk_page_rows(page: &[u8], num_rows: usize, mut on_row: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
    let mut offsets: Vec<usize> = Vec::with_capacity(num_rows);
    let mut accounted = 0usize;
    let mut g = 0usize;
    while accounted < num_rows {
        let base = page
            .len()
            .checked_sub(g * GROUP_STRIDE_BYTES)
            .ok_or_else(|| anyhow!("row group {g} underflows page start"))?;
        let present_mask = u16::from_le_bytes(
            page.get(base.wrapping_sub(4)..base.wrapping_sub(2))
                .ok_or_else(|| anyhow!("row group {g} present-mask out of bounds"))?
                .try_into()
                .unwrap(),
        );
        let slots_mask = u16::from_le_bytes(
            page.get(base.wrapping_sub(2)..base)
                .ok_or_else(|| anyhow!("row group {g} slots-mask out of bounds"))?
                .try_into()
                .unwrap(),
        );
        let n_slots = if slots_mask == 0 {
            0
        } else {
            (16 - slots_mask.leading_zeros() as usize).min(MAX_ROWS_PER_GROUP)
        };
        if n_slots == 0 {
            break; // malformed/empty group — stop rather than loop forever
        }
        for i in 0..n_slots {
            if (present_mask >> i) & 1 == 0 {
                continue; // tombstoned/deleted row — never emit it
            }
            let off = base
                .checked_sub(6 + 2 * i)
                .ok_or_else(|| anyhow!("row group {g} slot {i} offset out of bounds"))?;
            let rel = u16::from_le_bytes(
                page.get(off..off + 2)
                    .ok_or_else(|| anyhow!("row group {g} slot {i} offset field out of bounds"))?
                    .try_into()
                    .unwrap(),
            ) as usize;
            let row_start = PAGE_HEADER_LEN + rel;
            if row_start >= page.len() {
                continue; // out of range — skip rather than fail the whole page
            }
            offsets.push(row_start);
        }
        accounted += n_slots;
        g += 1;
        if g > 64 {
            break; // safety valve — no real page should need this many groups
        }
    }
    offsets.sort_unstable();
    for row_start in offsets {
        on_row(&page[row_start..])?;
    }
    Ok(())
}

/// Walks the pages of a table, from `first_page` through `last_page`
/// (inclusive, both taken from the table's own `TablePointer`), calling
/// `on_row` for every LIVE row found on every page in the chain, in
/// ascending file order.
///
/// Why bounded by `last_page` rather than followed via each page's
/// `next_page` field until it hits 0: the Task 2 header survey already
/// flagged that `next_unused_page` (the whole-file version of this field)
/// can point past every page actually present in the file, and the same
/// is true per-table — on the real fixture, playlist_tree's own last page
/// (page 16) has `next_page = 46`, three pages more than the entire
/// 42-page file. Following it panics/errors instead of stopping cleanly.
/// The table's `last_page` (from the header's table-pointer array, already
/// parsed and trusted by Task 3) is the correct, safe stopping point.
fn walk_table_pages(
    bytes: &[u8],
    header: &PdbHeader,
    first_page: u32,
    last_page: u32,
    mut on_row: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    let mut page_index = first_page;
    let mut visited = std::collections::HashSet::new();
    loop {
        if !visited.insert(page_index) {
            break; // cycle guard — never trust a linked list from an unofficial format
        }
        let off = page_offset(header, page_index)?;
        let page = bytes
            .get(off..off + header.page_len as usize)
            .ok_or_else(|| anyhow!("page {page_index} out of file bounds"))?;

        let next_page = u32le(page, 12)?;
        let num_rows = page_row_count(page)?;
        if num_rows > 0 {
            walk_page_rows(page, num_rows, &mut on_row)?;
        }

        if page_index >= last_page || next_page == 0 || next_page == page_index {
            break;
        }
        page_index = next_page;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct PlaylistNode {
    pub id: u32,
    pub parent_id: u32,
    pub name: String,
    pub is_folder: bool,
    pub sort_order: u32,
}

/// Width, in bytes, of a playlist_tree row's fixed numeric-field prefix
/// (parent_id, unknown, sort_order, id, raw_is_folder — five u4 fields)
/// before the DeviceSQL name string begins. Confirmed against the real
/// fixture by cross-table validation: the field at prefix offset 12
/// (`id`) reads 1 for "summer-hits", matching playlist_entries' single
/// row's `playlist_id = 1`.
const PLAYLIST_ROW_PREFIX_LEN: usize = 20;

pub fn parse_playlist_tree(bytes: &[u8], header: &PdbHeader) -> Result<Vec<PlaylistNode>> {
    let table = header
        .tables
        .iter()
        .find(|t| t.table_type == TABLE_TYPE_PLAYLIST_TREE)
        .ok_or_else(|| anyhow!("no playlist_tree table in this export.pdb"))?;

    let mut nodes = Vec::new();
    walk_table_pages(bytes, header, table.first_page, table.last_page, |row| {
        // Row layout confirmed against the real fixture: parent_id, unknown,
        // sort_order, id, raw_is_folder (u32 each), then a DeviceSQL string
        // for the name. See walk_table_pages' doc comment for how row_start
        // itself is located.
        if row.len() < PLAYLIST_ROW_PREFIX_LEN {
            return Ok(()); // malformed row, skip rather than fail the whole table
        }
        let parent_id = u32le(row, 0)?;
        let sort_order = u32le(row, 8)?;
        let id = u32le(row, 12)?;
        let raw_is_folder = u32le(row, 16)?;
        let name = parse_devicesql_string(&row[PLAYLIST_ROW_PREFIX_LEN..]).unwrap_or_else(|e| {
            tracing::warn!("could not decode Rekordbox playlist name for node {id}: {e}");
            String::new()
        });
        nodes.push(PlaylistNode {
            id,
            parent_id,
            name,
            is_folder: raw_is_folder != 0,
            sort_order,
        });
        Ok(())
    })?;
    Ok(nodes)
}

/// Tag byte of the one long-form DeviceSQL string shape this decoder has
/// real byte evidence for. See `parse_devicesql_string`.
const DEVICESQL_LONG_ASCII_TAG: u8 = 0x90;

/// Byte length of a long-form DeviceSQL string's header (tag byte, u16
/// total length, one further byte that reads 0x00 on the fixture). The
/// length field counts the header itself, so the body is `len - 4` bytes.
const DEVICESQL_LONG_HEADER_LEN: usize = 4;

/// DeviceSQL strings are length-prefixed, in one of two shapes selected by
/// a leading tag byte.
///
/// **Short form (odd tag byte)** — length embedded in the tag itself,
/// `(tag >> 1) - 1` bytes of ASCII follow. Verified against real strings in
/// the fixture: playlist names ("Untitled Playlist" -> tag 0x25 -> 17
/// bytes; "summer-hits" -> tag 0x19 -> 11 bytes) and, in the one live track
/// row, all twenty of its short strings including the 66-byte file path
/// (tag 0x87) and the 51-byte filename (tag 0x6b). Because the tag is a
/// single byte, `(0xff >> 1) - 1 = 126` is the longest string this form can
/// express — anything longer MUST use the long form, which is exactly why
/// the long form matters for real libraries: a DJ's folder + filename
/// commonly runs past 126 characters.
///
/// **Long form (even tag byte)** — `[tag][u16 total length][1 byte]` then
/// `length - 4` bytes of body. The fixture contains exactly ONE long-form
/// string, the track row's ISRC field at row-relative offset 136:
///
/// ```text
/// 90 12 00 00 03 55 53 41 54 32 32 35 30 34 33 36 34 00
/// ^tag  ^len=18  ^     ^ "USAT22504364"              ^NUL
///          ^hdr byte   ^ 0x03 quirk byte
/// ```
///
/// Those bytes are plain ASCII, NOT UTF-16 — decoding them as
/// little-endian UTF-16 (what this function used to do for every even tag)
/// produces mojibake. The body is wrapped: one leading control byte (0x03)
/// and one trailing NUL around the actual text. Two readings of that
/// leading byte fit the single sample equally well — either the long-form
/// header is really five bytes and 0x03 is part of it, or the header is
/// four bytes and 0x03 is a per-field quirk of how Rekordbox mangles the
/// ISRC (public writeups describe the ISRC as stored "in mangled format").
/// One sample cannot distinguish them, so this decoder does the thing that
/// is correct under BOTH readings: strip a single leading control byte
/// (< 0x20) and any trailing NULs, and treat what is left as the text.
///
/// **Any other even tag byte is an error, deliberately.** Public writeups
/// also describe long ASCII (0x40) and long UTF-16 shapes, but this
/// codebase has no real bytes for them, and a plausible-looking wrong
/// string is far worse here than a loud failure: a mis-decoded path fails
/// to match any indexed track and the entry silently vanishes from the
/// playlist. An `Err` instead surfaces as an `unresolved` count on
/// `ResolvedPlaylist` plus a `tracing::warn!` at the call site.
fn parse_devicesql_string(bytes: &[u8]) -> Result<String> {
    if bytes.is_empty() {
        bail!("empty string field");
    }
    let tag = bytes[0];
    // Short ASCII form: length embedded in the tag byte itself.
    if tag & 0x01 != 0 {
        let len = ((tag >> 1) as usize).saturating_sub(1);
        let s = bytes
            .get(1..1 + len)
            .ok_or_else(|| anyhow!("string length out of bounds"))?;
        return Ok(String::from_utf8_lossy(s).to_string());
    }
    if tag != DEVICESQL_LONG_ASCII_TAG {
        bail!("unsupported DeviceSQL long-string tag 0x{tag:02x}");
    }
    let len = u16::from_le_bytes(
        bytes
            .get(1..3)
            .ok_or_else(|| anyhow!("string length field out of bounds"))?
            .try_into()
            .unwrap(),
    ) as usize;
    let body_len = len
        .checked_sub(DEVICESQL_LONG_HEADER_LEN)
        .ok_or_else(|| anyhow!("long string length {len} shorter than its own header"))?;
    let mut body = bytes
        .get(DEVICESQL_LONG_HEADER_LEN..DEVICESQL_LONG_HEADER_LEN + body_len)
        .ok_or_else(|| anyhow!("string payload out of bounds"))?;
    // Leading control byte: either a fifth header byte or a per-field
    // quirk (see the doc comment) — stripping it is right either way.
    if body.first().is_some_and(|&b| b < 0x20) {
        body = &body[1..];
    }
    // Trailing C-string terminator(s).
    while body.last() == Some(&0) {
        body = &body[..body.len() - 1];
    }
    // An interior NUL would mean this is not the single-byte-per-character
    // encoding the fixture shows — refuse rather than emit half a string.
    if body.contains(&0) {
        bail!("long string body contains interior NUL bytes — unexpected encoding");
    }
    Ok(String::from_utf8_lossy(body).to_string())
}

#[derive(Debug, Clone)]
pub struct PlaylistEntry {
    pub playlist_id: u32,
    pub entry_index: u32,
    pub track_id: u32,
}

/// Row layout confirmed against the real fixture: the fixture's one
/// playlist_entries row decodes as three identical u32 fields (all = 1,
/// since the fixture has exactly one track/playlist/entry, so the values
/// alone can't disambiguate field order) — the ordering here (entry_index,
/// track_id, playlist_id) matches the publicly documented DeviceSQL
/// playlist_entries row shape. Fixed size, no string tail.
pub fn parse_playlist_entries(bytes: &[u8], header: &PdbHeader) -> Result<Vec<PlaylistEntry>> {
    let table = header
        .tables
        .iter()
        .find(|t| t.table_type == TABLE_TYPE_PLAYLIST_ENTRIES)
        .ok_or_else(|| anyhow!("no playlist_entries table in this export.pdb"))?;

    let mut entries = Vec::new();
    walk_table_pages(bytes, header, table.first_page, table.last_page, |row| {
        if row.len() < 12 {
            return Ok(()); // malformed row, skip rather than fail the whole table
        }
        entries.push(PlaylistEntry {
            entry_index: u32le(row, 0)?,
            track_id: u32le(row, 4)?,
            playlist_id: u32le(row, 8)?,
        });
        Ok(())
    })?;
    Ok(entries)
}

/// Byte offset, within a track row, of the track's own `id` field (u32 LE).
///
/// Confirmed against the real fixture's one live track row (tracks table,
/// page 2, row at page-relative offset 504) by cross-validating FIVE
/// independent fields against real, externally-known-plausible values, all
/// landing exactly where a recalled DeviceSQL `track_row` field ordering
/// predicts (each field spaced correctly from the last):
///   - offset  8 (u32): sample_rate = 44100 — a real AIFF/WAV sample rate.
///   - offset 48 (u32): bitrate = 1411 — exactly 44100 Hz * 16 bit * 2 ch /
///     1000, i.e. the textbook CD-quality PCM bitrate for this exact file.
///   - offset 56 (u32): tempo = 13100 -> 131.00 BPM (DeviceSQL stores tempo
///     as BPM * 100), a plausible dance-track tempo.
///   - offset 80 (u16): year = 2025, offset 82 (u16): sample_depth = 16,
///     offset 84 (u16): duration = 206 seconds (3:26) — all independently
///     plausible for a real purchased AIFF track.
///   - offset 72 (u32): id = 1, matching playlist_entries' one row's
///     track_id = 1 exactly.
/// Every one of these lines up with the predicted field spacing (4 bytes
/// per u32 field, 2 bytes per u16 field, in the recalled order: unknown1,
/// index_shift, bitmask, sample_rate, composer_id, file_size, unknown2,
/// unknown3, unknown4, artwork_id, key_id, orig_artist_id, label_id,
/// remixer_id, bitrate, track_number, tempo, genre_id, album_id, artist_id,
/// id, disc_number, play_count, year, sample_depth, duration, ...) — five
/// independent hits in the predicted positions is strong corroboration,
/// not coincidence. NOTE: because this fixture has exactly one live track,
/// several neighbouring FK fields (artwork_id, key_id, label_id,
/// track_number, genre_id, album_id, artist_id) also happen to read 1 (this
/// being the first and only row in each of those linked tables too), so the
/// id field's value alone could not disambiguate its offset — the offset
/// was pinned by the field-spacing pattern and the other four unrelated
/// value matches instead.
const TRACK_ROW_ID_OFFSET: usize = 72;

/// Byte length a track row must have to safely read the `id` field.
const TRACK_ROW_MIN_LEN: usize = TRACK_ROW_ID_OFFSET + 4;

/// Row-relative byte offset of the track row's string-offset table.
///
/// A code review of this task found the offset table (a run of 21
/// monotonically increasing u16 values, each one a row-relative offset to
/// one of the row's DeviceSQL strings) starts here — decoded independently
/// of, and confirming, the earlier `find_path_string_in_track_row` scan:
/// entry index 20 (the last of the 21) reads 350 on the real fixture's one
/// live track row, landing exactly on the `0x87` tag byte of the full
/// stick-relative file path (`/Library AIFF/Tinashe, Disco Lines - No Broke
/// Boys (Extended).aiff`) — see `tests/rekordbox_track_survey.rs` for the
/// byte-level walkthrough. This is self-consistent (all 21 entries land on
/// plausible string/empty-string boundaries when walked in order) and
/// closes the confidence gap the original scan-based approach left open.
const TRACK_ROW_STRING_OFFSET_TABLE: usize = 94;

/// Index, within the row's string-offset table, of the entry pointing at
/// the full file path (as opposed to the bare filename, artist, title,
/// etc. — the table carries one entry per known string field, most of
/// them present in this row). Confirmed against the real fixture: entry 20
/// -> row-relative offset 350 -> DeviceSQL tag byte 0x87 -> decodes to the
/// full path.
const TRACK_ROW_PATH_TABLE_INDEX: usize = 20;

/// Row-relative byte offset of the path field's u16 offset-table entry
/// itself (not the string it points to): `TRACK_ROW_STRING_OFFSET_TABLE +
/// TRACK_ROW_PATH_TABLE_INDEX * 2`.
const TRACK_ROW_PATH_OFFSET_ENTRY: usize = TRACK_ROW_STRING_OFFSET_TABLE + TRACK_ROW_PATH_TABLE_INDEX * 2;

pub fn parse_track_paths(
    bytes: &[u8],
    header: &PdbHeader,
) -> Result<std::collections::HashMap<u32, String>> {
    let table = header
        .tables
        .iter()
        .find(|t| t.table_type == TABLE_TYPE_TRACKS)
        .ok_or_else(|| anyhow!("no tracks table in this export.pdb"))?;

    let mut paths = std::collections::HashMap::new();
    walk_table_pages(bytes, header, table.first_page, table.last_page, |row| {
        if row.len() < TRACK_ROW_MIN_LEN {
            return Ok(()); // malformed row, skip rather than fail the whole table
        }
        let id = u32le(row, TRACK_ROW_ID_OFFSET)?;
        if let Some(path) = read_track_path(row) {
            paths.insert(id, path);
        }
        Ok(())
    })?;
    Ok(paths)
}

/// Reads a track row's full stick-relative file path via a bounded,
/// deterministic lookup: the u16 at `TRACK_ROW_PATH_OFFSET_ENTRY`
/// (row-relative) is itself a row-relative offset to the path string's
/// DeviceSQL tag byte. This deliberately does NOT scan the row (a row
/// slice extends to the END OF THE PAGE, per `walk_page_rows`' own doc
/// comment — a full-row scan on a page with multiple rows would read past
/// this row into every later row's bytes and could pick up a LATER row's
/// path as if it were this row's, silently mis-attributing files to track
/// ids; invisible on this fixture's one live row, real on any multi-track
/// page). Returns `None` when the row is too short for the offset table
/// entry, when the offset itself points outside the row, or when the byte
/// there doesn't decode as a DeviceSQL string.
fn read_track_path(row: &[u8]) -> Option<String> {
    let entry = row.get(TRACK_ROW_PATH_OFFSET_ENTRY..TRACK_ROW_PATH_OFFSET_ENTRY + 2)?;
    let rel = u16::from_le_bytes(entry.try_into().ok()?) as usize;
    let tag_byte = row.get(rel..)?;
    match parse_devicesql_string(tag_byte) {
        Ok(path) => Some(path),
        Err(e) => {
            // Loud, because the consequence is silent: an undecodable path
            // matches no indexed track, so the entry drops out of whatever
            // playlist references it (counted in `ResolvedPlaylist::unresolved`).
            tracing::warn!("could not decode a Rekordbox track path: {e}");
            None
        }
    }
}

/// One playlist node with its resolved SpinDex track ids, in playlist order.
#[derive(Debug, Clone)]
pub struct ResolvedPlaylist {
    pub id: u32,
    pub parent_id: u32,
    pub name: String,
    pub is_folder: bool,
    pub track_ids: Vec<i64>,
    /// Count of this playlist's entries that did NOT end up in `track_ids`
    /// — either the entry's track_id had no path in the tracks table, or
    /// the path didn't resolve to any indexed SpinDex track under `letter`.
    /// Kept separate from `track_ids` (rather than silently dropped) so a
    /// systemic resolution failure — e.g. every entry unresolved — is a
    /// loud, visible signal instead of an indistinguishable empty crate.
    pub unresolved: usize,
}

/// Parses an export.pdb's playlist tree, playlist_entries and track paths,
/// and resolves every entry's track to a SpinDex track id via the shared
/// stick-path resolver (`crate::core::stick_path`).
pub fn resolve_playlists(
    conn: &rusqlite::Connection,
    letter: &str,
    bytes: &[u8],
) -> Result<Vec<ResolvedPlaylist>> {
    let header = parse_header(bytes)?;
    let nodes = parse_playlist_tree(bytes, &header)?;
    let entries = parse_playlist_entries(bytes, &header)?;
    let paths = parse_track_paths(bytes, &header)?;

    let mut by_playlist: std::collections::HashMap<u32, Vec<&PlaylistEntry>> =
        std::collections::HashMap::new();
    for e in &entries {
        by_playlist.entry(e.playlist_id).or_default().push(e);
    }
    for v in by_playlist.values_mut() {
        v.sort_by_key(|e| e.entry_index);
    }

    // Fetched ONCE for the whole export: `db::list_libraries` runs a
    // correlated per-library track-count subquery, and a real export has
    // thousands of playlist entries to resolve.
    let libs = crate::core::stick_path::libraries(conn, letter).map_err(|e| anyhow!(e))?;

    let mut result = Vec::with_capacity(nodes.len());
    for node in &nodes {
        let (track_ids, unresolved) = match by_playlist.get(&node.id) {
            Some(entries) => {
                let mut track_ids = Vec::with_capacity(entries.len());
                let mut unresolved = 0usize;
                for e in entries {
                    // Rekordbox's file_path field starts with a leading
                    // '/' (confirmed on the real fixture) — the resolver
                    // expects a stick-relative path with NO leading
                    // separator (same convention as scanner-stored
                    // rel_path), so it must be stripped here or every
                    // lookup fails structurally, resolving nothing on any
                    // real device.
                    let resolved = paths.get(&e.track_id).and_then(|path| {
                        let rel = path.trim_start_matches(['/', '\\']);
                        crate::core::stick_path::resolve_with(&libs, conn, letter, rel)
                    });
                    match resolved {
                        Some(id) => track_ids.push(id),
                        None => unresolved += 1,
                    }
                }
                (track_ids, unresolved)
            }
            None => (Vec::new(), 0),
        };
        result.push(ResolvedPlaylist {
            id: node.id,
            parent_id: node.parent_id,
            name: node.name.clone(),
            is_folder: node.is_folder,
            track_ids,
            unresolved,
        });
    }
    Ok(result)
}

#[derive(Debug, Clone)]
pub struct PlaylistTreeItem {
    pub node: PlaylistNode,
    pub children: Vec<PlaylistTreeItem>,
}

pub fn build_playlist_hierarchy(nodes: &[PlaylistNode]) -> Vec<PlaylistTreeItem> {
    fn children_of(nodes: &[PlaylistNode], parent_id: u32) -> Vec<PlaylistTreeItem> {
        let mut items: Vec<PlaylistTreeItem> = nodes
            .iter()
            .filter(|n| n.parent_id == parent_id)
            .map(|n| PlaylistTreeItem {
                node: n.clone(),
                children: children_of(nodes, n.id),
            })
            .collect();
        items.sort_by_key(|i| i.node.sort_order);
        items
    }
    children_of(nodes, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real export.pdb fixture. It is local-only (test data is never
    /// committed), so the tests that need it skip on a clone without it.
    fn fixture_bytes() -> Option<Vec<u8>> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/rekordbox/export.pdb"
        ))
        .ok()
    }

    #[test]
    fn parses_header_from_real_export_pdb() {
        let Some(bytes) = fixture_bytes() else { return };
        let header = parse_header(&bytes).expect("header should parse");
        assert!(header.page_len > 0);
        assert!(header.num_tables > 0);
        assert_eq!(header.tables.len() as u32, header.num_tables);
        assert!(header
            .tables
            .iter()
            .any(|t| t.table_type == TABLE_TYPE_PLAYLIST_TREE));
        assert!(header
            .tables
            .iter()
            .any(|t| t.table_type == TABLE_TYPE_PLAYLIST_ENTRIES));
        assert!(header
            .tables
            .iter()
            .any(|t| t.table_type == TABLE_TYPE_TRACKS));
    }

    #[test]
    fn rejects_a_truncated_file_without_panicking() {
        let bytes = vec![0u8; 4];
        assert!(parse_header(&bytes).is_err());
    }

    // Task 4: parse playlist_tree rows.
    //
    // The fixture committed for this task (commit 8235eb7, replacing the
    // earlier empty-library export the same task first found and reported
    // BLOCKED on) has TWO row slots in playlist_tree's page-16 row group,
    // but only ONE is live: slot 0 ("Untitled Playlist") is a tombstoned
    // (deleted/renamed) row — its slots-mask (0x0003, 2 slots allocated)
    // disagrees with its present-mask (0x0002, only slot 1 live). Slot 1
    // ("summer-hits", id=1) is the fixture's one real playlist. This was
    // caught in code review (a first pass at this task read the tombstone
    // as a second live row, since its numeric prefix bytes are literally
    // identical to the live row's — same playlist before a rename, so of
    // course the leftover bytes match). The row-parsing logic itself is
    // cross-validated against playlist_entries' single row, whose
    // `playlist_id = 1` matches "summer-hits"' `id` field exactly.
    //
    // The fixture does NOT contain a nested playlist folder, despite the
    // original request for one — with only one live playlist, there is
    // nothing to nest. Per the coordinator's explicit guidance this is a
    // known gap in test coverage, not a blocker.
    #[test]
    fn parses_playlist_tree_from_real_export_pdb() {
        let Some(bytes) = fixture_bytes() else { return };
        let header = parse_header(&bytes).unwrap();
        let nodes = parse_playlist_tree(&bytes, &header).unwrap();

        assert_eq!(
            nodes.len(),
            1,
            "fixture has one tombstoned row and one live row — only the live one should be emitted"
        );
        let node = &nodes[0];
        assert_eq!(node.id, 1);
        assert_eq!(node.name, "summer-hits");
        assert_eq!(node.parent_id, 0, "the fixture's one playlist is top-level (no folder to nest under)");
        assert!(!node.is_folder);

        let tree = build_playlist_hierarchy(&nodes);
        assert_eq!(tree.len(), 1, "the one playlist should be a root (parent_id 0)");
        assert!(tree[0].children.is_empty());
    }

    // Regression test for the group-based row-offset scheme itself, against
    // a table this task does NOT otherwise touch: the "columns" system
    // table (table_type=16, page 34), which has 27 rows spanning TWO row
    // groups (16 + 11) — the one multi-group table in the fixture. A code
    // review of this task's first pass found the row-walking logic only
    // worked by coincidence for a single group ≤16 rows all-present (the
    // 2-row playlist_tree and 8-row colors cases); on this 27-row table it
    // silently dropped 2 real rows and fabricated 2 phantom ones (reading
    // the group's own bitmask fields as if they were row offsets). This
    // pins the fix directly against that failure mode so it can't recur
    // silently — asserting a monotonic, gap-free row_start sequence with no
    // duplicates and no wildly out-of-range values, without depending on
    // this module's playlist-specific field layout (columns rows aren't
    // playlist_tree rows; only the page/row-group walking is shared).
    #[test]
    fn walks_the_multi_group_columns_table_without_dropping_or_fabricating_rows() {
        let Some(bytes) = fixture_bytes() else { return };
        let header = parse_header(&bytes).unwrap();
        let page_index = 34u32; // columns, table_type=16, verified via the header survey
        let page_off = page_offset(&header, page_index).unwrap();
        let page = &bytes[page_off..page_off + header.page_len as usize];
        let num_rows = page_row_count(page).unwrap();
        assert_eq!(num_rows, 27, "columns table should report 27 rows");

        let base_ptr = page.as_ptr() as usize;
        let mut row_starts = Vec::new();
        walk_page_rows(page, num_rows, |row| {
            row_starts.push(row.as_ptr() as usize - base_ptr);
            Ok(())
        })
        .unwrap();

        assert_eq!(row_starts.len(), 27, "all 27 rows should be visited exactly once, no drops or duplicates");
        assert!(
            row_starts.windows(2).all(|w| w[0] < w[1]),
            "rows should be strictly increasing (ascending file order, no duplicates): {row_starts:?}"
        );
        assert_eq!(row_starts[0], 40, "first row should sit right after the 40-byte page header");
        // Every real row's numeric prefix + short string comfortably fits in
        // a few dozen bytes; a phantom row misread from a bitmask field
        // (like the reviewer's observed 2087 = 40 + 0x07ff) would blow well
        // past any plausible page-16-column-label content region.
        assert!(
            row_starts.iter().all(|&r| r < 1000),
            "no row should land implausibly far into the page (phantom-offset guard): {row_starts:?}"
        );
    }

    #[test]
    fn parses_playlist_entries_and_track_paths() {
        let Some(bytes) = fixture_bytes() else { return };
        let header = parse_header(&bytes).unwrap();
        let entries = parse_playlist_entries(&bytes, &header).unwrap();
        assert!(!entries.is_empty());

        let paths = parse_track_paths(&bytes, &header).unwrap();
        assert!(!paths.is_empty());
        // every entry's track_id should resolve to some path in the track table
        let unresolved: Vec<_> = entries
            .iter()
            .filter(|e| !paths.contains_key(&e.track_id))
            .collect();
        assert!(
            unresolved.is_empty(),
            "playlist_entries referencing unknown track ids: {unresolved:?}"
        );

        // The fixture's one live track resolves to the full stick-relative
        // path (leading slash, folder segment), not the bare filename.
        let path = paths.values().next().unwrap();
        assert!(path.starts_with('/'), "expected a full path, got {path:?}");
        assert!(path.contains('/'), "expected folder segments, got {path:?}");
        assert!(path.ends_with(".aiff"), "expected the real extension, got {path:?}");
    }

    #[test]
    fn resolves_playlists_against_a_fake_db_with_no_matching_libraries() {
        let Some(bytes) = fixture_bytes() else { return };
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::core::db::SCHEMA).unwrap();
        crate::core::db::migrate(&conn).unwrap();
        let resolved = resolve_playlists(&conn, "Z:", &bytes).unwrap();
        assert!(!resolved.is_empty());
        // no libraries indexed for "Z:" in this empty DB, so every
        // playlist's tracks are unresolved (empty track_ids AND a nonzero
        // unresolved count — this test can't tell "correctly empty" from
        // "resolver is structurally broken" on track_ids alone, which is
        // exactly why `unresolved` exists as a separate, loud signal).
        // See `resolves_playlists_against_a_db_with_a_matching_library`
        // below for proof this isn't just because resolution never works.
        assert!(resolved.iter().all(|p| p.track_ids.is_empty()));
        let node = resolved.iter().find(|p| p.id == 1).expect("summer-hits playlist");
        assert_eq!(node.name, "summer-hits");
        assert!(!node.is_folder);
        assert_eq!(node.unresolved, 1, "the fixture's one entry should be counted unresolved, not silently dropped");
    }

    #[test]
    fn resolves_playlists_against_a_db_with_a_matching_library() {
        // Genuine positive-resolution test: seeds an in-memory DB with a
        // library whose root matches the fixture's drive-letter convention
        // and a track row whose rel_path matches the fixture's real
        // decoded path (same leading-slash-stripping the fix applies),
        // then asserts resolve_playlists actually returns that track's id
        // — not just an empty vec, which the fake-db test above cannot
        // distinguish from a broken resolver.
        let Some(bytes) = fixture_bytes() else { return };
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::core::db::SCHEMA).unwrap();
        crate::core::db::migrate(&conn).unwrap();

        let header = parse_header(&bytes).unwrap();
        let paths = parse_track_paths(&bytes, &header).unwrap();
        let raw_path = paths.get(&1).expect("fixture's one track (id=1)");
        let rel_path = raw_path.trim_start_matches(['/', '\\']);

        conn.execute(
            "INSERT INTO library (id, name, root_path) VALUES (1, 'stick', 'D:\\')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO track (id, library_id, rel_path, file_size, modified_at) VALUES (42, 1, ?1, 1, '0')",
            [rel_path],
        )
        .unwrap();

        let resolved = resolve_playlists(&conn, "D:", &bytes).unwrap();
        let node = resolved.iter().find(|p| p.id == 1).expect("summer-hits playlist");
        assert_eq!(node.track_ids, vec![42], "the seeded track should resolve, not stay empty");
        assert_eq!(node.unresolved, 0, "the one real entry should now be resolved, not counted unresolved");
    }

    #[test]
    fn parse_devicesql_string_decodes_real_short_ascii_names() {
        // tag 0x25 -> length 17 -> "Untitled Playlist"; tag 0x19 -> length
        // 11 -> "summer-hits". Both taken verbatim from the real fixture.
        let mut untitled = vec![0x25u8];
        untitled.extend_from_slice(b"Untitled Playlist");
        assert_eq!(parse_devicesql_string(&untitled).unwrap(), "Untitled Playlist");

        let mut summer = vec![0x19u8];
        summer.extend_from_slice(b"summer-hits");
        assert_eq!(parse_devicesql_string(&summer).unwrap(), "summer-hits");
    }

    /// The one long-form DeviceSQL string in the committed fixture, copied
    /// byte for byte out of the live track row (page 2, row at page-relative
    /// offset 504, row-relative offset 136 — the first entry of that row's
    /// 21-entry string-offset table). It is the track's ISRC.
    ///
    /// This is the test vector that proves the long form carries ASCII, not
    /// UTF-16: the old decoder framed these same bytes correctly and then
    /// read them as little-endian UTF-16, yielding mojibake.
    const REAL_ISRC_BYTES: [u8; 18] = [
        0x90, 0x12, 0x00, 0x00, 0x03, b'U', b'S', b'A', b'T', b'2', b'2', b'5', b'0', b'4', b'3',
        b'6', b'4', 0x00,
    ];

    #[test]
    fn parse_devicesql_string_decodes_the_real_long_form_isrc_as_ascii() {
        assert_eq!(parse_devicesql_string(&REAL_ISRC_BYTES).unwrap(), "USAT22504364");
    }

    #[test]
    fn parse_devicesql_string_refuses_long_tags_it_has_no_evidence_for() {
        // 0x40 is described in public writeups as another long form, but no
        // real bytes for it exist in this repo. Guessing produces a
        // plausible-looking wrong path, which silently drops a track from a
        // playlist; an error is visible (unresolved count + warn).
        let mut unknown = REAL_ISRC_BYTES.to_vec();
        unknown[0] = 0x40;
        let err = parse_devicesql_string(&unknown).unwrap_err().to_string();
        assert!(err.contains("0x40"), "error should name the tag it refused: {err}");
    }

    #[test]
    fn parse_devicesql_string_refuses_a_long_body_that_is_not_single_byte_encoded() {
        // Interior NULs are what a UTF-16 body would look like. We have no
        // evidence for that encoding, so it must fail loudly rather than
        // decode to half a string.
        let body = b"\x00A\x00B\x00C";
        let mut utf16ish = vec![0x90u8, (4 + body.len()) as u8, 0x00, 0x00];
        utf16ish.extend_from_slice(body);
        assert!(parse_devicesql_string(&utf16ish).is_err());
    }

    #[test]
    fn the_fixtures_own_track_path_still_decodes_after_the_long_form_fix() {
        // The path string uses the SHORT form (tag 0x87, 66 bytes) — the
        // long-form change must not disturb it. Pinned here as well as in
        // the resolve tests so a regression names the cause directly.
        let Some(bytes) = fixture_bytes() else { return };
        let header = parse_header(&bytes).unwrap();
        let paths = parse_track_paths(&bytes, &header).unwrap();
        assert_eq!(
            paths.get(&1).map(String::as_str),
            Some("/Library AIFF/Tinashe, Disco Lines - No Broke Boys (Extended).aiff")
        );
    }
}
