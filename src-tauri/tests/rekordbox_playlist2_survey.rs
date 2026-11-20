// src-tauri/tests/rekordbox_playlist2_survey.rs
// Diagnostic only — throwaway exploratory code re-running Task 4's row-level
// byte survey against the REPLACED, populated fixture (commit 8235eb7).
// Not a foundation for the real parser on its own; findings feed directly
// into implementing parse_playlist_tree. Run with:
//   cd src-tauri && cargo test --test rekordbox_playlist2_survey -- --ignored --nocapture

use std::fs;

fn hexdump(page: &[u8], base: usize) {
    for (i, chunk) in page.chunks(16).enumerate() {
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
fn survey_playlist_tree_pages() {
    let bytes = fs::read("tests/fixtures/rekordbox/export.pdb")
        .expect("fixture missing");
    let page_len = 4096usize;
    let u32le = |b: &[u8], o: usize| -> u32 { u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) };

    // New header survey: table[7] type=7 (playlist_tree) first_page=15 last_page=16.
    for page_index in [15usize, 16] {
        let off = page_index * page_len;
        let page = &bytes[off..off + page_len];
        let num_rows_small = page[24];
        let page_flags = page[27];
        let next_page = u32le(page, 12);
        println!(
            "page {page_index}: next_page={next_page} num_rows_small={num_rows_small} page_flags=0x{page_flags:02x}"
        );
    }

    println!("=== page 16 full hexdump (expected real playlist_tree rows) ===");
    let off = 16 * page_len;
    let page = &bytes[off..off + page_len];
    hexdump(page, 0);

    println!("=== page 16 tail 64 bytes (offset table) ===");
    hexdump(&page[page_len - 64..page_len], page_len - 64);

    println!("=== page 16: scan whole page for u16 == 40 or 80 or 100 or 60 ===");
    for o in 0..page.len() - 1 {
        let v = u16::from_le_bytes(page[o..o + 2].try_into().unwrap());
        if matches!(v, 40 | 60 | 80 | 100 | 61 | 101) {
            println!("  offset {o}: value {v}");
        }
    }

    println!("=== page 16 bytes 0..120 detailed dump ===");
    hexdump(&page[0..120], 0);

    println!("=== page 16 bytes 36..104 byte-exact (row1/row2 boundary) ===");
    for o in 36..104 {
        print!("{:02x} ", page[o]);
        if (o - 35) % 8 == 0 {
            println!("  <- offset {}", o - 7);
        }
    }
    println!();

    println!("=== brute force: for candidate row2 start s in 64..100, print (parent_id,unknown,sort_order,id,is_folder) ===");
    for s in 64..100 {
        if s + 20 > page.len() {
            continue;
        }
        let parent_id = u32le(page, s);
        let unknown = u32le(page, s + 4);
        let sort_order = u32le(page, s + 8);
        let id = u32le(page, s + 12);
        let is_folder = u32le(page, s + 16);
        if id != 0 && id < 1000 {
            println!(
                "  s={s}: parent_id={parent_id} unknown={unknown} sort_order={sort_order} id={id} is_folder={is_folder}"
            );
        }
    }

    // playlist_entries: table[8], new header says first_page=17 last_page=18.
    println!("=== page 18 (playlist_entries, expected real data) header+body ===");
    let off18 = 18 * page_len;
    let page18 = &bytes[off18..off18 + page_len];
    println!("num_rows_small={} page_flags=0x{:02x}", page18[24], page18[27]);
    hexdump(&page18[0..200], 0);
    println!("=== page 18 tail 64 ===");
    hexdump(&page18[page_len - 64..page_len], page_len - 64);
}
