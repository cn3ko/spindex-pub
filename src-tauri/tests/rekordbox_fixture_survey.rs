// src-tauri/tests/rekordbox_fixture_survey.rs
// Diagnostic only — not asserting correctness, printing structure to learn it.
// Run with: cargo test --test rekordbox_fixture_survey -- --ignored --nocapture
//
// This is throwaway exploratory code kept as documentation of the byte survey
// (SP1 Task 2). It is NOT a foundation for the real parser — Task 3 writes
// that from scratch informed by the recorded output at the bottom of this
// file. Do not delete this file; do not extend it with real parsing logic.

use std::fs;

#[test]
#[ignore]
fn survey_header() {
    let bytes = fs::read("tests/fixtures/rekordbox/export.pdb")
        .expect("fixture missing — see Task 2 of the SP1 plan");
    println!("file length: {}", bytes.len());
    println!("first 64 bytes: {:02x?}", &bytes[0..64.min(bytes.len())]);

    // DeviceSQL header (per public reverse-engineering writeups referenced
    // in the spec): u32 LE zero, u32 LE page_len, u32 LE num_tables,
    // u32 LE next_unused_page, u32 LE unknown, u32 LE sequence, u32 LE gap,
    // then num_tables table-pointer entries of 4x u32 LE each
    // (type, empty_candidate, first_page, last_page).
    let u32le = |off: usize| -> u32 {
        u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
    };
    println!("offset 0 (expect 0): {}", u32le(0));
    println!("offset 4 (page_len candidate): {}", u32le(4));
    println!("offset 8 (num_tables candidate): {}", u32le(8));
    println!("offset 12 (next_unused_page candidate): {}", u32le(12));
    println!("offset 16 (unknown): {}", u32le(16));
    println!("offset 20 (sequence): {}", u32le(20));
    println!("offset 24 (gap): {}", u32le(24));

    let num_tables = u32le(8) as usize;
    for i in 0..num_tables.min(32) {
        let base = 28 + i * 16;
        if base + 16 > bytes.len() {
            break;
        }
        println!(
            "table[{i}] type={} empty_candidate={} first_page={} last_page={}",
            u32le(base),
            u32le(base + 4),
            u32le(base + 8),
            u32le(base + 12)
        );
    }
}

// ---------------------------------------------------------------------------
// Recorded output — RE-RUN 2026-08-19 against the CURRENT committed fixture
// (42 pages / 172,032 bytes, a real populated export). The block that used to
// live here recorded the ORIGINAL, empty fixture (41 pages / 167,936 bytes)
// that was later replaced; its numbers and its conclusions no longer describe
// anything in this repo, so they were removed rather than kept alongside.
// Run with:
//   cd src-tauri && cargo test --test rekordbox_fixture_survey -- --ignored --nocapture
//
// running 1 test
// file length: 172032
// offset 0 (expect 0): 0
// offset 4 (page_len candidate): 4096
// offset 8 (num_tables candidate): 20
// offset 12 (next_unused_page candidate): 56
// offset 16 (unknown): 1
// offset 20 (sequence): 25
// offset 24 (gap): 0
// table[0]  type=0  empty_candidate=52 first_page=1  last_page=2
// table[1]  type=1  empty_candidate=48 first_page=3  last_page=4
// table[2]  type=2  empty_candidate=47 first_page=5  last_page=6
// table[3]  type=3  empty_candidate=49 first_page=7  last_page=8
// table[4]  type=4  empty_candidate=50 first_page=9  last_page=10
// table[5]  type=5  empty_candidate=51 first_page=11 last_page=12
// table[6]  type=6  empty_candidate=42 first_page=13 last_page=14
// table[7]  type=7  empty_candidate=46 first_page=15 last_page=16
// table[8]  type=8  empty_candidate=55 first_page=17 last_page=18
// table[9]  type=9  empty_candidate=20 first_page=19 last_page=19
// table[10] type=10 empty_candidate=22 first_page=21 last_page=21
// table[11] type=11 empty_candidate=24 first_page=23 last_page=23
// table[12] type=12 empty_candidate=26 first_page=25 last_page=25
// table[13] type=13 empty_candidate=54 first_page=27 last_page=28
// table[14] type=14 empty_candidate=30 first_page=29 last_page=29
// table[15] type=15 empty_candidate=32 first_page=31 last_page=31
// table[16] type=16 empty_candidate=43 first_page=33 last_page=34
// table[17] type=17 empty_candidate=44 first_page=35 last_page=36
// table[18] type=18 empty_candidate=45 first_page=37 last_page=38
// table[19] type=19 empty_candidate=53 first_page=39 last_page=41
// test survey_header ... ok
//
// Observations that survived the fixture swap and are still true:
// - page_len = 4096; type == index for all 20 tables in this fixture too.
//   Still do NOT rely on that — always read the type field.
// - next_unused_page (56) is higher than any page the file actually holds
//   (42 pages, valid indices 0..41), so it does not bound the readable
//   range. Trust bytes.len() and each table's own first_page..last_page.
// - The canonical table-type enum used by the production parser (tracks=0,
//   playlist_tree=7, playlist_entries=8, columns=16) is no longer a
//   hypothesis: it was confirmed against real row contents by
//   rekordbox_playlist2_survey.rs, rekordbox_group_survey.rs and
//   rekordbox_track_survey.rs, which are the byte-level ground truth for
//   this format and supersede this file for anything beyond the header.
// ---------------------------------------------------------------------------
