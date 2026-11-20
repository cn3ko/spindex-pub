//! Diagnostic sweep for the "cannot save anymore, UTF-16 error" report:
//! run the metadata-editor write path TWICE against every audio file under
//! SPINDEX_SWEEP_DIR (on temp copies, originals untouched).
//!
//! Phase 1 failures = pre-existing tag the write path refuses (old behavior,
//! intended for mangled tags). Phase 2 failures = a file whose FIRST save
//! (our ID3v2.3 output) produced a tag the next save can't read back —
//! a self-inflicted regression.
//!
//!   SPINDEX_SWEEP_DIR=D:\ cargo test --test edit_twice_sweep -- --ignored --nocapture

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use spindex::core::metadata::{self, TagUpdate};

fn update_from(path: &Path, key: &str) -> TagUpdate {
    let existing = metadata::read(path).ok();
    let m = existing.as_ref();
    TagUpdate {
        title: m.and_then(|m| m.title.clone()),
        artist: m.and_then(|m| m.artist.clone()),
        album: m.and_then(|m| m.album.clone()),
        genre: m.and_then(|m| m.genre.clone()),
        year: m.and_then(|m| m.year),
        bpm: m.and_then(|m| m.bpm),
        key: Some(key.into()),
        comment: m.and_then(|m| m.comment.clone()),
        label: m.and_then(|m| m.label.clone()),
        rating: None,
    }
}

#[test]
#[ignore]
fn sweep_edit_twice() {
    let Ok(root) = std::env::var("SPINDEX_SWEEP_DIR") else {
        eprintln!("SPINDEX_SWEEP_DIR not set, skipping");
        return;
    };

    let last_panic: &'static Mutex<Option<String>> = Box::leak(Box::new(Mutex::new(None)));
    let hook_slot = last_panic;
    panic::set_hook(Box::new(move |info| {
        *hook_slot.lock().unwrap() = Some(format!("{info}"));
    }));

    let tmp = std::env::temp_dir().join("spindex-edit-twice");
    std::fs::create_dir_all(&tmp).unwrap();

    let mut total = 0u32;
    // (phase, source path, message)
    let mut failures: Vec<(&'static str, PathBuf, String)> = Vec::new();

    for entry in walkdir::WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() || !metadata::is_audio_file(path) {
            continue;
        }
        if path.components().any(|c| c.as_os_str() == "_Serato_") {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        total += 1;

        let copy = tmp.join(format!("sweep.{ext}"));
        let _ = std::fs::remove_file(&copy);
        if std::fs::copy(path, &copy).is_err() {
            continue;
        }

        let run = |phase: &'static str, key: &str, failures: &mut Vec<_>| -> bool {
            let update = update_from(&copy, key).normalized();
            let result =
                panic::catch_unwind(AssertUnwindSafe(|| metadata::write(&copy, &update)));
            match result {
                Err(_) => {
                    let msg = last_panic
                        .lock()
                        .unwrap()
                        .take()
                        .unwrap_or_else(|| "<no message captured>".into());
                    failures.push((phase, path.to_path_buf(), format!("PANIC {msg}")));
                    false
                }
                Ok(Err(e)) => {
                    failures.push((phase, path.to_path_buf(), format!("{e:#}")));
                    false
                }
                Ok(Ok(())) => {
                    let back = metadata::read(&copy).ok().and_then(|m| m.key);
                    if back.as_deref() != Some(key) {
                        failures.push((
                            phase,
                            path.to_path_buf(),
                            format!("write ok but key read back as {back:?}"),
                        ));
                        false
                    } else {
                        true
                    }
                }
            }
        };

        if run("first-edit", "8A", &mut failures) {
            run("second-edit", "9A", &mut failures);
        }
        let _ = std::fs::remove_file(&copy);
    }
    let _ = panic::take_hook();

    let count = |phase: &str| failures.iter().filter(|(p, _, _)| *p == phase).count();
    println!(
        "\nswept {total} files: {} first-edit failures, {} second-edit failures",
        count("first-edit"),
        count("second-edit")
    );
    for (phase, p, m) in &failures {
        println!("{phase:11}  {}\n             {}", p.display(), m);
    }
    assert_eq!(
        count("second-edit"),
        0,
        "files our own v2.3 save made un-editable exist"
    );
}
