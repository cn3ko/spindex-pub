//! Diagnostic: does a key written by SpinDex read back from the same file?
//! Ignored by default. Works on a COPY, never the original.
//!
//!   SPINDEX_KEY_FILES="D:\a.wav;D:\b.aiff" \
//!     cargo test --test key_roundtrip_live -- --ignored --nocapture
//!
//! A WAV in the wild can carry TWO ID3 chunks (`ID3 ` and `id3 `). If the
//! writer updates one and the reader prefers the other, every key edit looks
//! like it saved and then silently reverts on the next rescan.
use spindex::core::metadata::{self, TagUpdate};

#[test]
#[ignore]
fn a_written_key_reads_back() {
    let Ok(list) = std::env::var("SPINDEX_KEY_FILES") else {
        panic!("set SPINDEX_KEY_FILES to one or more paths separated by ';'");
    };
    let dir = std::env::temp_dir().join(format!("spindex-keyrt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut failures = 0;
    for path in list.split(';').filter(|s| !s.trim().is_empty()) {
        let src = std::path::Path::new(path.trim());
        // SPINDEX_KEY_INPLACE=1 edits the given file itself — only ever point
        // it at a scratch copy you made yourself.
        let copy = if std::env::var("SPINDEX_KEY_INPLACE").is_ok() {
            src.to_path_buf()
        } else {
            let c = dir.join(src.file_name().unwrap());
            std::fs::copy(src, &c).unwrap();
            c
        };

        let before = metadata::read(&copy).map(|m| m.key).unwrap_or(None);
        let update = TagUpdate {
            title: None,
            artist: None,
            album: None,
            genre: None,
            year: None,
            bpm: None,
            key: Some("1A".into()),
            comment: None,
            label: None,
            rating: None,
        };
        let wrote = metadata::apply_partial(&copy, &update, &["key".to_string()]);
        let after = metadata::read(&copy).map(|m| m.key).unwrap_or(None);
        let ok = after.as_deref() == Some("1A");
        if !ok {
            failures += 1;
        }
        println!(
            "{} before={:?} wrote={:?} after={:?}  <- {}",
            if ok { "ROUND-TRIPS" } else { "REVERTS    " },
            before,
            wrote.is_ok(),
            after,
            src.file_name().unwrap().to_string_lossy()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(failures, 0, "{failures} file(s) did not read back the key written to them");
}
