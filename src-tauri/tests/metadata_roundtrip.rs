//! Tag write/read round-trip on a real audio file. Uses the bundled ffmpeg
//! to generate a small flac; skips silently when ffmpeg isn't fetched yet.

use std::process::Command;

use spindex::core::{analysis, metadata};

fn make_flac(path: &std::path::Path) -> bool {
    let Some(ffmpeg) = analysis::find_ffmpeg() else {
        eprintln!("skipping metadata roundtrip: ffmpeg not fetched");
        return false;
    };
    let status = Command::new(ffmpeg)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
        .arg(path)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed to generate fixture flac");
    true
}

#[test]
fn tag_write_read_roundtrip() {
    let dir = std::env::temp_dir().join(format!("spindex-meta-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let flac = dir.join("roundtrip.flac");
    if !make_flac(&flac) {
        return;
    }

    let update = metadata::TagUpdate {
        title: Some("Test Title".into()),
        artist: Some("Test Artist".into()),
        album: Some("Test Album".into()),
        genre: Some("Techno".into()),
        year: Some(2026),
        bpm: Some(128.0),
        key: Some("8A".into()),
        comment: Some("  edited by test  ".into()),
        label: Some("Transmat".into()),
        rating: None,
    }
    .normalized();
    metadata::write(&flac, &update).expect("write tags");

    let read = metadata::read(&flac).expect("read tags back");
    assert_eq!(read.title.as_deref(), Some("Test Title"));
    assert_eq!(read.artist.as_deref(), Some("Test Artist"));
    assert_eq!(read.album.as_deref(), Some("Test Album"));
    assert_eq!(read.genre.as_deref(), Some("Techno"));
    assert_eq!(read.year, Some(2026));
    assert_eq!(read.bpm, Some(128.0));
    assert_eq!(read.key.as_deref(), Some("8A"));
    assert_eq!(read.comment.as_deref(), Some("edited by test"));
    // label rides the Vorbis comment path, not ID3v2 — cover it on a real flac
    assert_eq!(read.label.as_deref(), Some("Transmat"));

    // clearing a field removes it from the file
    let cleared = metadata::TagUpdate {
        comment: Some("   ".into()),
        ..update
    }
    .normalized();
    metadata::write(&flac, &cleared).expect("clear comment");
    let read = metadata::read(&flac).expect("re-read");
    assert_eq!(read.comment, None);
    assert_eq!(read.title.as_deref(), Some("Test Title"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn artwork_write_read_remove_roundtrip() {
    let dir = std::env::temp_dir().join(format!("spindex-art-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let flac = dir.join("art.flac");
    if !make_flac(&flac) {
        return;
    }

    assert_eq!(metadata::read_artwork(&flac).unwrap(), None, "starts bare");

    let fake_png = vec![0x89, 0x50, 0x4E, 0x47, 1, 2, 3, 4, 5];
    metadata::write_artwork(&flac, Some((fake_png.clone(), lofty::picture::MimeType::Png)))
        .expect("embed artwork");
    let (mime, data) = metadata::read_artwork(&flac).unwrap().expect("artwork back");
    assert!(mime.contains("png"), "mime was {mime}");
    assert_eq!(data, fake_png);

    // replacing goes through the same call; removing clears everything
    metadata::write_artwork(&flac, None).expect("remove artwork");
    assert_eq!(metadata::read_artwork(&flac).unwrap(), None);

    let _ = std::fs::remove_dir_all(&dir);
}
