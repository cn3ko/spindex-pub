//! Diagnostic: what does SpinDex's own tag reader see as a file's key?
//! Ignored by default.
//!
//!   SPINDEX_KEY_FILES="C:\a.wav;D:\b.aiff" \
//!     cargo test --test read_key_live -- --ignored --nocapture
//!
//! ffmpeg and lofty do not always agree: a WAV can carry both an ID3 chunk
//! and a RIFF INFO chunk, and whichever one the reader prefers is the value
//! the scanner writes into the database.
use spindex::core::metadata;

#[test]
#[ignore]
fn print_the_key_each_file_reports() {
    let Ok(list) = std::env::var("SPINDEX_KEY_FILES") else {
        panic!("set SPINDEX_KEY_FILES to one or more paths separated by ';'");
    };
    for path in list.split(';').filter(|s| !s.trim().is_empty()) {
        let p = std::path::Path::new(path.trim());
        match metadata::read(p) {
            Ok(m) => println!(
                "key={:?} bpm={:?} artist={:?}\n    {}",
                m.key,
                m.bpm,
                m.artist,
                p.display()
            ),
            Err(e) => println!("ERROR {e:#}\n    {}", p.display()),
        }
    }
}
