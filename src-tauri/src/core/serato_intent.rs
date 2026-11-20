//! Per-crate "intended membership" sidecar for Serato sync. Intent is the set
//! of stick-relative paths (the form Serato stores in `ptrk`) SpinDex believes
//! each crate should hold, persisted as one JSON on the stick so it survives a
//! SpinDex DB rebuild and travels with the stick. Serato only ever reads/writes
//! `.crate` files, so a sibling JSON is safe.

use std::collections::BTreeMap;
use std::path::Path;

/// Crate file name (e.g. "House.crate") -> its intended stick-relative paths.
pub type Intent = BTreeMap<String, Vec<String>>;

/// Reads the sidecar. A missing or corrupt file yields an empty map, never an
/// error — absent intent degrades to "Serato wins".
pub fn read_intent(path: &Path) -> Intent {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Intent::new(),
    }
}

/// Writes the sidecar atomically (parent dir created, tmp-write + rename).
pub fn write_intent(path: &Path, map: &Intent) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(map).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.spindex-tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)
}

/// Two-way difference between intended membership and the crate's actual
/// contents, matched case-insensitively (FAT/NTFS sticks ignore case).
#[derive(Debug, PartialEq)]
pub struct Diff {
    /// intended, but absent from the `.crate` (Serato likely overwrote them)
    pub spindex_only: Vec<String>,
    /// present in the `.crate`, but not intended (adopt or drop the link)
    pub serato_only: Vec<String>,
}

pub fn diff(intent: &[String], crate_paths: &[String]) -> Diff {
    let intent_lc: Vec<String> = intent.iter().map(|s| s.to_lowercase()).collect();
    let crate_lc: Vec<String> = crate_paths.iter().map(|s| s.to_lowercase()).collect();
    let spindex_only = intent
        .iter()
        .filter(|p| !crate_lc.contains(&p.to_lowercase()))
        .cloned()
        .collect();
    let serato_only = crate_paths
        .iter()
        .filter(|p| !intent_lc.contains(&p.to_lowercase()))
        .cloned()
        .collect();
    Diff { spindex_only, serato_only }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_missing_file_is_empty() {
        let p = std::env::temp_dir().join("spindex-intent-missing-xyz.json");
        let _ = std::fs::remove_file(&p);
        assert!(read_intent(&p).is_empty());
    }

    #[test]
    fn read_corrupt_file_is_empty() {
        let p = std::env::temp_dir()
            .join(format!("spindex-intent-corrupt-{}.json", std::process::id()));
        std::fs::write(&p, b"not json{{").unwrap();
        assert!(read_intent(&p).is_empty());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = std::env::temp_dir()
            .join(format!("spindex-intent-rt-{}", std::process::id()));
        let p = dir.join("nested").join("spindex-intent.json");
        let mut m = Intent::new();
        m.insert("House.crate".into(), vec!["MP3/a.mp3".into(), "AIFF/b.aiff".into()]);
        write_intent(&p, &m).unwrap(); // creates parent dirs
        assert_eq!(read_intent(&p), m);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_three_buckets_case_insensitive() {
        // intent has a+b, crate has B (diff case) + c
        let intent = vec!["MP3/a.mp3".to_string(), "MP3/b.mp3".to_string()];
        let crate_paths = vec!["mp3/B.MP3".to_string(), "MP3/c.mp3".to_string()];
        let d = diff(&intent, &crate_paths);
        assert_eq!(d.spindex_only, vec!["MP3/a.mp3".to_string()]); // b matches B case-insensitively
        assert_eq!(d.serato_only, vec!["MP3/c.mp3".to_string()]);
    }

    #[test]
    fn diff_empty_sides() {
        assert_eq!(
            diff(&[], &["x".to_string()]),
            Diff { spindex_only: vec![], serato_only: vec!["x".to_string()] }
        );
        assert_eq!(
            diff(&["y".to_string()], &[]),
            Diff { spindex_only: vec!["y".to_string()], serato_only: vec![] }
        );
    }
}
