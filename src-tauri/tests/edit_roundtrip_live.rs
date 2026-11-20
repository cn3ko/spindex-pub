//! Live round-trip: write a tag + artwork to a copy of a real file placed ON
//! the target medium (e.g. the USB stick itself), then read it back. Proves
//! whether edits actually persist to the physical file on that filesystem.
//! Env-gated; the copy is created next to the source and deleted afterward.
//!
//!   SPINDEX_EDIT_FILE="D:\Library AIFF\ACRAZE - Do It To It (Extended Mix).aiff" \
//!     cargo test --test edit_roundtrip_live -- --ignored --nocapture

use spindex::core::metadata::{self, MimeType, TagUpdate};

#[test]
#[ignore]
fn edit_roundtrip_live() {
    let Ok(src) = std::env::var("SPINDEX_EDIT_FILE") else {
        eprintln!("SPINDEX_EDIT_FILE not set, skipping");
        return;
    };
    let src = std::path::PathBuf::from(src);
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_string();
    // Copy sits on the SAME medium as the source (same parent dir).
    let copy = src.with_file_name(format!("_spindex_roundtrip_test.{ext}"));
    std::fs::copy(&src, &copy).expect("copy source to test file");
    println!("test file: {}", copy.display());

    let marker = "SPINDEX_ROUNDTRIP_MARKER";
    let existing = metadata::read(&src).ok();
    let m = existing.as_ref();
    let update = TagUpdate {
        title: Some(marker.into()),
        artist: m.and_then(|m| m.artist.clone()),
        album: m.and_then(|m| m.album.clone()),
        genre: m.and_then(|m| m.genre.clone()),
        year: m.and_then(|m| m.year),
        bpm: m.and_then(|m| m.bpm),
        key: m.and_then(|m| m.key.clone()),
        comment: m.and_then(|m| m.comment.clone()),
        label: m.and_then(|m| m.label.clone()),
        rating: None,
    };

    let tag_result = metadata::write(&copy, &update.normalized());
    println!("tag write result: {tag_result:?}");

    // 1x1 red PNG.
    let png: Vec<u8> = vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x18, 0xdd, 0x8d, 0xb0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let art_result = metadata::write_artwork(&copy, Some((png, MimeType::Png)));
    println!("artwork write result: {art_result:?}");

    // Read back from the physical file on the medium.
    let back = metadata::read(&copy).expect("read back test file");
    println!("read-back title: {:?}", back.title);
    println!("read-back has_artwork: {}", back.has_artwork);

    // The APIC description must be latin1 (encoding byte 0). Serato silently
    // ignores a picture frame whose description is a UTF-16 byte-order mark,
    // which is what the ID3v2.3 writer produces unless the frame is forced.
    let raw = std::fs::read(&copy).expect("read raw bytes");
    let apic_encoding = raw
        .windows(4)
        .position(|w| w == b"APIC")
        .map(|p| raw[p + 10]);
    println!("APIC text encoding byte: {apic_encoding:?} (0 = latin1, 1 = UTF-16)");

    // Serato reads a WAV's tag ONLY out of a lowercase `id3 ` chunk, and an
    // AIFF's only out of the uppercase `ID3 ` its own spec defines. lofty
    // writes `ID3 ` into both, so this is what the normalization must fix.
    let want: &[u8; 4] = if copy.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")) {
        b"id3 "
    } else {
        b"ID3 "
    };
    let big_endian = &raw[..4] == b"FORM";
    let mut ids: Vec<String> = Vec::new();
    let mut i = 12usize;
    while i + 8 <= raw.len() {
        let sz = [raw[i + 4], raw[i + 5], raw[i + 6], raw[i + 7]];
        let size = if big_endian {
            u32::from_be_bytes(sz)
        } else {
            u32::from_le_bytes(sz)
        } as usize;
        if raw[i..i + 4].eq_ignore_ascii_case(b"id3 ") {
            ids.push(String::from_utf8_lossy(&raw[i..i + 4]).into_owned());
        }
        i += 8 + size + (size % 2);
    }
    println!("ID3 chunk ids after the write: {ids:?} (want {:?})", String::from_utf8_lossy(want));

    let _ = std::fs::remove_file(&copy);

    // Only RIFF/AIFF have chunks; an MP3 carries its tag at the file start.
    if &raw[..4] == b"RIFF" || &raw[..4] == b"FORM" {
        assert_eq!(
            ids,
            vec![String::from_utf8_lossy(want).into_owned()],
            "the tag must sit in exactly one chunk, under the id its container calls for"
        );
    }

    assert!(tag_result.is_ok(), "tag write errored: {tag_result:?}");
    assert!(art_result.is_ok(), "artwork write errored: {art_result:?}");
    assert_eq!(
        back.title.as_deref(),
        Some(marker),
        "title did not persist to the physical file"
    );
    assert!(back.has_artwork, "artwork did not persist to the physical file");
    assert_ne!(
        apic_encoding,
        Some(1),
        "APIC description must never be UTF-16 — Serato ignores that frame"
    );
}
