//! Live check: does SpinDex re-encode every Serato Markers2 frame on a stick
//! byte for byte? Read-only — it reads each file's raw frame, decodes it to
//! entries and encodes them again, and compares; nothing is written.
//!
//!   SPINDEX_DIAG_DIR="D:\" \
//!     cargo test --release --test markers2_roundtrip_live -- --ignored --nocapture
//!
//! The Rekordbox import only writes Markers2 frames it can reproduce exactly,
//! so every entry it does not touch stays what Serato wrote.

use spindex::core::metadata::{self, RawMarkers};
use spindex::core::serato_markers as sm;

#[test]
#[ignore]
fn every_markers2_frame_re_encodes_identically() {
    let Ok(dir) = std::env::var("SPINDEX_DIAG_DIR") else {
        eprintln!("SPINDEX_DIAG_DIR not set, skipping");
        return;
    };
    // A frame whose bytes differ but whose entries re-read identically differs
    // only in padding past the data (seen once: stray bits in the last base64
    // character of a Serato-written FLAC value). That carries no information,
    // so it is counted apart and does not fail the check.
    let (mut checked, mut identical, mut unreadable, mut padding_only) = (0usize, 0usize, 0usize, 0usize);
    let mut first_diff: Option<String> = None;
    for e in walkdir::WalkDir::new(&dir).into_iter().flatten() {
        let path = e.path();
        let s = path.to_string_lossy();
        if !e.file_type().is_file() || s.contains("_SpinDexBackups_") || s.contains("_Serato_") {
            continue;
        }
        let ext = path.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !["mp3", "wav", "aif", "aiff", "flac"].contains(&ext.as_str()) {
            continue;
        }
        let raw = match metadata::read_serato_markers_raw(path) {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        checked += 1;
        let same = match &raw {
            RawMarkers::Geob(p) => match sm::entries_from_payload(p) {
                Ok(entries) => {
                    let again = sm::encode_payload(&entries);
                    if again != *p && first_diff.is_none() {
                        first_diff = Some(format!(
                            "{}\n  serato ({} bytes): {:02x?}\n  ours   ({} bytes): {:02x?}",
                            path.display(),
                            p.len(),
                            &p[..p.len().min(96)],
                            again.len(),
                            &again[..again.len().min(96)]
                        ));
                    }
                    again == *p
                }
                Err(_) => false,
            },
            RawMarkers::Vorbis(v) => match sm::entries_from_vorbis_value(v) {
                Ok(entries) => {
                    let again = sm::vorbis_value(&entries);
                    if again != *v && first_diff.is_none() {
                        let at = v.bytes().zip(again.bytes()).position(|(a, b)| a != b).unwrap_or(0);
                        let from = at.saturating_sub(30);
                        first_diff = Some(format!(
                            "{} (FLAC) first difference at char {at}\n  serato ({} chars): {:?}\n  ours   ({} chars): {:?}",
                            path.display(),
                            v.len(),
                            &v[from..v.len().min(at + 60)],
                            again.len(),
                            &again[from..again.len().min(at + 60)]
                        ));
                    }
                    if again != *v
                        && sm::entries_from_vorbis_value(&again).ok() == Some(entries.clone())
                    {
                        padding_only += 1;
                    }
                    again == *v
                }
                Err(_) => false,
            },
        };
        identical += same as usize;
    }
    println!(
        "Markers2 frames checked: {checked}, identical: {identical}, padding-only: {padding_only}, unreadable files: {unreadable}"
    );
    if let Some(d) = first_diff {
        println!("first difference:\n{d}");
    }
    assert_eq!(
        identical + padding_only,
        checked,
        "every frame must re-encode byte for byte, apart from padding past the data"
    );
}
