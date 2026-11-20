//! ML restoration planning (SP3): pure decisions — Apollo sidecar discovery
//! and readiness, `PROGRESS n/m` parsing, and dest/tmp/backup/encode-arg
//! derivation for the restored output. No I/O beyond readiness checks; the
//! worker in commands.rs executes the plan. Reuses SP2's backup/swap
//! primitives and encoder table from `core::enhance`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::core::analysis;
use crate::core::enhance::{self, EnhanceMode, OutputFormat};

pub struct RestoreJob {
    /// ffmpeg args: input 0 = the restored wav, input 1 = the ORIGINAL file
    /// (metadata + artwork source), output = tmp_path.
    pub encode_args: Vec<OsString>,
    pub tmp_path: PathBuf,
    pub dest_path: PathBuf,
    pub backup_path: Option<PathBuf>,
    pub new_rel_path: Option<String>,
}

pub enum RestorePlan {
    Unsupported(String),
    Job(Box<RestoreJob>),
}

/// The Apollo directory, searched across the same locations as every other
/// component: `binaries/apollo/` in dev, `%APPDATA%\app.spindex.desktop\bin\
/// apollo` for an installed app. It used to be derived from ffmpeg's own
/// parent directory, which broke once ffmpeg stopped being bundled and could
/// legitimately sit somewhere else than Apollo.
pub fn find_apollo_dir() -> Option<PathBuf> {
    analysis::find_component_dir("apollo")
}

fn nonzero_file(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_file() && m.len() > 0).unwrap_or(false)
}

/// Everything `run_restore.py` needs at runtime, produced by
/// `scripts/setup-apollo.ps1`. Gates all Restore UI.
pub fn apollo_ready(dir: &Path) -> bool {
    nonzero_file(&dir.join("venv").join("Scripts").join("python.exe"))
        && nonzero_file(&dir.join("run_restore.py"))
        && nonzero_file(&dir.join("pytorch_model.bin"))
        && dir.join("Apollo").join("look2hear").is_dir()
}

/// Parses a `PROGRESS <n>/<m>` stdout line from run_restore.py.
pub fn parse_progress(line: &str) -> Option<(u64, u64)> {
    let rest = line.strip_prefix("PROGRESS ")?;
    let (n, m) = rest.trim().split_once('/')?;
    Some((n.parse().ok()?, m.parse().ok()?))
}

pub fn plan_restoration(
    library_root: &Path,
    rel_path: &str,
    source_format: &str,
    bit_depth: Option<u8>,
    mode: EnhanceMode,
    out: OutputFormat,
    // NewFile only: save into this folder instead of beside the source.
    dest_dir: Option<&Path>,
    restored_wav: &Path,
) -> RestorePlan {
    let Some((enc, forced_ext)) = enhance::encoder_args(source_format, bit_depth, out) else {
        return RestorePlan::Unsupported(format!("unsupported format: {source_format}"));
    };

    let src_abs = {
        let mut p = library_root.to_path_buf();
        for seg in rel_path.split('/') {
            p.push(seg);
        }
        p
    };
    let src_ext = src_abs.extension().map(|e| e.to_string_lossy().to_string());
    let out_ext = forced_ext
        .map(str::to_string)
        .or(src_ext)
        .unwrap_or_else(|| source_format.to_string());
    let stem = src_abs
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let dir = src_abs.parent().unwrap_or(library_root).to_path_buf();

    let (dest_path, backup_path, new_rel_path) = match mode {
        EnhanceMode::NewFile => (
            dest_dir
                .unwrap_or(&dir)
                .join(format!("{stem} (Restored).{out_ext}")),
            None,
            None,
        ),
        EnhanceMode::Replace => {
            let dest = dir.join(format!("{stem}.{out_ext}"));
            let new_rel = if forced_ext.is_some() && dest != src_abs {
                let dot = rel_path.rfind('.').unwrap_or(rel_path.len());
                Some(format!("{}.{out_ext}", &rel_path[..dot]))
            } else {
                None
            };
            (dest, Some(enhance::backup_path_for(library_root, rel_path)), new_rel)
        }
    };
    let tmp_path = dest_path
        .parent()
        .unwrap_or(&dir)
        .join(format!("{stem}.spindex-tmp.{out_ext}"));

    let mut args: Vec<OsString> = vec![
        "-v".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        restored_wav.as_os_str().to_owned(),
        "-i".into(),
        src_abs.as_os_str().to_owned(),
        "-map".into(),
        "0:a".into(),
        "-map".into(),
        "1:v?".into(),
        "-c:v".into(),
        "copy".into(),
        "-map_metadata".into(),
        "1".into(),
    ];
    args.extend(enc.iter().map(|s| OsString::from(*s)));
    args.push(tmp_path.as_os_str().to_owned());

    RestorePlan::Job(Box::new(RestoreJob {
        encode_args: args,
        tmp_path,
        dest_path,
        backup_path,
        new_rel_path,
    }))
}

/// Drops the artwork mapping (`-map 1:v?` and `-c:v copy`) for the retry
/// path: wav/aiff muxers refuse video streams, and some sources make the
/// optional map fail anyway. Metadata mapping is kept.
pub fn strip_artwork_maps(args: &[OsString]) -> Vec<OsString> {
    let mut out: Vec<OsString> = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let a = args[i].to_string_lossy();
        let next = args.get(i + 1).map(|n| n.to_string_lossy().to_string());
        if (a == "-map" && next.as_deref() == Some("1:v?"))
            || (a == "-c:v" && next.as_deref() == Some("copy"))
        {
            i += 2;
            continue;
        }
        out.push(args[i].clone());
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::enhance::{EnhanceMode, OutputFormat};
    use std::path::Path;

    fn args_str(job: &RestoreJob) -> Vec<String> {
        job.encode_args.iter().map(|a| a.to_string_lossy().to_string()).collect()
    }

    #[test]
    fn parse_progress_lines() {
        assert_eq!(parse_progress("PROGRESS 3/12"), Some((3, 12)));
        assert_eq!(parse_progress("PROGRESS 0/1"), Some((0, 1)));
        assert_eq!(parse_progress("Model loaded in 30s"), None);
        assert_eq!(parse_progress("PROGRESS x/y"), None);
        assert_eq!(parse_progress("PROGRESS 3"), None);
    }

    #[test]
    fn new_file_naming_and_encode_args() {
        let RestorePlan::Job(job) = plan_restoration(
            Path::new("C:/lib"), "House/track.mp3", "mp3", None,
            EnhanceMode::NewFile, OutputFormat::Keep, None,
            Path::new("C:/tmp/restored.wav"),
        ) else { panic!() };
        assert_eq!(job.dest_path, Path::new("C:/lib/House/track (Restored).mp3"));
        assert!(job.backup_path.is_none());
        assert!(job.new_rel_path.is_none());
        let a = args_str(&job);
        // restored wav is input 0, the ORIGINAL is input 1 (metadata + artwork source)
        let i0 = a.iter().position(|s| s == "-i").unwrap();
        assert_eq!(a[i0 + 1], "C:/tmp/restored.wav");
        assert!(a.windows(2).any(|w| w == ["-map", "0:a"]));
        assert!(a.windows(2).any(|w| w == ["-map", "1:v?"]));
        assert!(a.windows(2).any(|w| w == ["-c:v", "copy"]));
        assert!(a.windows(2).any(|w| w == ["-map_metadata", "1"]));
        assert!(a.windows(2).any(|w| w == ["-c:a", "libmp3lame"]));
        let tmp = job.tmp_path.to_string_lossy().to_string();
        assert!(tmp.contains(".spindex-tmp."), "{tmp}");
    }

    #[test]
    fn replace_flac_out_changes_rel_path_and_backs_up() {
        let RestorePlan::Job(job) = plan_restoration(
            Path::new("C:/lib"), "House/track.mp3", "mp3", None,
            EnhanceMode::Replace, OutputFormat::Flac, None,
            Path::new("C:/tmp/restored.wav"),
        ) else { panic!() };
        assert_eq!(job.dest_path, Path::new("C:/lib/House/track.flac"));
        assert_eq!(job.new_rel_path.as_deref(), Some("House/track.flac"));
        assert_eq!(
            job.backup_path.as_deref(),
            Some(Path::new("C:/lib/_SpinDexBackups_/House/track.mp3"))
        );
        assert!(args_str(&job).windows(2).any(|w| w == ["-c:a", "flac"]));
    }

    #[test]
    fn new_file_dest_dir_override() {
        let RestorePlan::Job(job) = plan_restoration(
            Path::new("C:/lib"), "House/track.mp3", "mp3", None,
            EnhanceMode::NewFile, OutputFormat::Keep, Some(Path::new("E:/out")),
            Path::new("C:/tmp/r.wav"),
        ) else { panic!() };
        assert_eq!(job.dest_path, Path::new("E:/out/track (Restored).mp3"));
    }

    #[test]
    fn unsupported_source_format() {
        let p = plan_restoration(
            Path::new("C:/lib"), "t.ogg", "ogg", None,
            EnhanceMode::NewFile, OutputFormat::Keep, None, Path::new("C:/r.wav"),
        );
        assert!(matches!(p, RestorePlan::Unsupported(_)));
    }

    #[test]
    fn strip_artwork_maps_removes_video_mapping() {
        let args: Vec<std::ffi::OsString> =
            ["-i", "r.wav", "-i", "s.mp3", "-map", "0:a", "-map", "1:v?", "-c:v", "copy",
             "-map_metadata", "1", "-c:a", "flac", "out.flac"]
            .iter().map(|s| s.into()).collect();
        let out: Vec<String> = strip_artwork_maps(&args)
            .iter().map(|a| a.to_string_lossy().to_string()).collect();
        assert!(!out.windows(2).any(|w| w == ["-map", "1:v?"]));
        assert!(!out.windows(2).any(|w| w == ["-c:v", "copy"]));
        assert!(out.windows(2).any(|w| w == ["-map", "0:a"]), "audio map untouched");
        assert!(out.windows(2).any(|w| w == ["-map_metadata", "1"]), "metadata map untouched");
    }

    #[test]
    fn apollo_ready_requires_all_pieces() {
        let d = std::env::temp_dir().join(format!("spindex-apollo-rdy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("venv/Scripts")).unwrap();
        assert!(!apollo_ready(&d), "empty dir not ready");
        std::fs::write(d.join("venv/Scripts/python.exe"), b"x").unwrap();
        std::fs::write(d.join("run_restore.py"), b"x").unwrap();
        std::fs::write(d.join("pytorch_model.bin"), b"x").unwrap();
        assert!(!apollo_ready(&d), "Apollo code dir still missing");
        std::fs::create_dir_all(d.join("Apollo/look2hear")).unwrap();
        assert!(apollo_ready(&d));
        // zero-byte weights = not ready (mirrors find_binary's zero-byte guard)
        std::fs::write(d.join("pytorch_model.bin"), b"").unwrap();
        assert!(!apollo_ready(&d));
        let _ = std::fs::remove_dir_all(&d);
    }
}
