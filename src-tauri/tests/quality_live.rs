//! Live quality diagnostic on a real audio file: decodes with the bundled
//! ffmpeg and prints what the quality pipeline measures. Calibration aid for
//! the spectral-cutoff edge detector.
//!
//!   SPINDEX_QUALITY_FILE="D:\...\track.mp3" cargo test --test quality_live -- --ignored --nocapture

use spindex::core::analysis::{decode_pcm_stereo, find_ffmpeg};
use spindex::core::quality::{measure_stereo, QUALITY_SAMPLE_RATE};

#[test]
#[ignore]
fn quality_of_live_file() {
    let Ok(file) = std::env::var("SPINDEX_QUALITY_FILE") else {
        eprintln!("SPINDEX_QUALITY_FILE not set, skipping");
        return;
    };
    let ffmpeg = find_ffmpeg().expect("bundled ffmpeg not found");
    let pcm = decode_pcm_stereo(&ffmpeg, std::path::Path::new(&file), QUALITY_SAMPLE_RATE)
        .expect("decode failed");
    let q = measure_stereo(&pcm);
    eprintln!(
        "{file}\n  lufs={:?}  clip_ratio={:.5}  cutoff_hz={:?}",
        q.lufs, q.clip_ratio, q.cutoff_hz
    );
}
