//! BPM/key/waveform analysis via sidecar processes: ffmpeg decodes to raw PCM,
//! spindex-analyzer (Rust, Beat This! + OpenKeyScan via ONNX Runtime) turns
//! PCM into JSON. All failures are per-track: callers log them into
//! track.analysis_error.

use std::ffi::OsString;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};

pub const SAMPLE_RATE: u32 = 22050;
pub const PEAK_BUCKETS: usize = 400;
// CPU transformer inference on long tracks needs headroom beyond the old 60s.
pub const ANALYSIS_TIMEOUT: Duration = Duration::from_secs(180);
/// Full-track beat inference runs at roughly 13x realtime, so a long DJ mix
/// legitimately takes minutes — the normal watchdog would kill it mid-pass.
pub const FULL_BEATS_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct KeyCandidate {
    pub key: String,
    /// Mean probability across the vote passes, 0-1.
    pub p: f64,
}

#[derive(Debug, serde::Deserialize)]
pub struct AnalysisResult {
    pub bpm: Option<f64>,
    pub key_camelot: Option<String>,
    /// Mean winner probability of the segment-voted key passes (0-1).
    /// Optional so output from an older sidecar still parses.
    #[serde(default)]
    pub key_confidence: Option<f64>,
    /// Ranked alternatives from the vote, winner first.
    #[serde(default)]
    pub key_candidates: Option<Vec<KeyCandidate>>,
    pub peaks: Vec<u8>,
    /// Interleaved low,mid,high per peak bucket (3 × PEAK_BUCKETS bytes).
    /// Optional so output from an older sidecar still parses.
    #[serde(default)]
    pub bands: Option<Vec<u8>>,
    /// Absolute beat times in seconds — sampled islands on the normal Analyze
    /// pass, continuous only when the sidecar ran with `--full-beats`.
    /// Optional so output from an older sidecar still parses.
    #[serde(default)]
    pub beats: Option<Vec<f64>>,
    /// Absolute downbeat times in seconds (beat 1 of each bar), same sampling
    /// as `beats`. Optional so output from an older sidecar still parses.
    #[serde(default)]
    pub downbeats: Option<Vec<f64>>,
}

pub struct Sidecars {
    pub ffmpeg: PathBuf,
    pub analyzer: PathBuf,
}

/// Musical key -> Camelot code. Mirrors the switch in analyzer/main.cpp —
/// keep the two in sync. Used by the (future) metadata editor and tests.
pub fn camelot_for(key: &str) -> Option<&'static str> {
    Some(match key {
        "A major" => "11B",
        "A minor" => "8A",
        "Bb major" => "6B",
        "Bb minor" => "3A",
        "B major" => "1B",
        "B minor" => "10A",
        "C major" => "8B",
        "C minor" => "5A",
        "Db major" => "3B",
        "Db minor" => "12A",
        "D major" => "10B",
        "D minor" => "7A",
        "Eb major" => "5B",
        "Eb minor" => "2A",
        "E major" => "12B",
        "E minor" => "9A",
        "F major" => "7B",
        "F minor" => "4A",
        "Gb major" => "2B",
        "Gb minor" => "11A",
        "G major" => "9B",
        "G minor" => "6A",
        "Ab major" => "4B",
        "Ab minor" => "1A",
        _ => return None,
    })
}

/// Where the user-installed components live: `%APPDATA%\app.spindex.desktop\bin`.
///
/// ffmpeg (GPLv3) is deliberately NOT bundled in the
/// installer — SpinDex distributes only its own binaries — so the user fetches
/// it with `install-components.ps1`, which writes it here. This directory
/// is user-writable, unlike an installation under Program Files, and survives
/// an app upgrade.
pub fn components_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("app.spindex.desktop").join("bin"))
}

/// Candidate directories for the sidecar binaries, in priority order:
/// src-tauri/binaries first (in development it holds the *release* analyzer —
/// the debug build next to the dev exe is 4x slower, and the path doesn't
/// exist on end-user machines), then the exe's own directory (where the
/// installer puts SpinDex's own sidecar), then the user components directory.
fn sidecar_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
        }
    }
    dirs.extend(components_dir());
    dirs
}

/// A discovered file only counts when it has content: zero-byte placeholder
/// files (seen once from an interrupted copy into a worktree) would otherwise
/// pass discovery and fail cryptically at spawn/load time.
fn non_empty_file(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_file() && m.len() > 0).unwrap_or(false)
}

fn find_binary(name: &str) -> Option<PathBuf> {
    // Tauri strips the target-triple suffix when bundling, so both names occur.
    let candidates = [
        format!("{name}.exe"),
        format!("{name}-x86_64-pc-windows-msvc.exe"),
        name.to_string(),
    ];
    for dir in sidecar_dirs() {
        for file in &candidates {
            let p = dir.join(file);
            if non_empty_file(&p) {
                return Some(p);
            }
        }
    }
    None
}

/// ffmpeg alone is enough for playback transcoding.
pub fn find_ffmpeg() -> Option<PathBuf> {
    find_binary("ffmpeg")
}

/// The ML models the analyzer needs, looked up like the binaries: in a
/// models/ subfolder next to the exe or under src-tauri/binaries.
/// Windows error 126, ERROR_MOD_NOT_FOUND: the exe was found and started, but
/// a DLL it imports was not. For spindex-analyzer that is nearly always the
/// Visual C++ runtime — ONNX Runtime pulls in MSVCP140.dll, which is NOT part
/// of Windows and cannot be statically linked here (the prebuilt ONNX Runtime
/// library is compiled against the dynamic CRT, so `+crt-static` leaves dozens
/// of unresolved symbols). Without this, the failure surfaces as a bare "the
/// specified module could not be found" against a file that plainly exists.
const ERROR_MOD_NOT_FOUND: i32 = 126;

fn explain_analyzer_spawn_failure(err: std::io::Error) -> anyhow::Error {
    if err.raw_os_error() == Some(ERROR_MOD_NOT_FOUND) {
        return anyhow::anyhow!(
            "spindex-analyzer could not start because a system library is missing. \
             Install the Microsoft Visual C++ Redistributable (x64) from \
             https://aka.ms/vs/17/release/vc_redist.x64.exe and restart SpinDex. \
             (underlying error: {err})"
        );
    }
    anyhow::Error::new(err).context("spawning spindex-analyzer")
}

/// First `<sidecar dir>/<name>` that exists, for user-installed component
/// *directories* (Apollo) rather than single files.
pub fn find_component_dir(name: &str) -> Option<PathBuf> {
    sidecar_dirs().into_iter().map(|d| d.join(name)).find(|d| d.is_dir())
}

fn models_present() -> bool {
    sidecar_dirs().iter().map(|d| d.join("models")).any(|d| {
        non_empty_file(&d.join("beat_this.onnx")) && non_empty_file(&d.join("spindex-key.onnx"))
    })
}

pub fn find_sidecars() -> Option<Sidecars> {
    if !models_present() {
        return None;
    }
    Some(Sidecars {
        ffmpeg: find_binary("ffmpeg")?,
        analyzer: find_binary("spindex-analyzer")?,
    })
}

/// ffmpeg arguments decoding any input to the raw stream the analyzer expects.
pub fn ffmpeg_args(input: &Path) -> Vec<OsString> {
    decode_args(input, SAMPLE_RATE)
}

/// ffmpeg arguments decoding any input to raw mono f32le at `sample_rate`.
pub fn decode_args(input: &Path, sample_rate: u32) -> Vec<OsString> {
    vec![
        "-v".into(),
        "error".into(),
        "-i".into(),
        input.as_os_str().to_owned(),
        "-map".into(),
        "0:a:0".into(),
        "-f".into(),
        "f32le".into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        sample_rate.to_string().into(),
        "-".into(),
    ]
}

pub(crate) fn command_no_window(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Like `command_no_window`, for long jobs that use every core (Apollo):
/// starts the process below normal priority so the UI and playback keep
/// getting CPU. Child processes inherit the class (the venv's python.exe is
/// a launcher that starts the real interpreter).
pub(crate) fn command_background(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    }
    cmd
}

/// Runs ffmpeg | spindex-analyzer over one file. A watchdog kills both
/// processes if no result arrives within ANALYSIS_TIMEOUT.
pub fn analyze_file(sc: &Sidecars, path: &Path) -> Result<AnalysisResult> {
    run_analyzer(sc, path, &[], ANALYSIS_TIMEOUT)
}

/// Same pipeline, but asking the sidecar to cover the track end to end. Only
/// the tempo stabilizer needs this: the grid stored by the normal pass is
/// sampled, and holding one stretch ratio across a 30 s hole is guessing, not
/// warping. Costs ~3x the normal pass, hence the separate watchdog.
pub fn analyze_beats_full(sc: &Sidecars, path: &Path) -> Result<AnalysisResult> {
    run_analyzer(sc, path, &["--full-beats"], FULL_BEATS_TIMEOUT)
}

fn run_analyzer(
    sc: &Sidecars,
    path: &Path,
    analyzer_args: &[&str],
    timeout: Duration,
) -> Result<AnalysisResult> {
    let mut ffmpeg = command_no_window(&sc.ffmpeg)
        .args(ffmpeg_args(path))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawning ffmpeg for {}", path.display()))?;
    let pcm = ffmpeg.stdout.take().context("ffmpeg stdout unavailable")?;

    let mut analyzer = command_no_window(&sc.analyzer)
        .args(analyzer_args)
        .stdin(Stdio::from(pcm))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(explain_analyzer_spawn_failure)?;
    let mut out_pipe = analyzer.stdout.take().context("analyzer stdout unavailable")?;
    let mut err_pipe = analyzer.stderr.take().context("analyzer stderr unavailable")?;

    let (out_tx, out_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let res = out_pipe.read_to_string(&mut buf).map(|_| buf);
        let _ = out_tx.send(res);
    });
    let (err_tx, err_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = err_pipe.read_to_string(&mut buf);
        let _ = err_tx.send(buf);
    });

    let stdout = match out_rx.recv_timeout(timeout) {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            kill_both(&mut ffmpeg, &mut analyzer);
            bail!("reading analyzer output: {e}");
        }
        Err(_) => {
            kill_both(&mut ffmpeg, &mut analyzer);
            bail!("analysis timed out after {}s", timeout.as_secs());
        }
    };

    let status = analyzer.wait().context("waiting for analyzer")?;
    let _ = ffmpeg.kill();
    let _ = ffmpeg.wait();
    if !status.success() {
        let stderr = err_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_default();
        bail!("analyzer failed ({status}): {}", stderr.trim());
    }

    let result: AnalysisResult = serde_json::from_str(stdout.trim()).with_context(|| {
        let head: String = stdout.chars().take(200).collect();
        format!("unparseable analyzer output: {head}")
    })?;
    Ok(result)
}

/// Transcodes a file the webview cannot play (aiff) to a wav for preview.
pub fn transcode_to_wav(ffmpeg: &Path, input: &Path, output: &Path) -> Result<()> {
    let out = command_no_window(ffmpeg)
        .arg("-v")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(input)
        .arg("-map")
        .arg("0:a:0")
        .arg(output)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("spawning ffmpeg for {}", input.display()))?;
    if !out.status.success() {
        bail!(
            "transcode failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Decodes a file to mono f32 PCM at the given rate — the full-band feed for
/// quality measurement (the analyzer pipe runs at 22 050 Hz, whose Nyquist
/// hides lossy-codec cutoffs). No watchdog: a plain decode does not hang the
/// way model inference can, matching transcode_to_wav.
pub fn decode_pcm(ffmpeg: &Path, input: &Path, sample_rate: u32) -> Result<Vec<f32>> {
    let out = command_no_window(ffmpeg)
        .args(decode_args(input, sample_rate))
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("spawning ffmpeg for {}", input.display()))?;
    if !out.status.success() {
        bail!("decode failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim());
    }
    let mut samples = Vec::with_capacity(out.stdout.len() / 4);
    for chunk in out.stdout.chunks_exact(4) {
        samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(samples)
}

/// ffmpeg arguments decoding any input to raw stereo f32le at `sample_rate`.
fn decode_stereo_args(input: &Path, sample_rate: u32) -> Vec<OsString> {
    vec![
        "-v".into(),
        "error".into(),
        "-i".into(),
        input.as_os_str().to_owned(),
        "-map".into(),
        "0:a:0".into(),
        "-f".into(),
        "f32le".into(),
        "-ac".into(),
        "2".into(),
        "-ar".into(),
        sample_rate.to_string().into(),
        "-".into(),
    ]
}

/// Decodes a file to interleaved (L R L R…) stereo f32 PCM at the given
/// rate — BS.1770 loudness needs both channels (a mono downmix reads ~3 LU
/// low on true-stereo masters), so quality measurement decodes stereo and
/// downmixes to mono itself only for the parts that don't care (clip ratio,
/// spectral cutoff).
pub fn decode_pcm_stereo(ffmpeg: &Path, input: &Path, sample_rate: u32) -> Result<Vec<f32>> {
    let out = command_no_window(ffmpeg)
        .args(decode_stereo_args(input, sample_rate))
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("spawning ffmpeg for {}", input.display()))?;
    if !out.status.success() {
        bail!("decode failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim());
    }
    let mut samples = Vec::with_capacity(out.stdout.len() / 4);
    for chunk in out.stdout.chunks_exact(4) {
        samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(samples)
}

fn kill_both(a: &mut std::process::Child, b: &mut std::process::Child) {
    let _ = a.kill();
    let _ = b.kill();
    let _ = a.wait();
    let _ = b.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apollo runs for half an hour on every core; at normal priority it
    /// starved the UI. A background command must start below normal.
    #[cfg(windows)]
    #[test]
    fn background_command_runs_below_normal_priority() {
        let out = command_background(Path::new("powershell.exe"))
            .args(["-NoProfile", "-Command", "(Get-Process -Id $PID).PriorityClass"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "BelowNormal");
    }

    #[test]
    fn camelot_covers_all_24_keys() {
        let expected = [
            ("A major", "11B"), ("A minor", "8A"),
            ("Bb major", "6B"), ("Bb minor", "3A"),
            ("B major", "1B"), ("B minor", "10A"),
            ("C major", "8B"), ("C minor", "5A"),
            ("Db major", "3B"), ("Db minor", "12A"),
            ("D major", "10B"), ("D minor", "7A"),
            ("Eb major", "5B"), ("Eb minor", "2A"),
            ("E major", "12B"), ("E minor", "9A"),
            ("F major", "7B"), ("F minor", "4A"),
            ("Gb major", "2B"), ("Gb minor", "11A"),
            ("G major", "9B"), ("G minor", "6A"),
            ("Ab major", "4B"), ("Ab minor", "1A"),
        ];
        for (key, code) in expected {
            assert_eq!(camelot_for(key), Some(code), "{key}");
        }
        assert_eq!(camelot_for("H major"), None);
        // every camelot code 1A..12B appears exactly once
        let mut codes: Vec<_> = expected.iter().map(|(_, c)| *c).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), 24);
    }

    #[test]
    fn ffmpeg_args_produce_raw_mono_pcm() {
        let args = ffmpeg_args(Path::new("C:\\music\\track.mp3"));
        let joined: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(joined.windows(2).any(|w| w == ["-f", "f32le"]));
        assert!(joined.windows(2).any(|w| w == ["-ac", "1"]));
        assert!(joined.windows(2).any(|w| w == ["-ar", "22050"]));
        assert!(joined.contains(&"C:\\music\\track.mp3".to_string()));
        assert_eq!(joined.last().map(String::as_str), Some("-"));
    }

    #[test]
    fn decode_args_use_requested_rate() {
        let args = decode_args(Path::new("C:\\music\\track.mp3"), 44_100);
        let joined: Vec<String> = args.iter().map(|a| a.to_string_lossy().to_string()).collect();
        assert!(joined.windows(2).any(|w| w == ["-ar", "44100"]));
        assert!(joined.windows(2).any(|w| w == ["-f", "f32le"]));
        assert!(joined.windows(2).any(|w| w == ["-ac", "1"]));
    }

    #[test]
    fn result_parses_with_and_without_bands() {
        let with: AnalysisResult = serde_json::from_str(
            r#"{"bpm":120.0,"key_camelot":"8A","peaks":[1,2],"bands":[1,2,3,4,5,6]}"#,
        )
        .expect("with bands");
        assert_eq!(with.bands.as_deref(), Some(&[1u8, 2, 3, 4, 5, 6][..]));

        let without: AnalysisResult =
            serde_json::from_str(r#"{"bpm":null,"key_camelot":null,"peaks":[1,2]}"#)
                .expect("without bands");
        assert!(without.bands.is_none());
        assert!(without.beats.is_none(), "a sidecar without beats is not an error");

        let beats: AnalysisResult = serde_json::from_str(
            r#"{"bpm":120.0,"key_camelot":null,"peaks":[1,2],"beats":[0.5,1.0,1.5]}"#,
        )
        .expect("with beats");
        assert_eq!(beats.beats.as_deref(), Some(&[0.5f64, 1.0, 1.5][..]));
    }

    #[test]
    fn parses_downbeats_when_present_and_absent() {
        let with: AnalysisResult = serde_json::from_str(
            r#"{"bpm":120.0,"key_camelot":null,"peaks":[1],"beats":[0.5,1.0],"downbeats":[0.5]}"#,
        ).unwrap();
        assert_eq!(with.downbeats.as_deref(), Some(&[0.5f64][..]));
        let without: AnalysisResult =
            serde_json::from_str(r#"{"bpm":null,"key_camelot":null,"peaks":[1]}"#).unwrap();
        assert!(without.downbeats.is_none());
    }
}
