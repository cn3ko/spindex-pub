//! Golden check of `integrated_lufs` against ffmpeg's own `loudnorm` filter.
//! Synthesizes a ~10s stereo sine via ffmpeg, measures it two ways (our
//! `decode_pcm_stereo` + `integrated_lufs`, and ffmpeg's loudnorm analysis
//! pass), and asserts the two agree within +/-0.5 LU. Env-independent but
//! needs the bundled ffmpeg binary, so it is `#[ignore]`d — CI without
//! `binaries/ffmpeg.exe` stays green.
//!
//!   cargo test --test lufs_golden -- --ignored --nocapture

use spindex::core::analysis::{decode_pcm_stereo, find_ffmpeg};
use spindex::core::quality::integrated_lufs;

/// Finds `"key": <number>` or `"key": "<number>"` (loudnorm quotes its
/// values) in ffmpeg's loudnorm JSON (printed to stderr) and parses it.
fn extract_json_number(text: &str, key: &str) -> Option<f64> {
    let needle = format!("\"{key}\"");
    let start = text.find(&needle)? + needle.len();
    let rest = &text[start..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let after = after.strip_prefix('"').unwrap_or(after);
    let end = after
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .unwrap_or(after.len());
    after[..end].parse().ok()
}

#[test]
#[ignore]
fn lufs_matches_ffmpeg_loudnorm_golden() {
    let Some(ffmpeg) = find_ffmpeg() else {
        eprintln!("SPINDEX: no bundled ffmpeg found, skipping lufs_golden");
        return;
    };

    let dir = std::env::temp_dir();
    let wav_path = dir.join("spindex-lufs-golden.wav");

    // Synthesize ~10s of stereo sine content (attenuated so it isn't just
    // full-scale, exercising the meter's actual gain path).
    let synth = std::process::Command::new(&ffmpeg)
        .arg("-y")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("sine=frequency=440:duration=10")
        .arg("-af")
        .arg("volume=0.3")
        .arg("-ac")
        .arg("2")
        .arg(&wav_path)
        .output()
        .expect("running ffmpeg to synthesize golden wav");
    assert!(
        synth.status.success(),
        "ffmpeg synth failed: {}",
        String::from_utf8_lossy(&synth.stderr)
    );

    // Measure with ffmpeg's own loudnorm analysis pass (JSON on stderr).
    let out = std::process::Command::new(&ffmpeg)
        .arg("-i")
        .arg(&wav_path)
        .arg("-af")
        .arg("loudnorm=print_format=json")
        .arg("-f")
        .arg("null")
        .arg("-")
        .output()
        .expect("running ffmpeg loudnorm");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let ffmpeg_lufs = extract_json_number(&stderr, "input_i")
        .unwrap_or_else(|| panic!("no input_i in ffmpeg loudnorm output: {stderr}"));

    // Measure with our own stereo decode + meter.
    let interleaved =
        decode_pcm_stereo(&ffmpeg, &wav_path, 44_100).expect("decode_pcm_stereo on golden wav");
    let n = interleaved.len() / 2;
    let mut l = Vec::with_capacity(n);
    let mut r = Vec::with_capacity(n);
    for frame in interleaved.chunks_exact(2) {
        l.push(frame[0]);
        r.push(frame[1]);
    }
    let ours = integrated_lufs(&[&l, &r], 44_100).expect("golden signal has loud blocks");

    let _ = std::fs::remove_file(&wav_path);

    eprintln!("lufs golden: ours={ours:.2} LUFS, ffmpeg loudnorm input_i={ffmpeg_lufs:.2} LUFS, delta={:.2}", ours - ffmpeg_lufs);
    assert!(
        (ours - ffmpeg_lufs).abs() < 0.5,
        "integrated_lufs {ours} vs ffmpeg loudnorm {ffmpeg_lufs}"
    );
}
