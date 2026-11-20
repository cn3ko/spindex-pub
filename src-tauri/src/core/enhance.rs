//! Loudness enhancement planning (SP2): pure decisions — gain from measured
//! LUFS toward the fixed club target, skip rule, destination/backup/tmp path
//! derivation, and the exact ffmpeg argument vector. No I/O here; the worker
//! in commands.rs executes the plan. Raise-only by design: enhancement never
//! attenuates and never re-limits already-loud masters.

use anyhow::{bail, Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const TARGET_LUFS: f64 = -9.0;
pub const MIN_GAIN_DB: f64 = 0.25;
pub const BACKUP_DIR: &str = "_SpinDexBackups_";

#[derive(Clone, Copy, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnhanceMode {
    NewFile,
    Replace,
}

#[derive(Clone, Copy, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    Keep,
    Flac,
}

pub struct EnhanceJob {
    pub gain_db: f64,
    pub ffmpeg_args: Vec<OsString>,
    pub tmp_path: PathBuf,
    pub dest_path: PathBuf,
    pub backup_path: Option<PathBuf>,
    pub new_rel_path: Option<String>,
}

pub enum EnhancePlan {
    AlreadyLoudEnough,
    Unsupported(String),
    Job(Box<EnhanceJob>),
}

pub fn backup_path_for(library_root: &Path, rel_path: &str) -> PathBuf {
    let mut p = library_root.join(BACKUP_DIR);
    for seg in rel_path.split('/') {
        p.push(seg);
    }
    p
}

/// Encoder args for (source format, output choice). None = unsupported source.
pub(crate) fn encoder_args(
    source_format: &str,
    bit_depth: Option<u8>,
    out: OutputFormat,
) -> Option<(Vec<&'static str>, Option<&'static str>)> {
    // returns (args, forced extension for Flac out)
    if out == OutputFormat::Flac {
        return Some((vec!["-c:a", "flac"], Some("flac")));
    }
    let depth = bit_depth.unwrap_or(24);
    let args = match source_format.to_ascii_lowercase().as_str() {
        "mp3" => vec!["-c:a", "libmp3lame", "-b:a", "320k"],
        "m4a" | "aac" => vec!["-c:a", "aac", "-b:a", "256k"],
        "flac" => vec!["-c:a", "flac"],
        "wav" => vec!["-c:a", if depth <= 16 { "pcm_s16le" } else { "pcm_s24le" }],
        "aiff" | "aif" => vec!["-c:a", if depth <= 16 { "pcm_s16be" } else { "pcm_s24be" }],
        _ => return None,
    };
    Some((args, None))
}

/// Suffix appended to the stem in NewFile mode, per writer.
pub const ENHANCED_SUFFIX: &str = " (Enhanced)";
pub const STEADY_SUFFIX: &str = " (Steady)";

/// Where one output file goes, plus the encoder that produces it.
pub struct OutputPaths {
    pub src_abs: PathBuf,
    pub dest_path: PathBuf,
    pub tmp_path: PathBuf,
    /// Replace mode only — NewFile leaves the original untouched.
    pub backup_path: Option<PathBuf>,
    /// Set only when the output extension differs from the source's, i.e. a
    /// Replace that moves the track to a new file name.
    pub new_rel_path: Option<String>,
    pub encoder: Vec<&'static str>,
}

/// Destination/backup/tmp layout shared by every writer that produces an audio
/// file from an existing track (enhance, ML restore, tempo stabilize). One home
/// so the tmp-then-atomic-rename and backup conventions cannot drift apart per
/// feature; the writers differ only in `suffix`. None = unsupported source.
pub fn plan_paths(
    library_root: &Path,
    rel_path: &str,
    source_format: &str,
    bit_depth: Option<u8>,
    mode: EnhanceMode,
    out: OutputFormat,
    // NewFile only: save into this folder instead of beside the source.
    dest_dir: Option<&Path>,
    suffix: &str,
) -> Option<OutputPaths> {
    let (encoder, forced_ext) = encoder_args(source_format, bit_depth, out)?;

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
            dest_dir.unwrap_or(&dir).join(format!("{stem}{suffix}.{out_ext}")),
            None,
            None,
        ),
        EnhanceMode::Replace => {
            let dest = dir.join(format!("{stem}.{out_ext}"));
            let new_rel = if forced_ext.is_some() && dest != src_abs {
                // extension changed: derive the new rel_path from the old one
                let dot = rel_path.rfind('.').unwrap_or(rel_path.len());
                Some(format!("{}.{out_ext}", &rel_path[..dot]))
            } else {
                None
            };
            (dest, Some(backup_path_for(library_root, rel_path)), new_rel)
        }
    };
    let tmp_path = dest_path
        .parent()
        .unwrap_or(&dir)
        .join(format!("{stem}.spindex-tmp.{out_ext}"));

    Some(OutputPaths { src_abs, dest_path, tmp_path, backup_path, new_rel_path, encoder })
}

pub fn plan_enhancement(
    library_root: &Path,
    rel_path: &str,
    source_format: &str,
    bit_depth: Option<u8>,
    measured_lufs: f64,
    mode: EnhanceMode,
    out: OutputFormat,
    // NewFile only: save into this folder instead of beside the source.
    dest_dir: Option<&Path>,
) -> EnhancePlan {
    let gain_db = TARGET_LUFS - measured_lufs;
    if gain_db <= MIN_GAIN_DB {
        return EnhancePlan::AlreadyLoudEnough;
    }
    let Some(paths) = plan_paths(
        library_root, rel_path, source_format, bit_depth, mode, out, dest_dir, ENHANCED_SUFFIX,
    ) else {
        return EnhancePlan::Unsupported(format!("unsupported format: {source_format}"));
    };
    let OutputPaths { src_abs, dest_path, tmp_path, backup_path, new_rel_path, encoder } = paths;

    let mut args: Vec<OsString> = vec![
        "-v".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        src_abs.as_os_str().to_owned(),
        "-map".into(),
        "0".into(),
        "-map_metadata".into(),
        "0".into(),
        "-af".into(),
        format!("volume={gain_db:.2}dB,alimiter=limit=0.891:level=false").into(),
    ];
    args.extend(encoder.iter().map(|s| OsString::from(*s)));
    args.push(tmp_path.as_os_str().to_owned());

    EnhancePlan::Job(Box::new(EnhanceJob {
        gain_db,
        ffmpeg_args: args,
        tmp_path,
        dest_path,
        backup_path,
        new_rel_path,
    }))
}

/// Finds every file in `rel_path`'s mirrored backup subfolder whose stem
/// matches `rel_path`'s stem, regardless of extension. Shared by
/// `existing_backup_for` (stem-aware first-touch, see below) and by
/// `restore_tracks` in commands.rs (same matching, used to find the one
/// backup file to restore from — zero or multiple candidates there is a
/// named failure rather than a guess).
pub fn backup_candidates_for(library_root: &Path, rel_path: &str) -> Vec<PathBuf> {
    let Some(backup_dir) = backup_path_for(library_root, rel_path)
        .parent()
        .map(|p| p.to_path_buf())
    else {
        return Vec::new();
    };
    let stem = Path::new(rel_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut candidates = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&backup_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.file_stem().map(|s| s.to_string_lossy().to_string()).as_deref()
                == Some(stem.as_str())
            {
                candidates.push(p);
            }
        }
    }
    candidates
}

/// Stem-aware first-touch lookup: if a backup already exists for `rel_path`'s
/// stem under a DIFFERENT extension (e.g. a prior FLAC-out replace turned
/// `track.mp3` into `track.flac`, and the mp3 was backed up before that
/// swap), returns that existing backup so the caller treats the track as
/// already backed up rather than creating a second, stem-colliding backup
/// file (which would later make `restore_tracks`' own stem search ambiguous
/// and refuse to restore at all). Ambiguous (0 or >1 candidates) returns
/// `None` so the caller falls back to `ensure_backup` at the conventional
/// path for `rel_path` itself.
///
/// Guards against genuinely different sibling files that happen to share a
/// stem (e.g. `t.mp3` and `t.wav` living side by side in the same folder):
/// the stem match is only reused when the live file implied by the backup's
/// own name (same folder as `rel_path`, backup's file name) is either absent
/// — the legit post-FLAC-out case, where the original extension no longer
/// exists live — or is `rel_path` itself. If that live sibling exists and is
/// a different file, the candidate is a stem collision, not a prior backup
/// of `rel_path`, so this returns `None` and the caller falls back to
/// `ensure_backup` at the conventional path (giving `rel_path` its own,
/// separate backup rather than silently skipping it).
pub fn existing_backup_for(library_root: &Path, rel_path: &str) -> Option<PathBuf> {
    let candidate = {
        let mut candidates = backup_candidates_for(library_root, rel_path);
        if candidates.len() == 1 {
            candidates.pop()
        } else {
            None
        }
    }?;

    let candidate_name = candidate.file_name()?;
    let segs: Vec<&str> = rel_path.split('/').collect();
    let mut live_path = library_root.to_path_buf();
    for seg in &segs[..segs.len().saturating_sub(1)] {
        live_path.push(seg);
    }
    live_path.push(candidate_name);

    let mut track_abs = library_root.to_path_buf();
    for seg in &segs {
        track_abs.push(seg);
    }

    if live_path.exists() && live_path != track_abs {
        return None;
    }
    Some(candidate)
}

/// First-touch backup: copies src to backup unless the backup already exists.
/// Verifies the backup exists with size > 0 afterwards. Creates parents.
pub fn ensure_backup(src: &Path, backup: &Path) -> Result<()> {
    if !backup.exists() {
        if let Some(parent) = backup.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::copy(src, backup)
            .with_context(|| format!("backing up {}", src.display()))?;
    }
    let meta = std::fs::metadata(backup)
        .with_context(|| format!("verifying backup {}", backup.display()))?;
    if meta.len() == 0 {
        let _ = std::fs::remove_file(backup); // don't leave a poisoned first-touch
        bail!("backup at {} is empty", backup.display());
    }
    Ok(())
}

/// Renames tmp onto dest; when replacing across extensions, removes the old
/// source AFTER the new file is in place. Never called before ensure_backup
/// in Replace mode — ensure_backup must run first to guard against data loss.
///
/// `allow_replace` gates overwriting a pre-existing dest: NewFile mode passes
/// `false` (closing the TOCTOU race between the caller's own `dest.exists()`
/// check and this rename — a dest that appeared in between is now a hard
/// error instead of a silent overwrite); Replace mode passes `true` since
/// overwriting the track's own file (or, for a checked cross-extension
/// collision, a file already confirmed absent by the caller) is the point.
///
/// A pre-existing dest is renamed aside (not deleted) before the tmp->dest
/// rename, and only removed once that rename has actually succeeded — so a
/// failed rename restores the original dest instead of leaving neither the
/// original nor the enhanced file in place. The aside name contains
/// `.spindex-tmp.` so `clean_orphan_tmps` sweeps up any leftover from a crash
/// between the two renames.
pub fn swap_in(
    tmp: &Path,
    dest: &Path,
    old_src_to_remove: Option<&Path>,
    allow_replace: bool,
) -> Result<()> {
    let dest_existed = dest.exists();
    if dest_existed && !allow_replace {
        bail!("destination {} already exists", dest.display());
    }

    let mut aside_name = dest.as_os_str().to_os_string();
    aside_name.push(".spindex-tmp.old");
    let aside = PathBuf::from(aside_name);

    if dest_existed {
        std::fs::rename(dest, &aside)
            .with_context(|| format!("moving {} aside before replacing", dest.display()))?;
    }

    if let Err(e) = std::fs::rename(tmp, dest) {
        if dest_existed {
            // restore the original — a failed swap must never leave the
            // track with neither the original nor the enhanced file
            let _ = std::fs::rename(&aside, dest);
        }
        return Err(e).with_context(|| format!("moving {} into place", tmp.display()));
    }
    if dest_existed {
        let _ = std::fs::remove_file(&aside);
    }

    if let Some(old) = old_src_to_remove {
        if old != dest {
            std::fs::remove_file(old)
                .with_context(|| format!("removing superseded {}", old.display()))?;
        }
    }
    Ok(())
}

/// Copies `content` to a `.spindex-tmp.` file beside `dest`, then atomically
/// swaps it into place: a pre-existing `dest` is renamed aside first and
/// only removed once the tmp->dest rename has actually succeeded, so a crash
/// or failure between the two renames never leaves neither file in place —
/// the same aside pattern `swap_in` uses. The aside/tmp names contain
/// `.spindex-tmp.` so `clean_orphan_tmps` sweeps up any leftover.
fn atomic_copy_into(content: &Path, dest: &Path) -> Result<()> {
    let dir = dest.parent().unwrap_or_else(|| Path::new("."));
    let dest_name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = dir.join(format!("{dest_name}.spindex-tmp.restore"));
    std::fs::copy(content, &tmp)
        .with_context(|| format!("copying {} to {}", content.display(), tmp.display()))?;

    let dest_existed = dest.exists();
    let mut aside_name = dest.as_os_str().to_os_string();
    aside_name.push(".spindex-tmp.old");
    let aside = PathBuf::from(aside_name);

    if dest_existed {
        if let Err(e) = std::fs::rename(dest, &aside) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e)
                .with_context(|| format!("moving {} aside before restoring", dest.display()));
        }
    }
    if let Err(e) = std::fs::rename(&tmp, dest) {
        if dest_existed {
            // restore the original — a failed swap must never leave the
            // track with neither the original nor the restored file
            let _ = std::fs::rename(&aside, dest);
        }
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("moving restored file into {}", dest.display()));
    }
    if dest_existed {
        let _ = std::fs::remove_file(&aside);
    }
    Ok(())
}

/// Copies the backup over the enhanced file (backup kept), via an atomic
/// tmp-write + rename so a crash mid-restore never leaves `original`
/// half-written. When the enhanced file changed extension, removes it and
/// restores under the original name.
pub fn restore_from_backup(backup: &Path, original: &Path, enhanced: &Path) -> Result<()> {
    if !backup.exists() {
        bail!("backup not found at {}", backup.display());
    }
    atomic_copy_into(backup, original)?;
    if enhanced != original && enhanced.exists() {
        std::fs::remove_file(enhanced)
            .with_context(|| format!("removing enhanced {}", enhanced.display()))?;
    }
    Ok(())
}

/// Deletes `*.spindex-tmp.*` orphans in a directory (best effort).
pub fn clean_orphan_tmps(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.contains(".spindex-tmp.") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn args_str(job: &EnhanceJob) -> Vec<String> {
        job.ffmpeg_args
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("spindex-enh-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn skips_when_already_loud_enough() {
        for lufs in [-9.0, -8.0, -9.2] {
            let p = plan_enhancement(
                Path::new("C:/lib"),
                "a.mp3",
                "mp3",
                None,
                lufs,
                EnhanceMode::NewFile,
                OutputFormat::Keep,
                None,
            );
            assert!(matches!(p, EnhancePlan::AlreadyLoudEnough), "lufs {lufs}");
        }
        // 0.3 dB below target -> enhance
        let p = plan_enhancement(
            Path::new("C:/lib"),
            "a.mp3",
            "mp3",
            None,
            -9.3,
            EnhanceMode::NewFile,
            OutputFormat::Keep,
            None,
        );
        assert!(matches!(p, EnhancePlan::Job(_)));
    }

    #[test]
    fn gain_and_filter_args() {
        let EnhancePlan::Job(job) = plan_enhancement(
            Path::new("C:/lib"),
            "x/quiet.mp3",
            "mp3",
            None,
            -15.5,
            EnhanceMode::NewFile,
            OutputFormat::Keep,
            None,
        ) else {
            panic!()
        };
        assert!((job.gain_db - 6.5).abs() < 1e-9);
        let a = args_str(&job);
        assert!(a.contains(&"volume=6.50dB,alimiter=limit=0.891:level=false".to_string()));
        assert!(a.windows(2).any(|w| w == ["-c:a", "libmp3lame"]));
        assert!(a.windows(2).any(|w| w == ["-b:a", "320k"]));
        assert!(a.windows(2).any(|w| w == ["-map_metadata", "0"]));
    }

    #[test]
    fn new_file_naming_and_tmp() {
        let EnhancePlan::Job(job) = plan_enhancement(
            Path::new("C:/lib"),
            "House/track.flac",
            "flac",
            Some(16),
            -14.0,
            EnhanceMode::NewFile,
            OutputFormat::Keep,
            None,
        ) else {
            panic!()
        };
        assert_eq!(job.dest_path, Path::new("C:/lib/House/track (Enhanced).flac"));
        let tmp = job.tmp_path.to_string_lossy().to_string();
        assert!(tmp.contains(".spindex-tmp."), "{tmp}");
        assert!(tmp.ends_with(".flac"), "muxer inferred from the final extension: {tmp}");
        assert!(job.backup_path.is_none());
        assert!(job.new_rel_path.is_none());
    }

    #[test]
    fn replace_keep_format_has_backup_and_same_rel_path() {
        let EnhancePlan::Job(job) = plan_enhancement(
            Path::new("C:/lib"),
            "House/track.mp3",
            "mp3",
            None,
            -14.0,
            EnhanceMode::Replace,
            OutputFormat::Keep,
            None,
        ) else {
            panic!()
        };
        assert_eq!(job.dest_path, Path::new("C:/lib/House/track.mp3"));
        assert_eq!(
            job.backup_path.as_deref(),
            Some(Path::new("C:/lib/_SpinDexBackups_/House/track.mp3"))
        );
        assert!(job.new_rel_path.is_none(), "same extension: links stay valid");
    }

    #[test]
    fn replace_flac_out_changes_rel_path() {
        let EnhancePlan::Job(job) = plan_enhancement(
            Path::new("C:/lib"),
            "House/track.mp3",
            "mp3",
            None,
            -14.0,
            EnhanceMode::Replace,
            OutputFormat::Flac,
            None,
        ) else {
            panic!()
        };
        assert_eq!(job.dest_path, Path::new("C:/lib/House/track.flac"));
        assert_eq!(job.new_rel_path.as_deref(), Some("House/track.flac"));
        assert_eq!(
            job.backup_path.as_deref(),
            Some(Path::new("C:/lib/_SpinDexBackups_/House/track.mp3")),
            "backup keeps the ORIGINAL name"
        );
        let a = args_str(&job);
        assert!(a.windows(2).any(|w| w == ["-c:a", "flac"]));
    }

    #[test]
    fn pcm_encoder_matches_bit_depth_and_endianness() {
        let job = |fmt: &str, depth: Option<u8>| -> Vec<String> {
            let EnhancePlan::Job(j) = plan_enhancement(
                Path::new("C:/l"),
                &format!("t.{fmt}"),
                fmt,
                depth,
                -14.0,
                EnhanceMode::NewFile,
                OutputFormat::Keep,
                None,
            ) else {
                panic!()
            };
            args_str(&j)
        };
        assert!(job("wav", Some(16))
            .windows(2)
            .any(|w| w == ["-c:a", "pcm_s16le"]));
        assert!(job("wav", Some(24))
            .windows(2)
            .any(|w| w == ["-c:a", "pcm_s24le"]));
        assert!(job("wav", None)
            .windows(2)
            .any(|w| w == ["-c:a", "pcm_s24le"]));
        assert!(job("aiff", Some(16))
            .windows(2)
            .any(|w| w == ["-c:a", "pcm_s16be"]));
        assert!(job("aif", Some(24))
            .windows(2)
            .any(|w| w == ["-c:a", "pcm_s24be"]));
    }

    #[test]
    fn aac_and_unknown_formats() {
        let EnhancePlan::Job(j) = plan_enhancement(
            Path::new("C:/l"),
            "t.m4a",
            "m4a",
            None,
            -14.0,
            EnhanceMode::NewFile,
            OutputFormat::Keep,
            None,
        ) else {
            panic!()
        };
        assert!(args_str(&j).windows(2).any(|w| w == ["-c:a", "aac"]));
        let p = plan_enhancement(
            Path::new("C:/l"),
            "t.ogg",
            "ogg",
            None,
            -14.0,
            EnhanceMode::NewFile,
            OutputFormat::Keep,
            None,
        );
        assert!(matches!(p, EnhancePlan::Unsupported(_)));
    }

    #[test]
    fn new_file_dest_dir_override() {
        let EnhancePlan::Job(job) = plan_enhancement(
            Path::new("C:/lib"),
            "House/track.mp3",
            "mp3",
            None,
            -14.0,
            EnhanceMode::NewFile,
            OutputFormat::Keep,
            Some(Path::new("E:/exports")),
        ) else {
            panic!()
        };
        assert_eq!(job.dest_path, Path::new("E:/exports/track (Enhanced).mp3"));
        let tmp = job.tmp_path.to_string_lossy().to_string();
        assert!(tmp.replace('\\', "/").starts_with("E:/exports/"), "tmp beside dest: {tmp}");
        assert!(job.backup_path.is_none());
    }

    /// The tempo stabilizer shares this layout, differing only in the suffix —
    /// if the two ever drift, one feature silently stops backing up or starts
    /// writing tmp files across volumes.
    #[test]
    fn plan_paths_places_a_steady_output_like_an_enhanced_one() {
        let p = plan_paths(
            Path::new("C:/lib"),
            "House/track.mp3",
            "mp3",
            None,
            EnhanceMode::NewFile,
            OutputFormat::Keep,
            None,
            STEADY_SUFFIX,
        )
        .expect("mp3 is supported");
        assert_eq!(p.dest_path, Path::new("C:/lib/House/track (Steady).mp3"));
        assert_eq!(p.tmp_path, Path::new("C:/lib/House/track.spindex-tmp.mp3"));
        assert!(p.backup_path.is_none(), "NewFile never touches the original");
        assert!(p.new_rel_path.is_none());

        let r = plan_paths(
            Path::new("C:/lib"),
            "House/track.mp3",
            "mp3",
            None,
            EnhanceMode::Replace,
            OutputFormat::Flac,
            None,
            STEADY_SUFFIX,
        )
        .expect("flac out is always supported");
        assert_eq!(r.dest_path, Path::new("C:/lib/House/track.flac"));
        assert_eq!(r.new_rel_path.as_deref(), Some("House/track.flac"));
        assert_eq!(
            r.backup_path.as_deref(),
            Some(Path::new("C:/lib/_SpinDexBackups_/House/track.mp3")),
            "the backup keeps the ORIGINAL extension"
        );
    }

    #[test]
    fn backup_path_mirrors_tree() {
        assert_eq!(
            backup_path_for(Path::new("D:/Music"), "A/B/t.wav"),
            Path::new("D:/Music/_SpinDexBackups_/A/B/t.wav")
        );
    }

    #[test]
    fn ensure_backup_is_first_touch() {
        let d = tmpdir("backup");
        let src = d.join("t.mp3");
        std::fs::write(&src, b"ORIGINAL").unwrap();
        let backup = d.join("_SpinDexBackups_/t.mp3");
        ensure_backup(&src, &backup).unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), b"ORIGINAL");
        // second call with a modified source must NOT overwrite the backup
        std::fs::write(&src, b"ENHANCED-V1").unwrap();
        ensure_backup(&src, &backup).unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), b"ORIGINAL");
    }

    #[test]
    fn existing_backup_for_finds_stem_match_after_flac_out() {
        let d = tmpdir("existing-backup");
        // simulate the post-FLAC-out state: the original mp3 was backed up
        // before a Replace+Flac-out swap, so only track.flac remains at the
        // library root and track.mp3 lives solely in the backup mirror.
        let backup = backup_path_for(&d, "t.mp3");
        std::fs::create_dir_all(backup.parent().unwrap()).unwrap();
        std::fs::write(&backup, b"ORIGINAL-MP3").unwrap();
        std::fs::write(d.join("t.flac"), b"ENHANCED-FLAC").unwrap();

        let found = existing_backup_for(&d, "t.flac");
        assert_eq!(found.as_deref(), Some(backup.as_path()));

        // no backup at all yet -> None (ordinary ensure_backup path)
        let d2 = tmpdir("existing-backup-none");
        assert!(existing_backup_for(&d2, "u.mp3").is_none());

        // ambiguous (two stem matches) -> None, caller must not guess
        let d3 = tmpdir("existing-backup-ambiguous");
        let bdir = backup_path_for(&d3, "v.mp3").parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&bdir).unwrap();
        std::fs::write(bdir.join("v.mp3"), b"A").unwrap();
        std::fs::write(bdir.join("v.wav"), b"B").unwrap();
        assert!(existing_backup_for(&d3, "v.flac").is_none());
    }

    #[test]
    fn existing_backup_for_rejects_live_same_stem_sibling() {
        let d = tmpdir("existing-backup-sibling");
        // A genuinely different sibling file: t.mp3 is still live in the
        // folder (never swapped away), it just happens to share a stem with
        // t.wav. Its backup must NOT be reused for t.wav.
        let backup = backup_path_for(&d, "t.mp3");
        std::fs::create_dir_all(backup.parent().unwrap()).unwrap();
        std::fs::write(&backup, b"ORIGINAL-MP3").unwrap();
        std::fs::write(d.join("t.mp3"), b"LIVE-MP3").unwrap();

        assert!(
            existing_backup_for(&d, "t.wav").is_none(),
            "live t.mp3 sibling must not be treated as t.wav's backup"
        );

        // But for the track that IS t.mp3 itself, the backup is legitimately
        // its own first-touch copy and must be found.
        assert_eq!(existing_backup_for(&d, "t.mp3").as_deref(), Some(backup.as_path()));
    }

    /// Worker-visible property: using `existing_backup_for` before
    /// `ensure_backup` (as the enhance worker now does) leaves exactly one
    /// backup file for the stem, never a second stem-colliding one that
    /// would make `restore_tracks`' own search ambiguous.
    #[test]
    fn stem_aware_first_touch_leaves_exactly_one_backup() {
        let d = tmpdir("stem-first-touch");
        let backup = backup_path_for(&d, "t.mp3");
        std::fs::create_dir_all(backup.parent().unwrap()).unwrap();
        std::fs::write(&backup, b"ORIGINAL-MP3").unwrap();
        std::fs::write(d.join("t.flac"), b"ENHANCED-FLAC").unwrap();

        // mirrors the worker: check existing_backup_for first; only call
        // ensure_backup at the conventional (post-swap) path when it's None.
        let conventional = backup_path_for(&d, "t.flac");
        if let Some(existing) = existing_backup_for(&d, "t.flac") {
            assert_eq!(existing, backup);
        } else {
            ensure_backup(&d.join("t.flac"), &conventional).unwrap();
        }

        let bdir = backup.parent().unwrap();
        let entries: Vec<_> = std::fs::read_dir(bdir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["t.mp3".to_string()], "exactly one backup file for the stem");
    }

    #[test]
    fn ensure_backup_rejects_empty_result() {
        let d = tmpdir("backup-empty");
        let src = d.join("t.mp3");
        std::fs::write(&src, b"").unwrap(); // pathological zero-byte source
        let backup = d.join("_SpinDexBackups_/t.mp3");
        assert!(ensure_backup(&src, &backup).is_err(), "size-0 backup must fail");
    }

    #[test]
    fn swap_in_same_and_cross_extension() {
        let d = tmpdir("swap");
        let dest = d.join("t.mp3");
        std::fs::write(&dest, b"OLD").unwrap();
        let tmp = d.join("t.spindex-tmp.mp3");
        std::fs::write(&tmp, b"NEW").unwrap();
        swap_in(&tmp, &dest, None, true).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"NEW");
        assert!(!tmp.exists());
        // the aside file used during the swap is cleaned up on success
        assert!(!d.join("t.mp3.spindex-tmp.old").exists());
        // cross-extension: old source removed only after new file in place
        let old = d.join("u.mp3");
        std::fs::write(&old, b"OLD").unwrap();
        let tmp2 = d.join("u.spindex-tmp.flac");
        std::fs::write(&tmp2, b"NEWFLAC").unwrap();
        let dest2 = d.join("u.flac");
        swap_in(&tmp2, &dest2, Some(&old), true).unwrap();
        assert_eq!(std::fs::read(&dest2).unwrap(), b"NEWFLAC");
        assert!(!old.exists());
    }

    #[test]
    fn swap_in_refuses_pre_existing_dest_unless_allow_replace() {
        let d = tmpdir("swap-toctou");
        let dest = d.join("t.mp3");
        std::fs::write(&dest, b"EXISTING").unwrap();
        let tmp = d.join("t.spindex-tmp.mp3");
        std::fs::write(&tmp, b"NEW").unwrap();

        assert!(swap_in(&tmp, &dest, None, false).is_err(), "NewFile mode must refuse an existing dest");
        // the pre-existing dest and the tmp are both untouched by the refusal
        assert_eq!(std::fs::read(&dest).unwrap(), b"EXISTING");
        assert!(tmp.exists());
    }

    #[test]
    fn swap_in_restores_dest_when_final_rename_fails() {
        let d = tmpdir("swap-fail");
        let dest = d.join("t.mp3");
        std::fs::write(&dest, b"ORIGINAL").unwrap();
        // tmp doesn't exist -> the tmp->dest rename fails after the aside
        // rename already happened; dest must come back instead of vanishing.
        let tmp = d.join("missing.spindex-tmp.mp3");
        assert!(swap_in(&tmp, &dest, None, true).is_err());
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"ORIGINAL",
            "a failed swap must restore the original dest"
        );
        assert!(!d.join("t.mp3.spindex-tmp.old").exists(), "aside file must not leak on failure");
    }

    #[test]
    fn restore_round_trip_keeps_backup() {
        let d = tmpdir("restore");
        let backup = d.join("_SpinDexBackups_/t.mp3");
        std::fs::create_dir_all(backup.parent().unwrap()).unwrap();
        std::fs::write(&backup, b"ORIGINAL").unwrap();
        // enhanced-in-place, same name
        let orig = d.join("t.mp3");
        std::fs::write(&orig, b"ENHANCED").unwrap();
        restore_from_backup(&backup, &orig, &orig).unwrap();
        assert_eq!(std::fs::read(&orig).unwrap(), b"ORIGINAL");
        assert!(backup.exists(), "backup survives restore");
        // enhanced under a different extension
        let enhanced = d.join("t.flac");
        std::fs::write(&enhanced, b"ENHFLAC").unwrap();
        std::fs::write(&orig, b"STALE").unwrap();
        restore_from_backup(&backup, &orig, &enhanced).unwrap();
        assert_eq!(std::fs::read(&orig).unwrap(), b"ORIGINAL");
        assert!(!enhanced.exists(), "cross-extension enhanced file removed");

        // the atomic tmp/aside files used during the write are cleaned up
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            assert!(!n.contains(".spindex-tmp."), "leftover tmp file: {n}");
        }
    }

    #[test]
    fn clean_orphan_tmps_only_touches_tmps() {
        let d = tmpdir("orphans");
        std::fs::write(d.join("a.spindex-tmp.mp3"), b"x").unwrap();
        std::fs::write(d.join("keep.mp3"), b"x").unwrap();
        clean_orphan_tmps(&d);
        assert!(!d.join("a.spindex-tmp.mp3").exists());
        assert!(d.join("keep.mp3").exists());
    }
}
