//! Live check of the enhance pipeline against real ffmpeg: synthesizes a
//! quiet stereo wav (~-35 LUFS), runs `plan_enhancement`'s ffmpeg args on it
//! for both OutputFormat::Keep and OutputFormat::Flac, then decodes the
//! result back and asserts the measured loudness lands near the -9.0 LUFS
//! target and the limiter kept peaks under the ceiling. Needs the bundled
//! ffmpeg binary, so it is `#[ignore]`d.
//!
//!   cargo test --test enhance_live -- --ignored --nocapture

use spindex::core::analysis::{decode_pcm_stereo, find_ffmpeg};
use spindex::core::enhance::{plan_enhancement, EnhanceMode, EnhancePlan, OutputFormat};
use spindex::core::quality::integrated_lufs;

/// Synthesizes an 8s, ~-35 LUFS stereo sine at `path` (440 Hz, attenuated).
fn synth_quiet_wav(ffmpeg: &std::path::Path, path: &std::path::Path) {
    let synth = std::process::Command::new(ffmpeg)
        .arg("-y")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("sine=frequency=440:duration=8")
        .arg("-af")
        .arg("volume=0.02")
        .arg("-ac")
        .arg("2")
        .arg(path)
        .output()
        .expect("running ffmpeg to synthesize the quiet fixture wav");
    assert!(
        synth.status.success(),
        "ffmpeg synth failed: {}",
        String::from_utf8_lossy(&synth.stderr)
    );
}

/// Measures a file's loudness and peak sample via the same decode path the
/// enhance worker uses.
fn measure(ffmpeg: &std::path::Path, path: &std::path::Path) -> (f64, f32) {
    let interleaved =
        decode_pcm_stereo(ffmpeg, path, 44_100).expect("decode_pcm_stereo on measured file");
    let n = interleaved.len() / 2;
    let mut l = Vec::with_capacity(n);
    let mut r = Vec::with_capacity(n);
    for frame in interleaved.chunks_exact(2) {
        l.push(frame[0]);
        r.push(frame[1]);
    }
    let lufs = integrated_lufs(&[&l, &r], 44_100).expect("enhanced signal has loud blocks");
    let peak = interleaved.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    (lufs, peak)
}

fn run_case(ffmpeg: &std::path::Path, dir: &std::path::Path, out: OutputFormat, label: &str) {
    let src = dir.join(format!("quiet-{label}.wav"));
    synth_quiet_wav(ffmpeg, &src);

    let measured_lufs = {
        let interleaved = decode_pcm_stereo(ffmpeg, &src, 44_100).expect("decode source");
        let n = interleaved.len() / 2;
        let mut l = Vec::with_capacity(n);
        let mut r = Vec::with_capacity(n);
        for frame in interleaved.chunks_exact(2) {
            l.push(frame[0]);
            r.push(frame[1]);
        }
        integrated_lufs(&[&l, &r], 44_100).expect("source has loud blocks")
    };
    eprintln!("enhance_live[{label}]: source measured {measured_lufs:.2} LUFS");

    let rel_path = format!("quiet-{label}.wav");
    let job = match plan_enhancement(
        dir,
        &rel_path,
        "wav",
        Some(16),
        measured_lufs,
        EnhanceMode::NewFile,
        out,
        // No destination folder: the enhanced file lands beside the source.
        None,
    ) {
        EnhancePlan::Job(job) => job,
        EnhancePlan::AlreadyLoudEnough => panic!("quiet fixture should always need enhancement"),
        EnhancePlan::Unsupported(msg) => panic!("wav should be supported: {msg}"),
    };

    let output = std::process::Command::new(ffmpeg)
        .args(&job.ffmpeg_args)
        .output()
        .expect("running ffmpeg enhance pass");
    assert!(
        output.status.success(),
        "ffmpeg enhance pass failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let (lufs, peak) = measure(ffmpeg, &job.tmp_path);
    eprintln!(
        "enhance_live[{label}]: gain={:.2} dB, enhanced measured {lufs:.2} LUFS, peak={peak:.4}",
        job.gain_db
    );

    assert!(
        (lufs - (-9.0)).abs() <= 1.0,
        "[{label}] enhanced loudness {lufs:.2} LUFS not within +/-1.0 LU of -9.0"
    );
    assert!(
        peak <= 0.92,
        "[{label}] enhanced peak {peak:.4} exceeds the 0.92 ceiling"
    );

    if out == OutputFormat::Flac {
        assert_eq!(job.tmp_path.extension().and_then(|e| e.to_str()), Some("flac"));
    }

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&job.tmp_path);
}

#[test]
#[ignore]
fn enhance_pipeline_hits_target_lufs_and_peak_ceiling() {
    let Some(ffmpeg) = find_ffmpeg() else {
        eprintln!("SPINDEX: no bundled ffmpeg found, skipping enhance_live");
        return;
    };

    let dir = std::env::temp_dir().join(format!("spindex-enhance-live-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("creating enhance_live scratch dir");

    run_case(&ffmpeg, &dir, OutputFormat::Keep, "keep");
    run_case(&ffmpeg, &dir, OutputFormat::Flac, "flac");

    let _ = std::fs::remove_dir_all(&dir);
}
