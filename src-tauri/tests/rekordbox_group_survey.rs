// src-tauri/tests/rekordbox_group_survey.rs
// Diagnostic only — verifying the reviewer-proposed group-based row-offset
// model against the real fixture before reimplementing walk_table_pages.
// Run with:
//   cd src-tauri && cargo test --test rekordbox_group_survey -- --ignored --nocapture

use std::fs;

const GROUP_STRIDE: usize = 0x24; // 36 bytes per row-group
const MAX_ROWS_PER_GROUP: usize = 16;
const PAGE_HEADER_LEN: usize = 40;

fn group_rows(page: &[u8], num_rows: usize) -> Vec<(usize, u16, u16, u16)> {
    // returns (absolute_row_offset, relative_value, present_mask, slots_mask) for every PRESENT row
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
            16 - slots_mask.leading_zeros() as usize
        };
        for i in 0..n_slots.min(MAX_ROWS_PER_GROUP) {
            let off = base - 6 - 2 * i;
            let rel = u16::from_le_bytes(page[off..off + 2].try_into().unwrap());
            let present = (present_mask >> i) & 1 == 1;
            if present {
                out.push((PAGE_HEADER_LEN + rel as usize, rel, present_mask, slots_mask));
            }
        }
        accounted += n_slots;
        g += 1;
        if g > 8 {
            break; // safety
        }
    }
    out
}

#[test]
#[ignore]
fn survey_group_model_playlist_tree() {
    let bytes = fs::read("tests/fixtures/rekordbox/export.pdb").expect("fixture missing");
    let page_len = 4096usize;
    let page_index = 16usize;
    let off = page_index * page_len;
    let page = &bytes[off..off + page_len];
    let num_rows = page[24] as usize;
    println!("page {page_index} num_rows={num_rows}");
    let rows = group_rows(page, num_rows);
    for (abs, rel, present, slots) in &rows {
        println!("  abs={abs} rel={rel} present_mask=0x{present:04x} slots_mask=0x{slots:04x}");
    }
}

#[test]
#[ignore]
fn survey_group_model_columns_multi_group() {
    let bytes = fs::read("tests/fixtures/rekordbox/export.pdb").expect("fixture missing");
    let page_len = 4096usize;
    let page_index = 34usize; // columns table, table_type=16, 27 rows (multi-group)
    let off = page_index * page_len;
    let page = &bytes[off..off + page_len];
    let num_rows = page[24] as usize;
    println!("page {page_index} num_rows={num_rows}");
    let mut rows = group_rows(page, num_rows);
    rows.sort_by_key(|r| r.0);
    println!("count found: {}", rows.len());
    for (abs, rel, present, slots) in &rows {
        println!("  abs={abs} rel={rel} present_mask=0x{present:04x} slots_mask=0x{slots:04x}");
    }
    // Decode each as a devicesql short-ascii string to sanity check against
    // the known column-label strings (GENRE, ARTIST, ...).
    let decode = |b: &[u8]| -> String {
        if b.is_empty() {
            return String::new();
        }
        let tag = b[0];
        if tag & 1 != 0 {
            let len = ((tag >> 1) as usize).saturating_sub(1);
            String::from_utf8_lossy(&b[1..(1 + len).min(b.len())]).to_string()
        } else {
            "<non-ascii>".to_string()
        }
    };
    for (abs, ..) in &rows {
        let slice = &page[*abs..(*abs + 40).min(page.len())];
        println!("    decoded name at {abs}: {:?}", decode(slice));
    }
}

#[test]
#[ignore]
fn survey_num_rows_large_and_flags() {
    let bytes = fs::read("tests/fixtures/rekordbox/export.pdb").expect("fixture missing");
    let page_len = 4096usize;
    for page_index in [15usize, 16, 34, 14, 17, 18] {
        let off = page_index * page_len;
        let page = &bytes[off..off + page_len];
        let num_rows_small = page[24];
        let page_flags = page[27];
        let num_rows_large = u16::from_le_bytes(page[32..34].try_into().unwrap());
        println!(
            "page {page_index}: num_rows_small={num_rows_small} num_rows_large=0x{num_rows_large:04x} page_flags=0x{page_flags:02x} flag_0x40_set={}",
            page_flags & 0x40 != 0
        );
    }
}
