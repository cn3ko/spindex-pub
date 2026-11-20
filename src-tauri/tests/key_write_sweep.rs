//! Diagnostic sweep: run the metadata-editor write path against every audio
//! file under SPINDEX_SWEEP_DIR (on temp copies, originals untouched) and
//! report files that panic or error. Env-gated like dupe_bench.
//!
//!   SPINDEX_SWEEP_DIR=D:\ cargo test --test key_write_sweep -- --ignored --nocapture

use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Mutex;

use spindex::core::metadata::{self, TagUpdate};


#[test]
#[ignore]
fn sweep_key_write() {
    let Ok(root) = std::env::var("SPINDEX_SWEEP_DIR") else {
        eprintln!("SPINDEX_SWEEP_DIR not set, skipping");
        return;
    };

    // Capture panic messages instead of the default stderr dump.
    let last_panic: &'static Mutex<Option<String>> =
        Box::leak(Box::new(Mutex::new(None)));
    let hook_slot = last_panic;
    panic::set_hook(Box::new(move |info| {
        let msg = format!("{info}");
        *hook_slot.lock().unwrap() = Some(msg);
    }));

    let tmp = std::env::temp_dir().join("spindex-key-sweep");
    std::fs::create_dir_all(&tmp).unwrap();

    let mut total = 0u32;
    let mut panics: Vec<(PathBuf, String)> = Vec::new();
    let mut errors: Vec<(PathBuf, String)> = Vec::new();

    for entry in walkdir::WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if !metadata::is_audio_file(path) {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        // Skip Serato internals just like the scanner's folder logic would.
        if path.components().any(|c| c.as_os_str() == "_Serato_") {
            continue;
        }
        total += 1;

        let copy = tmp.join(format!("sweep.{ext}"));
        if std::fs::copy(path, &copy).is_err() {
            continue;
        }
        // Mirror the editor: keep existing tags, change only the key.
        let existing = metadata::read(path).ok();
        let m = existing.as_ref();
        let update = TagUpdate {
            title: m.and_then(|m| m.title.clone()),
            artist: m.and_then(|m| m.artist.clone()),
            album: m.and_then(|m| m.album.clone()),
            genre: m.and_then(|m| m.genre.clone()),
            year: m.and_then(|m| m.year),
            bpm: m.and_then(|m| m.bpm),
            key: Some("8A".into()),
            comment: m.and_then(|m| m.comment.clone()),
            label: m.and_then(|m| m.label.clone()),
            rating: None,
        };
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            metadata::write(&copy, &update.normalized())
        }));
        match result {
            Err(_) => {
                let msg = last_panic
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap_or_else(|| "<no message captured>".into());
                panics.push((path.to_path_buf(), msg));
            }
            Ok(Err(e)) => errors.push((path.to_path_buf(), format!("{e:#}"))),
            Ok(Ok(())) => {
                // The write must actually be readable afterwards.
                let back = metadata::read(&copy)
                    .ok()
                    .and_then(|m| m.key);
                if back.as_deref() != Some("8A") {
                    errors.push((
                        path.to_path_buf(),
                        format!("write succeeded but key read back as {back:?}"),
                    ));
                }
            }
        }
        let _ = std::fs::remove_file(&copy);
    }
    let _ = panic::take_hook();

    println!("\nswept {total} files: {} panics, {} errors", panics.len(), errors.len());
    for (p, m) in &panics {
        println!("PANIC  {}\n       {}", p.display(), m);
    }
    for (p, m) in &errors {
        println!("ERROR  {}\n       {}", p.display(), m);
    }
    assert!(panics.is_empty(), "files that panic the write path exist");
}
