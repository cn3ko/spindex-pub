//! Pure helpers for copying tracks between library folders. The rule that
//! matters: never overwrite or delete an existing audio file — an existing
//! destination is a skip, not a replace.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

const ILLEGAL: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

/// Validates a user-entered folder name; returns the trimmed name.
pub fn validate_folder_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        bail!("folder name is empty");
    }
    if let Some(c) = trimmed.chars().find(|c| ILLEGAL.contains(c)) {
        bail!("folder name may not contain '{c}'");
    }
    Ok(trimmed.to_string())
}

/// Destination for a flat copy: target root + the source's filename.
pub fn dest_path(target_root: &Path, source: &Path) -> Option<PathBuf> {
    source.file_name().map(|f| target_root.join(f))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyOutcome {
    Copied,
    SkippedExists,
    Failed,
}

/// Copies one file flat into `target_root`. Existing destinations are left
/// untouched (SkippedExists); any IO problem is Failed, never fatal.
pub fn copy_one(source: &Path, target_root: &Path) -> CopyOutcome {
    let Some(dest) = dest_path(target_root, source) else {
        return CopyOutcome::Failed;
    };
    if dest.exists() {
        return CopyOutcome::SkippedExists;
    }
    match std::fs::copy(source, &dest) {
        Ok(_) => CopyOutcome::Copied,
        Err(e) => {
            tracing::warn!("copy {} -> {}: {e}", source.display(), dest.display());
            // a partial destination from a failed copy must not linger
            let _ = std::fs::remove_file(&dest);
            CopyOutcome::Failed
        }
    }
}

/// Moves one file flat into `target_dir` (same-volume rename — crate moves
/// stay inside one library). Existing destinations are left untouched
/// (SkippedExists); the source is only ever renamed, never deleted.
pub fn move_one(source: &Path, target_dir: &Path) -> CopyOutcome {
    let Some(dest) = dest_path(target_dir, source) else {
        return CopyOutcome::Failed;
    };
    if dest.exists() {
        return CopyOutcome::SkippedExists;
    }
    match std::fs::rename(source, &dest) {
        Ok(()) => CopyOutcome::Copied,
        Err(e) => {
            tracing::warn!("move {} -> {}: {e}", source.display(), dest.display());
            CopyOutcome::Failed
        }
    }
}

/// Destination rel-path for a track when copying crate `folder` to another
/// library: the folder's PARENT prefix is stripped so the crate keeps its own
/// name at the target root ("House/Deep" + "House/Deep/Vocal/x.mp3" ->
/// "Deep/Vocal/x.mp3"). Tracks outside the crate's subtree -> None.
pub fn crate_dest_rel(folder: &str, rel_path: &str) -> Option<String> {
    let tail = rel_path.strip_prefix(folder)?.strip_prefix('/')?;
    let crate_name = folder.rsplit('/').next()?;
    Some(format!("{crate_name}/{tail}"))
}

/// Copies one file to an exact destination path, creating parent directories.
/// Existing destinations are left untouched (SkippedExists); any IO problem
/// is Failed, never fatal.
pub fn copy_to(source: &Path, dest_abs: &Path) -> CopyOutcome {
    if dest_abs.exists() {
        return CopyOutcome::SkippedExists;
    }
    if let Some(parent) = dest_abs.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return CopyOutcome::Failed;
        }
    }
    match std::fs::copy(source, dest_abs) {
        Ok(_) => CopyOutcome::Copied,
        Err(e) => {
            tracing::warn!("copy {} -> {}: {e}", source.display(), dest_abs.display());
            // a partial destination from a failed copy must not linger
            let _ = std::fs::remove_file(dest_abs);
            CopyOutcome::Failed
        }
    }
}

/// Validates a bare filename dropped in from the OS: no separators or other
/// illegal characters, no dot-names, and it must be an audio type we index.
/// The webview hands us `File.name` (already a basename), but never trust it.
pub fn validate_import_filename(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        bail!("file name is empty");
    }
    if trimmed == "." || trimmed == ".." {
        bail!("invalid file name");
    }
    if let Some(c) = trimmed.chars().find(|c| ILLEGAL.contains(c)) {
        bail!("file name may not contain '{c}'");
    }
    if !crate::core::metadata::is_audio_file(Path::new(trimmed)) {
        bail!("not a supported audio file type");
    }
    Ok(trimmed.to_string())
}

/// Writes OS-dropped file bytes to `<root>/<folder>/<filename>`. Existing
/// destinations are never overwritten (SkippedExists). The bytes go to a
/// temp name first and are renamed into place, so a failed write never
/// leaves a partial audio file under the real name.
pub fn import_bytes(root: &Path, folder: &str, filename: &str, bytes: &[u8]) -> Result<CopyOutcome> {
    let name = validate_import_filename(filename)?;
    let dir = if folder.is_empty() {
        root.to_path_buf()
    } else {
        root.join(folder)
    };
    let dest = dir.join(&name);
    if dest.exists() {
        return Ok(CopyOutcome::SkippedExists);
    }
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("{name}.spindex-partial"));
    if let Err(e) = std::fs::write(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(CopyOutcome::Copied)
}

/// Decodes a JS `encodeURIComponent` string (percent-escaped UTF-8). Invoke
/// headers are ASCII-only, so non-ASCII filenames cross the IPC boundary
/// encoded; this is the matching decoder. Malformed escapes are an error,
/// never a silent pass-through.
pub fn percent_decode(s: &str) -> Result<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            match hex {
                Some(b) => {
                    out.push(b);
                    i += 3;
                }
                None => bail!("malformed percent escape"),
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| anyhow::anyhow!("percent escape is not valid UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spindex-copy-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn folder_name_validation() {
        assert_eq!(validate_folder_name("  House 2026  ").unwrap(), "House 2026");
        assert!(validate_folder_name("").is_err());
        assert!(validate_folder_name("   ").is_err());
        for c in ['\\', '/', ':', '*', '?', '"', '<', '>', '|'] {
            assert!(validate_folder_name(&format!("bad{c}name")).is_err(), "{c}");
        }
    }

    #[test]
    fn dest_is_flat_filename_in_root() {
        let d = dest_path(Path::new("D:/target"), Path::new("C:/music/sub/track.mp3")).unwrap();
        assert_eq!(d, Path::new("D:/target").join("track.mp3"));
        assert!(dest_path(Path::new("D:/t"), Path::new("/")).is_none());
    }

    #[test]
    fn copy_skips_existing_and_reports_missing() {
        let src_dir = tmp("src");
        let dst_dir = tmp("dst");
        let src = src_dir.join("a.mp3");
        std::fs::write(&src, b"audio-bytes").unwrap();

        assert_eq!(copy_one(&src, &dst_dir), CopyOutcome::Copied);
        assert_eq!(std::fs::read(dst_dir.join("a.mp3")).unwrap(), b"audio-bytes");

        // second copy: destination exists -> skipped, content untouched
        std::fs::write(&src, b"changed").unwrap();
        assert_eq!(copy_one(&src, &dst_dir), CopyOutcome::SkippedExists);
        assert_eq!(std::fs::read(dst_dir.join("a.mp3")).unwrap(), b"audio-bytes");

        // missing source -> failed
        assert_eq!(copy_one(&src_dir.join("nope.mp3"), &dst_dir), CopyOutcome::Failed);

        let _ = std::fs::remove_dir_all(&src_dir);
        let _ = std::fs::remove_dir_all(&dst_dir);
    }

    #[test]
    fn move_renames_and_skips_existing() {
        let dir = tmp("move");
        let sub = dir.join("crate");
        std::fs::create_dir_all(&sub).unwrap();
        let src = dir.join("a.mp3");
        std::fs::write(&src, b"bytes").unwrap();

        assert_eq!(move_one(&src, &sub), CopyOutcome::Copied);
        assert!(!src.exists(), "source is gone after a move");
        assert_eq!(std::fs::read(sub.join("a.mp3")).unwrap(), b"bytes");

        // a same-named file at the destination blocks the move; source stays
        std::fs::write(&src, b"other").unwrap();
        assert_eq!(move_one(&src, &sub), CopyOutcome::SkippedExists);
        assert!(src.exists(), "skipped move leaves the source in place");
        assert_eq!(std::fs::read(sub.join("a.mp3")).unwrap(), b"bytes");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crate_dest_rel_keeps_crate_name_and_subtree() {
        // nested crate: parent prefix stripped, crate's own name kept
        assert_eq!(
            crate_dest_rel("House/Deep", "House/Deep/Vocal/x.mp3").as_deref(),
            Some("Deep/Vocal/x.mp3")
        );
        assert_eq!(
            crate_dest_rel("House/Deep", "House/Deep/y.mp3").as_deref(),
            Some("Deep/y.mp3")
        );
        // root-level crate maps to itself
        assert_eq!(
            crate_dest_rel("Deep", "Deep/z.mp3").as_deref(),
            Some("Deep/z.mp3")
        );
        // outside the folder -> None; sibling with the folder as prefix -> None
        assert_eq!(crate_dest_rel("House/Deep", "House/other.mp3"), None);
        assert_eq!(crate_dest_rel("House", "House Party/x.mp3"), None);
        // a track directly named like the folder (no slash after) -> None
        assert_eq!(crate_dest_rel("House", "House"), None);
    }

    #[test]
    fn import_filename_validation() {
        assert_eq!(
            validate_import_filename("  Track.mp3 ").unwrap(),
            "Track.mp3"
        );
        assert_eq!(validate_import_filename("Üñïçødé.aiff").unwrap(), "Üñïçødé.aiff");
        assert!(validate_import_filename("").is_err());
        assert!(validate_import_filename("..").is_err());
        assert!(validate_import_filename("../escape.mp3").is_err());
        assert!(validate_import_filename("a\\b.mp3").is_err());
        assert!(validate_import_filename("C:evil.mp3").is_err());
        assert!(validate_import_filename("notes.txt").is_err(), "non-audio rejected");
        assert!(validate_import_filename("no-extension").is_err());
    }

    #[test]
    fn import_writes_creates_folder_and_never_overwrites() {
        let root = tmp("import");

        // into a crate folder that doesn't exist yet
        let r = import_bytes(&root, "House/Deep", "a.mp3", b"first").unwrap();
        assert_eq!(r, CopyOutcome::Copied);
        assert_eq!(std::fs::read(root.join("House/Deep/a.mp3")).unwrap(), b"first");

        // same name again: skip, existing content untouched
        let r = import_bytes(&root, "House/Deep", "a.mp3", b"second").unwrap();
        assert_eq!(r, CopyOutcome::SkippedExists);
        assert_eq!(std::fs::read(root.join("House/Deep/a.mp3")).unwrap(), b"first");

        // empty folder = library root
        let r = import_bytes(&root, "", "b.flac", b"root-file").unwrap();
        assert_eq!(r, CopyOutcome::Copied);
        assert_eq!(std::fs::read(root.join("b.flac")).unwrap(), b"root-file");

        // no stray partial files anywhere
        let leftovers: Vec<_> = walk_files(&root)
            .into_iter()
            .filter(|p| p.to_string_lossy().contains("spindex-partial"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        // bad filename never touches the disk
        assert!(import_bytes(&root, "", "../up.mp3", b"x").is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    fn walk_files(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    out.extend(walk_files(&p));
                } else {
                    out.push(p);
                }
            }
        }
        out
    }

    #[test]
    fn percent_decode_roundtrips_utf8() {
        assert_eq!(percent_decode("plain.mp3").unwrap(), "plain.mp3");
        // JS: encodeURIComponent("Üñïçødé Ärtist.aiff")
        assert_eq!(
            percent_decode("%C3%9C%C3%B1%C3%AF%C3%A7%C3%B8d%C3%A9%20%C3%84rtist.aiff").unwrap(),
            "Üñïçødé Ärtist.aiff"
        );
        assert_eq!(percent_decode("House%2FDeep").unwrap(), "House/Deep");
        assert!(percent_decode("bad%2").is_err());
        assert!(percent_decode("bad%zz").is_err());
        assert!(percent_decode("%ff%fe").is_err(), "invalid UTF-8 rejected");
    }

    #[test]
    fn copy_to_creates_parents_and_skips_existing() {
        let dir = tmp("copyto");
        let src = dir.join("a.mp3");
        std::fs::write(&src, b"bytes").unwrap();
        let dest = dir.join("out").join("Deep").join("Vocal").join("a.mp3");

        assert_eq!(copy_to(&src, &dest), CopyOutcome::Copied);
        assert_eq!(std::fs::read(&dest).unwrap(), b"bytes");

        // second run: destination exists -> skipped, content untouched
        std::fs::write(&src, b"changed").unwrap();
        assert_eq!(copy_to(&src, &dest), CopyOutcome::SkippedExists);
        assert_eq!(std::fs::read(&dest).unwrap(), b"bytes");

        // missing source -> failed
        assert_eq!(copy_to(&dir.join("nope.mp3"), &dir.join("out2").join("n.mp3")), CopyOutcome::Failed);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
