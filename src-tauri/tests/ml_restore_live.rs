//! Live ML-restoration pipeline test on a real damaged file. Ignored by
//! default; needs the bundled ffmpeg AND a completed scripts/setup-apollo.ps1.
//!
//!   SPINDEX_RESTORE_FILE="D:\...\track.mp3" cargo test --release --test ml_restore_live -- --ignored --nocapture
//!
//! Takes a few minutes (CPU inference on a 20 s excerpt).
//!
//! What Apollo actually does, per live evidence (two runs on real damaged
//! material): it reduces lossy-compression artifacts *within* the existing
//! frequency band. It does NOT extend bandwidth — measured cutoff was
//! effectively unchanged (16106 Hz before, 16085 Hz after on a 20 s excerpt).
//! So this test does not assert a cutoff rise or a quality-score improvement.
//! Instead it verifies the pipeline runs end to end and guards against
//! regressions: the restored audio decodes, is a genuine (non-passthrough,
//! non-silent) transformation of the input, and doesn't introduce clipping
//! or measurably narrow the band.

use std::path::Path;
use std::process::Command;

use spindex::core::analysis::{decode_pcm_stereo, find_ffmpeg};
use spindex::core::quality::measure_stereo;
use spindex::core::restore;

fn measure_file(ffmpeg: &Path, path: &Path) -> spindex::core::quality::QualityResult {
    let pcm = decode_pcm_stereo(ffmpeg, path, 44_100).expect("decode for measurement");
    measure_stereo(&pcm)
}

#[test]
#[ignore]
fn restores_real_file() {
    let Ok(file) = std::env::var("SPINDEX_RESTORE_FILE") else {
        eprintln!("SPINDEX_RESTORE_FILE not set, skipping");
        return;
    };

    let Some(ffmpeg) = find_ffmpeg() else {
        panic!("ffmpeg not found — run scripts/fetch-ffmpeg.ps1");
    };
    let Some(apollo) = restore::find_apollo_dir().filter(|d| restore::apollo_ready(d)) else {
        panic!("Apollo not set up — run scripts/setup-apollo.ps1");
    };

    let dir = std::env::temp_dir().join(format!("spindex-mlrestore-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let damaged = dir.join("damaged.wav");
    let real_path = Path::new(&file);
    let st = Command::new(&ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(real_path)
        .args(["-t", "20", "-ac", "2", "-ar", "44100", "-c:a", "pcm_s16le"])
        .arg(&damaged)
        .status()
        .unwrap();
    assert!(st.success(), "ffmpeg trim of SPINDEX_RESTORE_FILE failed");
    println!("fixture: {file} (trimmed to 20s)");

    let before = measure_file(&ffmpeg, &damaged);
    println!("before: cutoff={:?} lufs={:?}", before.cutoff_hz, before.lufs);

    let restored_wav = dir.join("restored.wav");
    let python = apollo.join("venv/Scripts/python.exe");
    let start = std::time::Instant::now();
    let out = Command::new(&python)
        .arg(apollo.join("run_restore.py"))
        .arg("--in_wav").arg(&damaged)
        .arg("--out_wav").arg(&restored_wav)
        .current_dir(&apollo)
        .output()
        .unwrap();
    let elapsed = start.elapsed();
    assert!(out.status.success(), "apollo failed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PROGRESS "), "progress lines expected, got: {stdout}");
    println!("inference took {:.1}s", elapsed.as_secs_f64());

    // (a) restored wav exists, decodes, and is close in sample count to the input.
    assert!(restored_wav.exists(), "restored wav was not written");
    let in_pcm = decode_pcm_stereo(&ffmpeg, &damaged, 44_100).expect("decode input");
    let out_pcm = decode_pcm_stereo(&ffmpeg, &restored_wav, 44_100).expect("decode output");
    let ratio = out_pcm.len() as f64 / in_pcm.len() as f64;
    assert!(
        (0.95..=1.05).contains(&ratio),
        "restored sample count should be within 5% of input: in={} out={} ratio={ratio:.3}",
        in_pcm.len(),
        out_pcm.len()
    );

    // (b) restored audio is a genuine transformation, not a passthrough or silence.
    let overlap = in_pcm.len().min(out_pcm.len());
    let diff_sq_sum: f64 = (0..overlap)
        .map(|i| {
            let d = (out_pcm[i] - in_pcm[i]) as f64;
            d * d
        })
        .sum();
    let diff_rms = (diff_sq_sum / overlap as f64).sqrt();
    let out_sq_sum: f64 = out_pcm.iter().map(|&s| (s as f64) * (s as f64)).sum();
    let out_rms = (out_sq_sum / out_pcm.len() as f64).sqrt();
    println!("diff_rms={diff_rms:.6} out_rms={out_rms:.6}");
    assert!(diff_rms > 1e-4, "restored audio is suspiciously close to a passthrough: diff_rms={diff_rms:.6}");
    assert!(diff_rms < 0.5, "restored audio differs from the input at gross-corruption level: diff_rms={diff_rms:.6}");
    assert!(out_rms > 1e-3, "restored audio is suspiciously close to silence: out_rms={out_rms:.6}");

    // (c) no quality regression: no new clipping, cutoff doesn't collapse, LUFS stable.
    let after = measure_file(&ffmpeg, &restored_wav);
    println!(
        "after: cutoff={:?} lufs={:?} clip_ratio={:.5}",
        after.cutoff_hz, after.lufs, after.clip_ratio
    );
    println!(
        "cutoff before={:?} after={:?}",
        before.cutoff_hz.unwrap_or(22_050),
        after.cutoff_hz.unwrap_or(22_050)
    );
    assert!(after.clip_ratio < 0.01, "restoration introduced clipping: clip_ratio={:.5}", after.clip_ratio);
    let before_cut = before.cutoff_hz.unwrap_or(22_050);
    let after_cut = after.cutoff_hz.unwrap_or(22_050);
    assert!(
        after_cut as i64 >= before_cut as i64 - 1_500,
        "restoration narrowed the band beyond measurement noise: before={before_cut} after={after_cut}"
    );
    if let (Some(before_lufs), Some(after_lufs)) = (before.lufs, after.lufs) {
        assert!(
            (after_lufs - before_lufs).abs() <= 3.0,
            "restoration shifted loudness beyond expectations: before={before_lufs:.2} after={after_lufs:.2}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
