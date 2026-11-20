use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::core::{
    analysis, copying, db, dupes, enhance, enrich, metadata, quality, scanner, tempo, trash,
};
use crate::core::crate_order;
use crate::core::grid_edit;
use crate::core::restore;
use crate::core::serato_intent;
use crate::core::stick_path::resolve_crate_entries;
use anyhow::Context;

pub struct AppState {
    pub db_path: PathBuf,
    pub conn: Mutex<rusqlite::Connection>,
    /// The file the deck was last given, so aiff previews older than it and the
    /// current one can be pruned. Paired with the sequence number of the
    /// `prepare_playback` call that last won the race (see `play_seq`):
    /// calls run concurrently, so a slow aiff transcode can finish after the
    /// call it was superseded by.
    ///
    /// Played files are NOT revoked from the asset scope: in Tauri a forbidden
    /// path stays forbidden even after a later `allow_file`, which made every
    /// track played earlier in a session unplayable.
    pub last_play: Mutex<(u64, Option<PathBuf>)>,
    /// Issues the sequence numbers `prepare_playback` uses to detect
    /// out-of-order completion.
    pub play_seq: std::sync::atomic::AtomicU64,
    /// The grid editor's last decode (track id, samples), kept from
    /// `grid_waveform` until `grid_colours` takes it, so the colourings don't
    /// decode the file a second time. One track at most (~8 MB for 3 minutes).
    pub grid_pcm: Mutex<Option<(i64, std::sync::Arc<Vec<f32>>)>>,
}

/// A `prepare_playback` completion only gets to update the shared playback
/// state (and prune previews) if no later-issued call has already done so —
/// otherwise a slow call for a track the user has already skipped past could
/// delete the preview that's actually loaded.
fn play_seq_wins(candidate: u64, latest_applied: u64) -> bool {
    candidate >= latest_applied
}

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

const LOCK_ERR: &str = "database lock poisoned";

/// Scratch directory for files the webview reads but nobody keeps: extracted
/// artwork and the transcoded aiff preview.
///
/// A subdirectory rather than `%TEMP%` itself, so `clear_temp_cache` can empty
/// it wholesale without ever touching a file SpinDex did not write.
pub fn temp_cache_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("spindex-cache");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Empties the scratch directory. Called once at startup, because these files
/// otherwise accumulate for the life of the machine — `artwork_path` writes one
/// per track whose cover is ever displayed.
///
/// Deleting them is always safe: every one is regenerated on demand from the
/// audio file, which is the source of truth. Best-effort throughout — a file
/// still held open by a previous instance is simply skipped.
pub fn clear_temp_cache() {
    let dir = temp_cache_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

const PREVIEW_PREFIX: &str = "spindex-preview-";

/// The transcoded aiff preview for one `prepare_playback` call. One file per
/// call, never shared: ffmpeg truncates its output on open, and the deck is
/// still streaming the previous preview while the next one transcodes.
fn preview_wav_path(dir: &Path, seq: u64) -> PathBuf {
    dir.join(format!("{PREVIEW_PREFIX}{seq}.wav"))
}

fn is_preview_wav(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(PREVIEW_PREFIX) && n.ends_with(".wav"))
}

/// Removes every aiff preview in `dir` except `keep`. Best-effort: a file
/// still open elsewhere is left for the startup sweep.
fn prune_previews(dir: &Path, keep: &[PathBuf]) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if is_preview_wav(&p) && !keep.contains(&p) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

#[tauri::command]
pub fn list_libraries(state: State<AppState>) -> Result<Vec<db::LibraryInfo>, String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    db::list_libraries(&conn).map_err(err)
}

#[tauri::command]
pub fn add_library(path: String, state: State<AppState>) -> Result<db::LibraryInfo, String> {
    let p = PathBuf::from(&path);
    if !p.is_dir() {
        return Err(format!("not a folder: {path}"));
    }
    let canonical = p
        .canonicalize()
        .map_err(|e| format!("resolving {path}: {e}"))?;
    let root = scanner::clean_path_string(&canonical);
    let name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| root.clone());

    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let id = db::ensure_library(&conn, &name, &root).map_err(err)?;
    db::list_libraries(&conn)
        .map_err(err)?
        .into_iter()
        .find(|l| l.id == id)
        .ok_or_else(|| "library not found after insert".to_string())
}

#[derive(Serialize, Clone)]
pub struct ScanProgress {
    pub library_id: i64,
    pub found: u64,
}

#[derive(Serialize, Clone)]
pub struct ScanResult {
    pub library_id: i64,
    pub found: u64,
    pub new: u64,
    pub updated: u64,
    pub unchanged: u64,
    pub errors: u64,
    pub elapsed_secs: f64,
}

/// Scans on a blocking thread with its own connection (WAL allows readers
/// to keep searching meanwhile), emitting `scan:progress` events along the way.
#[tauri::command]
pub async fn scan_library(
    library_id: i64,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ScanResult, String> {
    let root = {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        db::library_root(&conn, library_id).map_err(err)?
    };
    let db_path = state.db_path.clone();

    let progress_app = app.clone();
    let summary = tauri::async_runtime::spawn_blocking(move || {
        let mut conn = db::open(&db_path)?;
        scanner::scan_folder_with_progress(&mut conn, Path::new(&root), |found| {
            let _ = progress_app.emit("scan:progress", ScanProgress { library_id, found });
        })
    })
    .await
    .map_err(|e| format!("scan task failed: {e}"))?
    .map_err(err)?;

    let result = ScanResult {
        library_id,
        found: summary.found,
        new: summary.new,
        updated: summary.updated,
        unchanged: summary.unchanged,
        errors: summary.errors,
        elapsed_secs: summary.elapsed_secs,
    };
    let _ = app.emit("scan:done", result.clone());
    Ok(result)
}

#[derive(Default)]
pub struct AnalysisState {
    pub running: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Serialize, Clone)]
pub struct AnalysisProgress {
    pub done: u64,
    pub failed: u64,
    pub total: u64,
}

#[tauri::command]
pub fn analyzer_available() -> bool {
    analysis::find_sidecars().is_some()
}

#[tauri::command]
pub fn cancel_analysis(analysis_state: State<AnalysisState>) {
    analysis_state.cancel.store(true, Ordering::SeqCst);
}

/// Queues tracks for BPM/key/waveform analysis on background worker threads.
/// Returns the number of queued tracks; progress arrives via events.
#[tauri::command]
pub fn start_analysis(
    track_ids: Option<Vec<i64>>,
    library_id: Option<i64>,
    overwrite: bool,
    app: AppHandle,
    state: State<AppState>,
    analysis_state: State<AnalysisState>,
) -> Result<u64, String> {
    let sidecars = analysis::find_sidecars().ok_or_else(|| {
        "analyzer binaries or models missing — run scripts/fetch-ffmpeg.ps1, scripts/fetch-models.ps1 and scripts/convert-key-model.py"
            .to_string()
    })?;
    if analysis_state.running.swap(true, Ordering::SeqCst) {
        return Err("analysis is already running".into());
    }
    analysis_state.cancel.store(false, Ordering::SeqCst);

    let items = {
        let conn = match state.conn.lock() {
            Ok(c) => c,
            Err(_) => {
                analysis_state.running.store(false, Ordering::SeqCst);
                return Err(LOCK_ERR.to_string());
            }
        };
        db::tracks_for_analysis(&conn, track_ids.as_deref(), library_id, !overwrite)
            .map_err(err)
            .inspect_err(|_| analysis_state.running.store(false, Ordering::SeqCst))?
    };

    let total = items.len() as u64;
    if total == 0 {
        analysis_state.running.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "analysis:done",
            AnalysisProgress { done: 0, failed: 0, total: 0 },
        );
        return Ok(0);
    }

    let workers = std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(1))
        .unwrap_or(1)
        .min(items.len());
    let queue = Arc::new(Mutex::new(VecDeque::from(items)));
    let done = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let alive = Arc::new(AtomicU64::new(workers as u64));
    let sidecars = Arc::new(sidecars);
    let running = analysis_state.running.clone();
    let cancel = analysis_state.cancel.clone();

    for _ in 0..workers {
        let queue = queue.clone();
        let done = done.clone();
        let failed = failed.clone();
        let alive = alive.clone();
        let sidecars = sidecars.clone();
        let running = running.clone();
        let cancel = cancel.clone();
        let app = app.clone();
        let db_path = state.db_path.clone();

        std::thread::spawn(move || {
            // Each worker gets its own connection; WAL keeps readers unblocked.
            let conn = db::open(&db_path);
            if let Ok(conn) = conn {
                while !cancel.load(Ordering::SeqCst) {
                    let item = queue.lock().ok().and_then(|mut q| q.pop_front());
                    let Some((track_id, path)) = item else { break };
                    match analysis::analyze_file(&sidecars, &path) {
                        Ok(res) => {
                            let candidates_json = res
                                .key_candidates
                                .as_ref()
                                .and_then(|c| serde_json::to_string(c).ok());
                            let saved = db::save_analysis(
                                &conn,
                                track_id,
                                res.bpm,
                                res.key_camelot.as_deref(),
                                res.key_confidence,
                                candidates_json.as_deref(),
                                &res.peaks,
                                res.bands.as_deref(),
                                res.beats.as_deref(),
                                res.downbeats.as_deref(),
                                overwrite,
                            );
                            let ok = saved.is_ok();
                            match saved {
                                Ok(()) => done.fetch_add(1, Ordering::SeqCst),
                                Err(_) => failed.fetch_add(1, Ordering::SeqCst),
                            };

                            if ok {
                                // Quality metrics ride along on every
                                // successfully-saved analysis: a real stereo
                                // decode (BS.1770 sums per-channel energy,
                                // unlike a mono downmix) at
                                // QUALITY_SAMPLE_RATE, since the analyzer
                                // feed's 11 kHz Nyquist hides cutoffs.
                                match analysis::decode_pcm_stereo(
                                    &sidecars.ffmpeg,
                                    &path,
                                    crate::core::quality::QUALITY_SAMPLE_RATE,
                                ) {
                                    Ok(interleaved) => {
                                        let q = crate::core::quality::measure_stereo(&interleaved);
                                        if let Err(e) = db::save_quality(
                                            &conn, track_id, q.lufs, q.clip_ratio,
                                            q.cutoff_hz, &q.spectrogram,
                                        ) {
                                            tracing::warn!(
                                                "saving quality for {}: {e:#}", path.display()
                                            );
                                        }
                                    }
                                    Err(e) => tracing::warn!(
                                        "quality decode failed for {}: {e:#}", path.display()
                                    ),
                                }
                            }
                        }
                        Err(e) => {
                            let msg = format!("{e:#}");
                            tracing::warn!("analysis failed for {}: {msg}", path.display());
                            let _ = db::save_analysis_error(&conn, track_id, &msg);
                            failed.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    let _ = app.emit(
                        "analysis:progress",
                        AnalysisProgress {
                            done: done.load(Ordering::SeqCst),
                            failed: failed.load(Ordering::SeqCst),
                            total,
                        },
                    );
                }
            }
            if alive.fetch_sub(1, Ordering::SeqCst) == 1 {
                running.store(false, Ordering::SeqCst);
                let _ = app.emit(
                    "analysis:done",
                    AnalysisProgress {
                        done: done.load(Ordering::SeqCst),
                        failed: failed.load(Ordering::SeqCst),
                        total,
                    },
                );
            }
        });
    }
    Ok(total)
}

#[derive(Serialize)]
pub struct PlaybackInfo {
    pub path: String,
    pub transcoded: bool,
}

/// Resolves a track to a file the webview can play and whitelists it on the
/// asset protocol scope. aiff gets transcoded to a temp wav via ffmpeg.
///
/// Async + spawn_blocking: a sync command runs on the main thread, and the
/// aiff transcode (1-2 s for a 100 MB file on a stick) froze the whole UI on
/// every track switch.
#[tauri::command]
pub async fn prepare_playback(
    track_id: i64,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PlaybackInfo, String> {
    use tauri::Manager;

    // Claimed before any slow work (the aiff transcode below), so completion
    // order can be checked against invocation order later.
    let my_seq = state.play_seq.fetch_add(1, Ordering::SeqCst) + 1;

    let abs = {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        db::track_abs_path(&conn, track_id).map_err(err)?
    };
    let format = abs
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let needs_transcode = matches!(format.as_str(), "aiff" | "aif");

    let source = abs.clone();
    let prepared = tauri::async_runtime::spawn_blocking(move || -> Result<Option<PathBuf>, String> {
        if !source.is_file() {
            return Ok(None);
        }
        if !needs_transcode {
            return Ok(Some(source));
        }
        let ffmpeg = analysis::find_ffmpeg().ok_or_else(|| {
            "aiff preview needs ffmpeg — run scripts/fetch-ffmpeg.ps1".to_string()
        })?;
        let temp = preview_wav_path(&temp_cache_dir(), my_seq);
        analysis::transcode_to_wav(&ffmpeg, &source, &temp).map_err(err)?;
        Ok(Some(temp))
    })
    .await
    .map_err(|e| format!("playback task failed: {e}"))??;

    let Some(play_path) = prepared else {
        // Only a verifiably gone file is flagged: a tag save that has the
        // file renamed aside for a moment must not grey it out.
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        return Err(if scanner::recheck_track(&conn, track_id).unwrap_or(false) {
            format!("file not found: {}", abs.display())
        } else {
            "the file is being rewritten right now, try again in a moment".to_string()
        });
    };

    app.asset_protocol_scope()
        .allow_file(&play_path)
        .map_err(|e| format!("allowing playback path: {e}"))?;

    // Played files are never revoked from the asset scope: Tauri's forbidden
    // list always wins over its allowed list and nothing removes an entry, so
    // a forbidden file stays unplayable for the rest of the session even after
    // `allow_file` ("no supported source" when a track is played again).
    //
    // Only the most recently issued call to finish prunes aiff previews:
    // `prepare_playback` runs concurrently, so a slow transcode for a track
    // the user already skipped past can finish after the one now playing.
    if let Ok(mut last) = state.last_play.lock() {
        if play_seq_wins(my_seq, last.0) {
            let prev = last.1.replace(play_path.clone());
            last.0 = my_seq;
            let mut keep = vec![play_path.clone()];
            if let Some(prev) = prev {
                // The previous preview stays until the next switch: the deck
                // is still playing it until this call returns.
                keep.push(prev);
            }
            prune_previews(&temp_cache_dir(), &keep);
        } else if needs_transcode {
            // Superseded before it finished: nobody will ever load this one.
            let _ = std::fs::remove_file(&play_path);
        }
    }

    Ok(PlaybackInfo {
        path: play_path.to_string_lossy().to_string(),
        transcoded: needs_transcode,
    })
}

/// Writes edited tags to the audio file first (the source of truth), then
/// mirrors them into the cache with fresh file stamps. The DB is only
/// touched after the file write succeeded.
#[tauri::command]
pub fn update_track_metadata(
    track_id: i64,
    update: metadata::TagUpdate,
    state: State<AppState>,
) -> Result<(), String> {
    let update = update.normalized();
    let abs = {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        db::track_abs_path(&conn, track_id).map_err(err)?
    };
    // File I/O happens without holding the connection lock.
    metadata::write(&abs, &update).map_err(err)?;
    let fs_meta =
        std::fs::metadata(&abs).map_err(|e| format!("reading file stamps: {e}"))?;
    let modified_at = fs_meta
        .modified()
        .map(scanner::mtime_string)
        .map_err(|e| format!("reading mtime: {e}"))?;

    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    db::apply_metadata_update(&conn, track_id, &update, fs_meta.len() as i64, &modified_at)
        .map_err(err)
}

/// Looks up online metadata for a track by artist + title (MusicBrainz text
/// search). Read-only: returns proposed field values, writes nothing.
/// Any lookup failure returns a result with source "none", never an error.
///
/// Runs on a blocking thread (mirrors `scan_library`): a chain of 10s HTTP calls would otherwise stall the Tauri event loop, since
/// non-async commands run on the main thread.
#[tauri::command]
pub async fn fetch_online_metadata(
    track_id: i64,
    state: State<'_, AppState>,
) -> Result<enrich::EnrichResult, String> {
    let (artist, title) = {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        let (artist, title): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT artist, title FROM track WHERE id = ?1",
                [track_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| err(e.into()))?;
        (artist, title)
        // conn (MutexGuard) dropped here, before the blocking network call.
    };
    tauri::async_runtime::spawn_blocking(move || {
        enrich::enrich_track(artist.as_deref(), title.as_deref())
    })
    .await
    .map_err(|e| format!("enrich task failed: {e}"))
}

/// Extracts embedded artwork to a temp file the webview can display via the
/// asset protocol. Returns None when the track has no artwork.
/// Async + spawn_blocking (mirrors `scan_library`): the track table calls this
/// once per visible row while scrolling, and both the tag read and the temp
/// write hit the disk — on a sleeping USB stick a sync command froze the whole
/// UI for the duration.
#[tauri::command]
pub async fn artwork_path(
    track_id: i64,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    use tauri::Manager;
    let abs = {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        db::track_abs_path(&conn, track_id).map_err(err)?
    }; // lock dropped before the (possibly slow) disk work below

    let temp = tauri::async_runtime::spawn_blocking(move || -> Result<Option<PathBuf>, String> {
        let Some((mime, data)) = metadata::read_artwork(&abs).map_err(err)? else {
            return Ok(None);
        };
        let ext = if mime.contains("png") { "png" } else { "jpg" };
        let temp = temp_cache_dir().join(format!("spindex-art-{track_id}.{ext}"));
        std::fs::write(&temp, data).map_err(|e| format!("writing artwork cache: {e}"))?;
        Ok(Some(temp))
    })
    .await
    .map_err(|e| format!("artwork task failed: {e}"))??;

    let Some(temp) = temp else {
        return Ok(None);
    };
    app.asset_protocol_scope()
        .allow_file(&temp)
        .map_err(|e| format!("allowing artwork path: {e}"))?;
    Ok(Some(temp.to_string_lossy().to_string()))
}

/// Embeds an image file as the track's front cover (replaces all pictures).
#[tauri::command]
pub fn set_artwork(
    track_id: i64,
    image_path: String,
    state: State<AppState>,
) -> Result<(), String> {
    let image = PathBuf::from(&image_path);
    let mime = metadata::image_mime_for(&image)
        .ok_or_else(|| "unsupported image type — use jpg or png".to_string())?;
    let data = std::fs::read(&image).map_err(|e| format!("reading {image_path}: {e}"))?;
    const MAX_BYTES: usize = 20 * 1024 * 1024;
    if data.len() > MAX_BYTES {
        return Err("image larger than 20 MB — pick a smaller cover".to_string());
    }

    let abs = {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        db::track_abs_path(&conn, track_id).map_err(err)?
    };
    metadata::write_artwork(&abs, Some((data, mime))).map_err(err)?;
    finish_artwork_update(&state, track_id, &abs, true)
}

/// Removes all embedded pictures from the track's file.
#[tauri::command]
pub fn remove_artwork(track_id: i64, state: State<AppState>) -> Result<(), String> {
    let abs = {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        db::track_abs_path(&conn, track_id).map_err(err)?
    };
    metadata::write_artwork(&abs, None).map_err(err)?;
    finish_artwork_update(&state, track_id, &abs, false)
}

fn finish_artwork_update(
    state: &State<AppState>,
    track_id: i64,
    abs: &Path,
    has_artwork: bool,
) -> Result<(), String> {
    let fs_meta = std::fs::metadata(abs).map_err(|e| format!("reading file stamps: {e}"))?;
    let modified_at = fs_meta
        .modified()
        .map(scanner::mtime_string)
        .map_err(|e| format!("reading mtime: {e}"))?;
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    db::update_artwork_flag(&conn, track_id, has_artwork, fs_meta.len() as i64, &modified_at)
        .map_err(err)
}

#[tauri::command]
pub fn get_waveform(track_id: i64, state: State<AppState>) -> Result<Option<Vec<u8>>, String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    db::get_waveform(&conn, track_id).map_err(err)
}

#[tauri::command]
pub fn waveform_bands(track_id: i64, state: State<AppState>) -> Result<Option<Vec<u8>>, String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    db::waveform_bands(&conn, track_id).map_err(err)
}

/// Stored (sampled) beat times, for the tempo dialog's readout.
#[tauri::command]
pub fn beat_grid(track_id: i64, state: State<AppState>) -> Result<Option<Vec<f64>>, String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    db::beat_grid(&conn, track_id).map_err(err)
}

/// Stored (sampled) downbeat times, for the beat-grid tooling.
#[tauri::command]
pub fn downbeat_grid(track_id: i64, state: State<AppState>) -> Result<Option<Vec<f64>>, String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    db::downbeat_grid(&conn, track_id).map_err(err)
}

/// Quality metrics + spectrogram blob for the quality panel.
#[tauri::command]
pub fn quality_detail(track_id: i64, state: State<AppState>) -> Result<db::QualityDetail, String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    db::quality_detail(&conn, track_id).map_err(err)
}

#[tauri::command]
pub fn mark_missing(track_id: i64, state: State<AppState>) -> Result<(), String> {
    // The deck calls this on ANY playback failure, and most of those are not
    // a missing file (the loaded track's file was just rewritten by a tag
    // save, an interrupted load, a failed aiff transcode). So the disk
    // decides: only a verifiably gone file is flagged, and a present one has
    // any stale flag cleared.
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    scanner::recheck_track(&conn, track_id).map(|_| ()).map_err(err)
}

#[derive(Default)]
pub struct HashState {
    pub running: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Serialize, Clone)]
pub struct HashProgress {
    pub done: u64,
    pub failed: u64,
    pub total: u64,
}

#[tauri::command]
pub fn cancel_hashing(hash_state: State<HashState>) {
    hash_state.cancel.store(true, Ordering::SeqCst);
}

/// Queues unhashed tracks for SHA-256 content hashing on background worker
/// threads. Returns the number queued; progress arrives via hash:* events.
#[tauri::command]
pub fn start_hashing(
    library_ids: Option<Vec<i64>>,
    app: AppHandle,
    state: State<AppState>,
    hash_state: State<HashState>,
) -> Result<u64, String> {
    if hash_state.running.swap(true, Ordering::SeqCst) {
        return Err("hashing is already running".into());
    }
    hash_state.cancel.store(false, Ordering::SeqCst);

    let items = {
        let conn = match state.conn.lock() {
            Ok(c) => c,
            Err(_) => {
                hash_state.running.store(false, Ordering::SeqCst);
                return Err(LOCK_ERR.to_string());
            }
        };
        db::tracks_for_hashing(&conn, library_ids.as_deref())
            .map_err(err)
            .inspect_err(|_| hash_state.running.store(false, Ordering::SeqCst))?
    };

    let total = items.len() as u64;
    if total == 0 {
        hash_state.running.store(false, Ordering::SeqCst);
        let _ = app.emit("hash:done", HashProgress { done: 0, failed: 0, total: 0 });
        return Ok(0);
    }

    // Hashing is disk-bound; a few workers keep the drive busy without
    // starving the UI.
    let workers = std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(1))
        .unwrap_or(1)
        .min(4)
        .min(items.len());
    let queue = Arc::new(Mutex::new(VecDeque::from(items)));
    let done = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let alive = Arc::new(AtomicU64::new(workers as u64));
    let running = hash_state.running.clone();
    let cancel = hash_state.cancel.clone();

    for _ in 0..workers {
        let queue = queue.clone();
        let done = done.clone();
        let failed = failed.clone();
        let alive = alive.clone();
        let running = running.clone();
        let cancel = cancel.clone();
        let app = app.clone();
        let db_path = state.db_path.clone();

        std::thread::spawn(move || {
            // Each worker gets its own connection; WAL keeps readers unblocked.
            if let Ok(conn) = db::open(&db_path) {
                while !cancel.load(Ordering::SeqCst) {
                    let item = queue.lock().ok().and_then(|mut q| q.pop_front());
                    let Some((track_id, path)) = item else { break };
                    match dupes::hash_file(&path) {
                        Ok(hex) => match db::save_sha256(&conn, track_id, &hex) {
                            Ok(()) => done.fetch_add(1, Ordering::SeqCst),
                            Err(_) => failed.fetch_add(1, Ordering::SeqCst),
                        },
                        Err(e) => {
                            tracing::warn!("hashing failed for {}: {e:#}", path.display());
                            failed.fetch_add(1, Ordering::SeqCst)
                        }
                    };
                    let _ = app.emit(
                        "hash:progress",
                        HashProgress {
                            done: done.load(Ordering::SeqCst),
                            failed: failed.load(Ordering::SeqCst),
                            total,
                        },
                    );
                }
            }
            if alive.fetch_sub(1, Ordering::SeqCst) == 1 {
                running.store(false, Ordering::SeqCst);
                let _ = app.emit(
                    "hash:done",
                    HashProgress {
                        done: done.load(Ordering::SeqCst),
                        failed: failed.load(Ordering::SeqCst),
                        total,
                    },
                );
            }
        });
    }
    Ok(total)
}

#[tauri::command]
pub fn compare_library(
    library_id: i64,
    state: State<AppState>,
) -> Result<Vec<dupes::LibraryComparison>, String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    dupes::compare_library(&conn, library_id).map_err(err)
}

#[tauri::command]
pub fn compare_details(
    source_id: i64,
    target_id: i64,
    level: Option<String>,
    limit: i64,
    offset: i64,
    state: State<AppState>,
) -> Result<dupes::MatchPage, String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    dupes::compare_details(
        &conn,
        source_id,
        target_id,
        level.as_deref(),
        limit.clamp(1, 5000),
        offset.max(0),
    )
    .map_err(err)
}

#[derive(Default)]
pub struct DevicesState {
    pub drives: Arc<Mutex<crate::core::devices::DriveSet>>,
}

#[derive(Serialize, Clone)]
pub struct DeviceInfo {
    pub letter: String,
    pub label: String,
    pub serial: Option<String>,
    pub known_library_id: Option<i64>,
}

#[derive(Serialize, Clone)]
pub struct DeviceArrived {
    pub letter: String,
    pub label: String,
    pub known_library_ids: Vec<i64>,
}

#[derive(Serialize, Clone)]
pub struct DeviceNeedsConfirm {
    pub letter: String,
    pub label: String,
    pub serial: String,
    pub candidate_library_id: i64,
    pub candidate_name: String,
}

#[derive(Serialize, Clone)]
pub struct DeviceRemoved {
    pub letter: String,
    pub library_ids: Vec<i64>,
}

/// Current snapshot of removable drives with their known-library mapping.
#[tauri::command]
pub fn list_devices(
    state: State<AppState>,
    devices_state: State<DevicesState>,
) -> Result<Vec<DeviceInfo>, String> {
    let drives = devices_state
        .drives
        .lock()
        .map_err(|_| LOCK_ERR.to_string())?
        .clone();
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let mut out = Vec::new();
    for d in drives.values() {
        let known = match &d.serial {
            Some(serial) => db::removable_libraries_for_serial(&conn, serial)
                .map_err(err)?
                .first()
                .copied(),
            None => None,
        };
        out.push(DeviceInfo {
            letter: d.letter.clone(),
            label: d.label.clone(),
            serial: d.serial.clone(),
            known_library_id: known,
        });
    }
    Ok(out)
}

/// Indexes a removable drive as one library per top-level audio folder
/// (plus a root library for loose files), all tagged with the stick's
/// volume serial. Idempotent: existing folders re-use their rows, new
/// folders are created — safe to re-run after adding folders to a stick.
/// The frontend follows up with scan_library per returned library.
#[tauri::command]
pub fn add_device_libraries(
    letter: String,
    state: State<AppState>,
    devices_state: State<DevicesState>,
) -> Result<Vec<db::LibraryInfo>, String> {
    let device = devices_state
        .drives
        .lock()
        .map_err(|_| LOCK_ERR.to_string())?
        .get(&letter)
        .cloned()
        .ok_or_else(|| format!("drive {letter} is not connected"))?;
    let drive_root = format!("{letter}\\");
    if !Path::new(&drive_root).is_dir() {
        return Err(format!("cannot read drive {letter}"));
    }
    let af = crate::core::devices::audio_folders(Path::new(&drive_root));

    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let mut ids = Vec::new();
    let mut register = |name: String, root: String| -> Result<(), String> {
        db::park_conflicting_removable(&conn, &root, device.serial.as_deref()).map_err(err)?;
        let id = db::ensure_library(&conn, &name, &root).map_err(err)?;
        db::set_library_removable(&conn, id, device.serial.as_deref()).map_err(err)?;
        ids.push(id);
        Ok(())
    };
    for folder in &af.folders {
        register(
            format!("{} ({})", folder, device.label),
            format!("{drive_root}{folder}\\"),
        )?;
    }
    if af.root_has_audio {
        // loose files directly in the stick root get their own library
        register(format!("{} (root)", device.label), drive_root.clone())?;
    }
    // No audio folders and no root audio: nothing is registered; the empty
    // Vec tells the frontend to leave the device listed with its button.
    let libs = db::list_libraries(&conn).map_err(err)?;
    Ok(libs.into_iter().filter(|l| ids.contains(&l.id)).collect())
}

/// Safe-removal of a stick. Blocking (the volume lock retries while files
/// are open, ~1.5s worst case), so it runs off the event loop.
#[tauri::command]
pub async fn eject_device(letter: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::core::devices::eject_drive(&letter).map_err(err)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Drops a stick's libraries and index (by volume serial). Audio files are
/// untouched — the device presents as fresh/unindexed afterwards.
#[tauri::command]
pub fn forget_device(serial: String, state: State<AppState>) -> Result<usize, String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    db::forget_device(&conn, &serial).map_err(err)
}

/// Unlinks one library from the index. Files on disk are never touched.
#[tauri::command]
pub fn remove_library(library_id: i64, state: State<AppState>) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    db::remove_library(&conn, library_id).map_err(err)
}

/// Crate paths come from our own tree, but never trust them blindly: only
/// plain forward-slash relative segments may reach the filesystem.
fn safe_crate_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Ok(());
    }
    let bad = path.contains('\\')
        || path.contains(':')
        || path.split('/').any(|s| s.is_empty() || s == "." || s == "..");
    if bad {
        return Err("invalid crate path".into());
    }
    Ok(())
}

/// Imports one audio file dropped in from Windows Explorer. The webview only
/// exposes a dropped file's bytes, never its path (`dragDropEnabled` is false
/// so internal HTML5 DnD keeps working), so the payload arrives as the raw
/// invoke body; filename, library and folder ride in URI-encoded headers.
/// Existing destinations are never overwritten. Returns "copied" | "skipped".
/// Async + spawn_blocking: a multi-hundred-MB drop onto a slow USB stick would
/// otherwise write the whole file on the event loop and freeze the window.
#[tauri::command]
pub async fn import_file(
    request: tauri::ipc::Request<'_>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected raw file bytes".into());
    };
    let header = |name: &str| -> Result<String, String> {
        let v = request
            .headers()
            .get(name)
            .ok_or_else(|| format!("missing {name} header"))?
            .to_str()
            .map_err(|_| format!("invalid {name} header"))?;
        copying::percent_decode(v).map_err(err)
    };
    let library_id: i64 = header("x-library-id")?
        .parse()
        .map_err(|_| "invalid x-library-id header".to_string())?;
    // Library-root drops send folder "", and the webview's HTTP layer DROPS
    // empty-valued headers entirely — so a missing x-folder means root, not
    // a protocol error (requiring it broke every drop onto a library row).
    let folder = match request.headers().get("x-folder") {
        Some(_) => header("x-folder")?,
        None => String::new(),
    };
    safe_crate_path(&folder)?;
    let filename = header("x-filename")?;
    let root = {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        db::library_root(&conn, library_id).map_err(err)?
    }; // lock dropped before the (possibly slow USB) write
    // Owned copy so the bytes can cross onto the blocking thread; the webview
    // already holds the whole file in memory, so this is a transient double.
    let bytes = bytes.clone();
    tauri::async_runtime::spawn_blocking(move || {
        match copying::import_bytes(Path::new(&root), &folder, &filename, &bytes).map_err(err)? {
            copying::CopyOutcome::Copied => Ok("copied".into()),
            copying::CopyOutcome::SkippedExists => Ok("skipped".into()),
            copying::CopyOutcome::Failed => Err("write failed".into()),
        }
    })
    .await
    .map_err(|e| format!("import task failed: {e}"))?
}

/// Creates a crate = a real folder under the library root (optionally inside
/// an existing crate). Returns the new crate's rel path.
#[tauri::command]
pub fn create_crate(
    library_id: i64,
    parent_path: String,
    name: String,
    state: State<AppState>,
) -> Result<String, String> {
    safe_crate_path(&parent_path)?;
    let name = copying::validate_folder_name(&name).map_err(err)?;
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let root = db::library_root(&conn, library_id).map_err(err)?;
    let parent_abs = if parent_path.is_empty() {
        PathBuf::from(&root)
    } else {
        Path::new(&root).join(&parent_path)
    };
    if !parent_abs.is_dir() {
        return Err("parent folder does not exist".into());
    }
    let target = parent_abs.join(&name);
    if target.exists() {
        return Err(format!("\"{name}\" already exists here"));
    }
    std::fs::create_dir(&target).map_err(|e| e.to_string())?;
    Ok(if parent_path.is_empty() {
        name
    } else {
        format!("{parent_path}/{name}")
    })
}

/// Renames a crate folder on disk and rewrites the rel_path of every track
/// under it. Returns the crate's new rel path.
#[tauri::command]
pub fn rename_crate(
    library_id: i64,
    path: String,
    new_name: String,
    state: State<AppState>,
) -> Result<String, String> {
    safe_crate_path(&path)?;
    if path.is_empty() {
        return Err("cannot rename the library root".into());
    }
    let new_name = copying::validate_folder_name(&new_name).map_err(err)?;
    let new_rel = match path.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{new_name}"),
        None => new_name.clone(),
    };
    if new_rel == path {
        return Ok(path);
    }
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let root = db::library_root(&conn, library_id).map_err(err)?;
    let old_abs = Path::new(&root).join(&path);
    let new_abs = Path::new(&root).join(&new_rel);
    if !old_abs.is_dir() {
        return Err("crate folder not found on disk".into());
    }
    // exists() would also trip on a case-only rename (NTFS is case-insensitive)
    let case_only = new_rel.to_lowercase() == path.to_lowercase();
    if !case_only && new_abs.exists() {
        return Err(format!("\"{new_name}\" already exists here"));
    }
    std::fs::rename(&old_abs, &new_abs).map_err(|e| e.to_string())?;
    db::rename_crate_paths(&conn, library_id, &path, &new_rel).map_err(err)?;
    Ok(new_rel)
}

/// Deletes a crate folder. `remove_dir` refuses non-empty directories — that
/// failure IS the safety guard; audio is never deleted through this path. The
/// one exception is SpinDex's own `.spindex-order.json` sidecar when it is all
/// that is left, which would otherwise make a hand-ordered but now empty-looking
/// crate undeletable — see `crate_order::delete_crate_dir`.
#[tauri::command]
pub fn delete_crate(library_id: i64, path: String, state: State<AppState>) -> Result<(), String> {
    safe_crate_path(&path)?;
    if path.is_empty() {
        return Err("cannot delete the library root".into());
    }
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let root = db::library_root(&conn, library_id).map_err(err)?;
    crate_order::delete_crate_dir(&Path::new(&root).join(&path))
        .map_err(|e| format!("could not delete the crate (it must be empty): {e}"))
}

#[derive(serde::Serialize)]
pub struct MoveResult {
    pub moved: u64,
    pub skipped: u64,
    pub failed: u64,
}

/// Moves tracks of ONE library into one of its crate folders ("" = library
/// root, i.e. "remove from crate"). A move is a same-volume rename: existing
/// destinations are skipped, cross-library ids are skipped (those are a copy,
/// not a move), and the file is never deleted.
#[tauri::command]
pub fn move_tracks_to_folder(
    track_ids: Vec<i64>,
    library_id: i64,
    folder: String,
    state: State<AppState>,
) -> Result<MoveResult, String> {
    safe_crate_path(&folder)?;
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let root = db::library_root(&conn, library_id).map_err(err)?;
    let dest_dir = if folder.is_empty() {
        PathBuf::from(&root)
    } else {
        Path::new(&root).join(&folder)
    };
    if !dest_dir.is_dir() {
        return Err("target crate folder does not exist".into());
    }
    let mut res = MoveResult { moved: 0, skipped: 0, failed: 0 };
    for id in track_ids {
        let row = conn
            .query_row(
                "SELECT library_id, rel_path FROM track WHERE id = ?1",
                params![id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some((lib, rel)) = row else {
            res.failed += 1;
            continue;
        };
        if lib != library_id {
            res.skipped += 1;
            continue;
        }
        let file_name = rel.rsplit('/').next().unwrap_or(rel.as_str());
        let new_rel = if folder.is_empty() {
            file_name.to_string()
        } else {
            format!("{folder}/{file_name}")
        };
        if new_rel == rel {
            res.skipped += 1;
            continue;
        }
        match copying::move_one(&Path::new(&root).join(&rel), &dest_dir) {
            copying::CopyOutcome::Copied => {
                // file is moved; a DB hiccup here is repaired by the next rescan
                match db::set_track_rel_path(&conn, id, &new_rel) {
                    Ok(()) => res.moved += 1,
                    Err(_) => res.failed += 1,
                }
            }
            copying::CopyOutcome::SkippedExists => res.skipped += 1,
            copying::CopyOutcome::Failed => res.failed += 1,
        }
    }
    Ok(res)
}

/// Moves the selected tracks one slot up (delta < 0) or down (delta > 0) inside
/// one folder-crate. Insert-and-renumber: positions stay a contiguous 1..N.
#[tauri::command]
pub fn move_crate_tracks(
    track_ids: Vec<i64>,
    library_id: i64,
    folder: String,
    delta: i32,
    state: State<AppState>,
) -> Result<(), String> {
    safe_crate_path(&folder)?;
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let order = crate_order::ensure_positions(&conn, library_id, &folder).map_err(err)?;
    let moved = crate_order::move_block(&order, &track_ids, delta);
    crate_order::apply_order(&conn, library_id, &folder, &moved).map_err(err)
}

/// Drops the selected tracks immediately before `before_index` (an index into
/// the crate's current order; `order.len()` means "at the end").
#[tauri::command]
pub fn reorder_crate_tracks(
    track_ids: Vec<i64>,
    library_id: i64,
    folder: String,
    before_index: usize,
    state: State<AppState>,
) -> Result<(), String> {
    safe_crate_path(&folder)?;
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let order = crate_order::ensure_positions(&conn, library_id, &folder).map_err(err)?;
    let moved = crate_order::insert_block(&order, &track_ids, before_index);
    crate_order::apply_order(&conn, library_id, &folder, &moved).map_err(err)
}

/// The crate's tracks in `order`, with the key and BPM the Sort helper needs.
fn sort_items(
    conn: &rusqlite::Connection,
    order: &[i64],
) -> Result<Vec<crate::core::crate_sort::Item>, String> {
    let mut stmt = conn
        .prepare_cached("SELECT key_camelot, bpm FROM track WHERE id = ?1")
        .map_err(|e| e.to_string())?;
    Ok(order
        .iter()
        .map(|&id| {
            let (key, bpm): (Option<String>, Option<f64>) = stmt
                .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap_or((None, None));
            crate::core::crate_sort::Item {
                id,
                key: key.as_deref().and_then(crate::core::crate_sort::parse_key),
                bpm,
            }
        })
        .collect())
}

/// The new order of a whole crate for one Sort-menu `mode`.
fn sorted_order(conn: &rusqlite::Connection, order: &[i64], mode: &str) -> Result<Vec<i64>, String> {
    use crate::core::crate_sort::{sort_order, Rng, SortMode};
    let mode = SortMode::parse(mode).ok_or_else(|| format!("unknown sort: {mode}"))?;
    let items = sort_items(conn, order)?;
    Ok(sort_order(&items, mode, &mut Rng::from_clock()))
}

/// Re-numbers a whole folder-crate by the Sort menu's `mode` (key_asc,
/// key_desc, harmonic, harmonic_first, bpm, random).
#[tauri::command]
pub fn sort_crate_tracks(
    library_id: i64,
    folder: String,
    mode: String,
    state: State<AppState>,
) -> Result<(), String> {
    safe_crate_path(&folder)?;
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let order = crate_order::ensure_positions(&conn, library_id, &folder).map_err(err)?;
    let sorted = sorted_order(&conn, &order, &mode)?;
    crate_order::apply_order(&conn, library_id, &folder, &sorted).map_err(err)
}

#[derive(Serialize)]
pub struct AddToCrateResult {
    pub moved: u64,
    pub copied: u64,
    pub skipped: u64,
    pub failed: u64,
}

/// The split view's drop: tracks from anywhere land in one folder-crate before
/// row `before_index`. Tracks of the same library MOVE into the folder (a
/// rename), tracks of other libraries are COPIED (the source stays). The new
/// file names are then written into the crate's order file at the drop spot,
/// so a copied file takes its place as soon as the rescan indexes it. Nothing
/// is ever overwritten or deleted.
#[tauri::command]
pub async fn add_tracks_to_crate(
    track_ids: Vec<i64>,
    library_id: i64,
    folder: String,
    before_index: usize,
    app: AppHandle,
) -> Result<AddToCrateResult, String> {
    safe_crate_path(&folder)?;
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let state = app.state::<AppState>();
        let mut res = AddToCrateResult { moved: 0, copied: 0, skipped: 0, failed: 0 };
        // first pass under the lock: current order, sources, and the moves
        // (renames on one volume, fast); copies run after the lock is released
        let (dest_dir, current, mut names, copies) = {
            let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
            let root = db::library_root(&conn, library_id).map_err(err)?;
            let dest_dir = if folder.is_empty() {
                PathBuf::from(&root)
            } else {
                Path::new(&root).join(&folder)
            };
            if !dest_dir.is_dir() {
                return Err("target crate folder does not exist".into());
            }
            let order = crate_order::ensure_positions(&conn, library_id, &folder).map_err(err)?;
            let files = db::crate_track_files(&conn, library_id, &folder).map_err(err)?;
            let current: Vec<String> = order
                .iter()
                .filter_map(|id| files.iter().find(|f| f.id == *id).map(|f| f.file_name.clone()))
                .collect();
            // one slot per dropped track, so the block keeps the drag's order
            let mut names: Vec<Option<String>> = vec![None; track_ids.len()];
            let mut copies = Vec::new();
            for (i, id) in track_ids.iter().enumerate() {
                let Some((lib, src)) =
                    db::tracks_for_copy(&conn, &[*id]).map_err(err)?.into_iter().next()
                else {
                    res.failed += 1;
                    continue;
                };
                let Some(name) = src.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                    res.failed += 1;
                    continue;
                };
                if lib != library_id {
                    copies.push((i, src, name));
                    continue;
                }
                if src.parent() == Some(dest_dir.as_path()) {
                    // already in this crate: the drop just places it
                    names[i] = Some(name);
                    continue;
                }
                match copying::move_one(&src, &dest_dir) {
                    copying::CopyOutcome::Copied => {
                        let new_rel =
                            if folder.is_empty() { name.clone() } else { format!("{folder}/{name}") };
                        match db::set_track_rel_path(&conn, *id, &new_rel) {
                            Ok(()) => res.moved += 1,
                            // the file moved; the next rescan repairs the row
                            Err(_) => res.failed += 1,
                        }
                        names[i] = Some(name);
                    }
                    copying::CopyOutcome::SkippedExists => res.skipped += 1,
                    copying::CopyOutcome::Failed => res.failed += 1,
                }
            }
            (dest_dir, current, names, copies)
        };
        for (i, src, name) in copies {
            match copying::copy_one(&src, &dest_dir) {
                copying::CopyOutcome::Copied => {
                    res.copied += 1;
                    names[i] = Some(name);
                }
                copying::CopyOutcome::SkippedExists => res.skipped += 1,
                copying::CopyOutcome::Failed => res.failed += 1,
            }
        }
        let names: Vec<String> = names.into_iter().flatten().collect();
        if !names.is_empty() {
            let order = crate_order::insert_names(&current, &names, before_index);
            crate_order::write_order(&dest_dir, &order)
                .map_err(|e| format!("tracks added, but the crate order could not be saved: {e}"))?;
        }
        Ok(res)
    })
    .await
    .map_err(|e| format!("add task failed: {e}"))?
}

/// Reconciles one arriving stick with the index and builds the event payload
/// the frontend needs. Split out of the watcher loop so it can be tested
/// without a Tauri app handle — the payload it returns is what makes an
/// inserted stick appear in the sidebar and rescan its known libraries.
fn reconcile_arrival(
    conn: &rusqlite::Connection,
    dev: &crate::core::devices::Device,
) -> DeviceArrived {
    use crate::core::devices;
    // Identity is the volume serial: a different stick on the same letter
    // stays unknown; a known stick on a new letter is recognized and its
    // library root moves with it.
    let known = match &dev.serial {
        Some(serial) => db::removable_libraries_for_serial(conn, serial).unwrap_or_default(),
        None => Vec::new(),
    };
    let root = format!("{}\\", dev.letter);
    // A stale removable row from a *different* stick may still own this root
    // (root_path is UNIQUE) — park it out of the way.
    let _ = db::park_conflicting_removable(conn, &root, dev.serial.as_deref());
    for lib in &known {
        if let Ok(old_root) = db::library_root(conn, *lib) {
            let new_root = devices::swap_drive_prefix(&old_root, &dev.letter);
            let _ = db::update_library_root(conn, *lib, &new_root);
        }
        let _ = db::clear_library_missing(conn, *lib);
    }
    DeviceArrived {
        letter: dev.letter.clone(),
        label: dev.label.clone(),
        known_library_ids: known,
    }
}

/// 2-second polling loop; runs for the app's lifetime on its own thread
/// with its own DB connection. Emits device:arrived / device:removed and
/// keeps the DevicesState snapshot fresh for list_devices.
pub fn spawn_device_watcher(
    app: AppHandle,
    db_path: PathBuf,
    drives: Arc<Mutex<crate::core::devices::DriveSet>>,
) {
    use crate::core::devices;
    std::thread::spawn(move || {
        let Ok(conn) = db::open(&db_path) else {
            tracing::error!("device watcher: cannot open database");
            return;
        };
        let mut prev = devices::DriveSet::new();
        loop {
            let cur = devices::enumerate_removable();
            let diff = devices::diff_drives(&prev, &cur);
            if let Ok(mut snapshot) = drives.lock() {
                *snapshot = cur.clone();
            }
            for dev in &diff.arrived {
                let _ = app.emit("device:arrived", reconcile_arrival(&conn, dev));
            }
            for dev in &diff.removed {
                let libs = match &dev.serial {
                    Some(serial) => {
                        db::removable_libraries_for_serial(&conn, serial).unwrap_or_default()
                    }
                    None => Vec::new(),
                };
                for lib in &libs {
                    let _ = db::mark_library_tracks_missing(&conn, *lib);
                }
                let _ = app.emit(
                    "device:removed",
                    DeviceRemoved {
                        letter: dev.letter.clone(),
                        library_ids: libs,
                    },
                );
            }
            prev = cur;
            std::thread::sleep(devices::POLL_INTERVAL);
        }
    });
}

#[tauri::command]
pub async fn list_tracks(
    query: String,
    library_id: Option<i64>,
    sort: Option<String>,
    descending: Option<bool>,
    health: Option<String>,
    folder: Option<String>,
    track_ids: Option<Vec<i64>>,
    keys: Option<Vec<String>>,
    limit: i64,
    offset: i64,
    app: AppHandle,
) -> Result<db::TrackPage, String> {
    // Off the main thread: a folder-crate listing also writes (positions, missing
    // flags), and that can wait on SQLite's busy timeout while a scan holds the
    // database, which froze the window when this was a sync command.
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let state = app.state::<AppState>();
        list_tracks_blocking(
            query, library_id, sort, descending, health, folder, track_ids, keys, limit, offset,
            &state,
        )
    })
    .await
    .map_err(|e| format!("listing tracks failed: {e}"))?
}

#[allow(clippy::too_many_arguments)]
fn list_tracks_blocking(
    query: String,
    library_id: Option<i64>,
    sort: Option<String>,
    descending: Option<bool>,
    health: Option<String>,
    folder: Option<String>,
    track_ids: Option<Vec<i64>>,
    keys: Option<Vec<String>>,
    limit: i64,
    offset: i64,
    state: &AppState,
) -> Result<db::TrackPage, String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    // A folder-crate's positions are a cache of its order file: refresh them on
    // every listing so the # column is right even when the file changed on disk
    // (another machine, a restored backup) and whatever the active sort is. A
    // failure here is never fatal — the crate just shows in add order.
    // heal_missing_flags is the same idea for the is_missing flag: a track
    // deleted or restored outside SpinDex dims/undims on the very next view
    // of its crate, instead of waiting for a full library rescan.
    if let (Some(lib), Some(f)) = (library_id, folder.as_deref()) {
        let _ = crate_order::heal_missing_flags(&conn, lib, f);
        let _ = crate_order::ensure_positions(&conn, lib, f);
    }
    db::list_tracks_keyed(
        &conn,
        &query,
        library_id,
        sort.as_deref(),
        descending.unwrap_or(false),
        health.as_deref(),
        folder.as_deref(),
        track_ids.as_deref(),
        keys.as_deref(),
        limit.clamp(1, 1000),
        offset.max(0),
    )
    .map_err(err)
}

#[derive(Serialize)]
pub struct SeratoCrateInfo {
    pub name: String,
    /// file name inside _Serato_/Subcrates, the crate's identity for edits
    pub file: String,
    pub track_ids: Vec<i64>,
    /// crate entries that are not in the index (unindexed or gone)
    pub missing: u64,
}

/// Read-only view of the Serato subcrates on a connected stick. Each crate's
/// stick-relative paths ("MP3/x.mp3") are resolved against the drive's
/// libraries (longest root wins) into track ids. Serato's files are never
/// written. No `_Serato_` folder simply yields an empty list.
#[tauri::command]
pub async fn list_serato_crates(
    letter: String,
    app: AppHandle,
) -> Result<Vec<SeratoCrateInfo>, String> {
    // Reads every .crate on the stick and resolves each entry: off the main
    // thread, so the window keeps painting (it reruns after every Serato edit).
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let state = app.state::<AppState>();
        list_serato_crates_blocking(letter, &state)
    })
    .await
    .map_err(|e| format!("listing Serato crates failed: {e}"))?
}

fn list_serato_crates_blocking(
    letter: String,
    state: &AppState,
) -> Result<Vec<SeratoCrateInfo>, String> {
    let dir = PathBuf::from(format!("{letter}\\_Serato_\\Subcrates"));
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("crate") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let file = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let name = crate::core::serato::crate_display_name(&file);
        let (_, track_ids, missing) = resolve_crate_entries(
            &conn,
            &letter,
            crate::core::serato::parse_crate(&bytes).into_iter().map(Some),
        );
        out.push(SeratoCrateInfo { name, file, track_ids, missing });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[derive(Serialize)]
pub struct RekordboxCrateInfo {
    pub id: u32,
    pub parent_id: u32,
    pub name: String,
    pub is_folder: bool,
    pub track_count: usize,
    /// resolved SpinDex track ids for this playlist, in Rekordbox's own
    /// order — the explicit id-set filter `list_tracks` already supports
    /// for Serato crates (added 2026-07-16). Empty for folder nodes.
    pub track_ids: Vec<i64>,
    /// playlist entries that did not resolve to an indexed SpinDex track
    /// (same meaning as `SeratoCrateInfo::missing`).
    pub missing: usize,
}

/// Read-only view of a connected stick's Rekordbox playlists, parsed from
/// its `export.pdb`. Mirrors `list_serato_crates`: a parse failure or a
/// missing export degrades to an empty list, never an `Err`, so a stick
/// with no Rekordbox export (or one this parser cannot read) simply shows
/// no Rekordbox crates instead of surfacing an error to the user.
#[tauri::command]
pub fn list_rekordbox_crates(
    letter: String,
    state: State<AppState>,
) -> Result<Vec<RekordboxCrateInfo>, String> {
    let pdb_path = PathBuf::from(format!("{letter}\\PIONEER\\rekordbox\\export.pdb"));
    let Ok(bytes) = std::fs::read(&pdb_path) else {
        return Ok(Vec::new()); // no Rekordbox export on this device — not an error
    };

    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let playlists = match crate::core::rekordbox::resolve_playlists(&conn, &letter, &bytes) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("failed to parse Rekordbox export.pdb at {pdb_path:?}: {e}");
            return Ok(Vec::new()); // parse failure degrades to "no playlists", per spec
        }
    };

    Ok(playlists
        .into_iter()
        .map(|p| RekordboxCrateInfo {
            id: p.id,
            parent_id: p.parent_id,
            name: p.name,
            is_folder: p.is_folder,
            track_count: p.track_ids.len(),
            track_ids: p.track_ids,
            missing: p.unresolved,
        })
        .collect())
}

/// The Subcrates file for a crate identity; rejects anything that could
/// escape the folder.
fn serato_crate_path(letter: &str, file: &str) -> Result<PathBuf, String> {
    if file.is_empty()
        || file.contains('/')
        || file.contains('\\')
        || file.contains("..")
        || !file.ends_with(".crate")
    {
        return Err("invalid crate file name".into());
    }
    Ok(PathBuf::from(format!("{letter}\\_Serato_\\Subcrates\\{file}")))
}

/// The write path for every Serato edit: a first-touch backup of the
/// original file into _Serato_/SpinDexBackups (kept forever, never
/// overwritten), then an atomic tmp-write + rename.
fn serato_backup_and_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let subcrates = path.parent().ok_or_else(|| "bad crate path".to_string())?;
    if path.exists() {
        let backups = subcrates
            .parent()
            .unwrap_or(subcrates)
            .join("SpinDexBackups");
        std::fs::create_dir_all(&backups).map_err(|e| e.to_string())?;
        let name = path
            .file_name()
            .ok_or_else(|| "bad crate path".to_string())?;
        let bak = backups.join(name);
        if !bak.exists() {
            std::fs::copy(path, &bak).map_err(|e| format!("backup failed, not writing: {e}"))?;
        }
    }
    let tmp = path.with_extension("crate.spindex-tmp");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Best-effort check for a running Serato DJ process. Advisory only: if the
/// check itself fails, returns false so we fall through to the confirm rather
/// than hard-blocking. Editing a `.crate` while Serato runs is futile — Serato
/// rewrites every crate from memory on exit.
///
/// Reads a Win32 process snapshot instead of spawning `tasklist`: that took
/// ~0.6 s per call, ran before every Serato edit, and every reorder step paid
/// it again.
fn serato_running() -> bool {
    any_serato_process(process_names())
}

/// True when any process image name contains "serato", case-insensitively —
/// the same test the old `tasklist` scan applied to its whole output.
fn any_serato_process<I: IntoIterator<Item = String>>(names: I) -> bool {
    names.into_iter().any(|n| n.to_lowercase().contains("serato"))
}

/// Image names of every running process; empty when the snapshot fails.
#[cfg(windows)]
fn process_names() -> Vec<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    let mut names = Vec::new();
    // SAFETY: a plain snapshot walk; the entry struct is sized as the API
    // requires and the handle is closed on every path out.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return names;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut entry);
        while ok != 0 {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            names.push(String::from_utf16_lossy(&entry.szExeFile[..len]));
            ok = Process32NextW(snap, &mut entry);
        }
        CloseHandle(snap);
    }
    names
}

#[cfg(not(windows))]
fn process_names() -> Vec<String> {
    Vec::new()
}

/// The intent sidecar path for a stick.
fn intent_path(letter: &str) -> PathBuf {
    PathBuf::from(format!("{letter}\\_Serato_\\SpinDexBackups\\spindex-intent.json"))
}

/// Records a crate's full intended path set (best-effort — a sidecar write
/// failure never fails the edit that already succeeded on disk).
fn record_crate_intent(letter: &str, file: &str, paths: Vec<String>) {
    let path = intent_path(letter);
    let mut map = serato_intent::read_intent(&path);
    map.insert(file.to_string(), paths);
    let _ = serato_intent::write_intent(&path, &map);
}



/// (stick-relative path with forward slashes, source abs path, library id)
/// for one track, when it lives on the given stick; None = foreign track.
fn stick_rel(conn: &rusqlite::Connection, prefix: &str, id: i64) -> Option<(String, i64)> {
    let (root, rel): (String, String) = conn
        .query_row(
            "SELECT l.root_path, t.rel_path FROM track t
             JOIN library l ON l.id = t.library_id WHERE t.id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten()?;
    let lib_id: i64 = conn
        .query_row("SELECT library_id FROM track WHERE id = ?1", params![id], |r| r.get(0))
        .ok()?;
    let folder = root.strip_prefix(prefix)?.replace('\\', "/");
    Some((format!("{folder}{rel}"), lib_id))
}

#[derive(Serialize)]
pub struct SeratoEditResult {
    pub added: u64,
    pub removed: u64,
    pub copied: u64,
    pub skipped: u64,
    pub failed: u64,
    /// library that received copied files and needs a rescan
    pub rescan_library: Option<i64>,
}

/// Adds tracks to a Serato crate. Tracks already on the stick are linked
/// directly; foreign tracks are first copied into a stick library folder
/// (the folder most of the crate already lives in), then linked. Existing
/// files are never overwritten, the crate file is backed up first-touch.
#[tauri::command]
pub fn serato_add_tracks(
    letter: String,
    file: String,
    track_ids: Vec<i64>,
    before_index: Option<usize>,
    state: State<AppState>,
) -> Result<SeratoEditResult, String> {
    // Checked before the lock, like every other Serato write: Serato rewrites
    // its crates from memory on exit, so links added while it runs are lost.
    if serato_running() {
        return Err("Close Serato first — it rewrites its crates on exit.".into());
    }
    let path = serato_crate_path(&letter, &file)?;
    let bytes = std::fs::read(&path).map_err(|e| format!("reading crate: {e}"))?;
    let mut chunks = crate::core::serato::read_chunks(&bytes);
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let prefix = format!("{letter}\\");
    // `before_index` counts the rows the crate view shows; the new links go in
    // before the chunk of the track shown at that row (None or past the end:
    // appended)
    let insert_at = before_index.and_then(|b| {
        let (resolved, order, _) = resolve_crate_entries(
            &conn,
            &letter,
            chunks.iter().map(crate::core::serato::otrk_path),
        );
        order.get(b).and_then(|id| resolved.iter().position(|r| *r == Some(*id)))
    });

    // stick libraries, keyed by their folder segment ("MP3/" -> "MP3")
    let stick_libs: Vec<(i64, String, String)> = db::list_libraries(&conn)
        .map_err(err)?
        .into_iter()
        .filter(|l| l.root_path.starts_with(&prefix))
        .map(|l| {
            let seg = l.root_path[prefix.len()..].trim_end_matches('\\').to_string();
            (l.id, seg, l.root_path)
        })
        .collect();
    // copy target: the folder most of the crate's entries already use,
    // falling back to the stick's first library
    let copy_target = {
        let mut counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for c in &chunks {
            if let Some(p) = crate::core::serato::otrk_path(c) {
                let seg = p.split('/').next().unwrap_or("").to_string();
                *counts.entry(seg).or_insert(0) += 1;
            }
        }
        stick_libs
            .iter()
            .max_by_key(|(_, seg, _)| counts.get(seg).copied().unwrap_or(0))
            .cloned()
    };

    let mut res = SeratoEditResult {
        added: 0,
        removed: 0,
        copied: 0,
        skipped: 0,
        failed: 0,
        rescan_library: None,
    };
    let mut to_add: Vec<String> = Vec::new();
    for id in track_ids {
        if let Some((stick_path, _)) = stick_rel(&conn, &prefix, id) {
            to_add.push(stick_path);
            continue;
        }
        // foreign track: copy to the stick first, then link
        let Some((lib_id, seg, root)) = copy_target.clone() else {
            res.failed += 1;
            continue;
        };
        let Some((_, src)) = db::tracks_for_copy(&conn, &[id]).map_err(err)?.into_iter().next()
        else {
            res.failed += 1;
            continue;
        };
        let Some(fname) = src.file_name().and_then(|f| f.to_str()).map(str::to_string) else {
            res.failed += 1;
            continue;
        };
        match copying::copy_one(&src, Path::new(&root)) {
            copying::CopyOutcome::Copied => {
                res.copied += 1;
                res.rescan_library = Some(lib_id);
            }
            copying::CopyOutcome::SkippedExists => res.skipped += 1,
            copying::CopyOutcome::Failed => {
                res.failed += 1;
                continue;
            }
        }
        to_add.push(if seg.is_empty() { fname } else { format!("{seg}/{fname}") });
    }
    res.added = crate::core::serato::add_tracks_at(&mut chunks, &to_add, insert_at) as u64;
    serato_backup_and_write(&path, &crate::core::serato::write_chunks(&chunks))?;
    let paths: Vec<String> = chunks.iter().filter_map(crate::core::serato::otrk_path).collect();
    record_crate_intent(&letter, &file, paths);
    Ok(res)
}

/// Removes tracks from a Serato crate (link entries only — the audio files
/// stay exactly where they are).
#[tauri::command]
pub fn serato_remove_tracks(
    letter: String,
    file: String,
    track_ids: Vec<i64>,
    state: State<AppState>,
) -> Result<u64, String> {
    // Checked before the lock, like every other Serato write: Serato rewrites
    // its crates from memory on exit, so a removal made while it runs is
    // silently undone.
    if serato_running() {
        return Err("Close Serato first — it rewrites its crates on exit.".into());
    }
    let path = serato_crate_path(&letter, &file)?;
    let bytes = std::fs::read(&path).map_err(|e| format!("reading crate: {e}"))?;
    let mut chunks = crate::core::serato::read_chunks(&bytes);
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let prefix = format!("{letter}\\");
    let paths: Vec<String> = track_ids
        .iter()
        .filter_map(|&id| stick_rel(&conn, &prefix, id).map(|(p, _)| p))
        .collect();
    let removed = crate::core::serato::remove_tracks(&mut chunks, &paths) as u64;
    if removed > 0 {
        serato_backup_and_write(&path, &crate::core::serato::write_chunks(&chunks))?;
        let paths: Vec<String> =
            chunks.iter().filter_map(crate::core::serato::otrk_path).collect();
        record_crate_intent(&letter, &file, paths);
    }
    Ok(removed)
}

/// Everything the writer needs to permute one crate: its path, the source
/// file's byte length (the permutation guard compares against it), the chunk
/// list, the per-chunk resolution, and the resulting track-id sequence — the
/// same sequence, from the same helper, that the crate viewer shows.
struct SeratoCrateState {
    path: std::path::PathBuf,
    src_len: usize,
    chunks: Vec<crate::core::serato::Chunk>,
    resolved: Vec<Option<i64>>,
    order: Vec<i64>,
}

fn serato_crate_state(
    conn: &rusqlite::Connection,
    letter: &str,
    file: &str,
) -> Result<SeratoCrateState, String> {
    let path = serato_crate_path(letter, file)?;
    let bytes = std::fs::read(&path).map_err(|e| format!("reading crate: {e}"))?;
    let chunks = crate::core::serato::read_chunks(&bytes);
    let (resolved, order, _) = resolve_crate_entries(
        conn,
        letter,
        chunks.iter().map(crate::core::serato::otrk_path),
    );
    Ok(SeratoCrateState { path, src_len: bytes.len(), chunks, resolved, order })
}

/// Writes a permuted crate: backup + atomic write, then the intent record.
/// Both callers refuse while Serato is running (it rewrites every crate from
/// memory on exit) and do so before taking the SQLite mutex, so this path does
/// not spawn `tasklist` a second time.
///
/// Guarded by `permutation_is_intact` BEFORE anything touches the disk: a
/// reorder may only shuffle chunks, so an output that lost a chunk, changed
/// size, or came from a crate the parser could not read to the end is a bug or
/// a damaged file, and writing it would delete links permanently.
fn serato_write_order(
    st: &SeratoCrateState,
    letter: &str,
    file: &str,
    out: &[crate::core::serato::Chunk],
) -> Result<(), String> {
    if !crate::core::serato::permutation_is_intact(&st.chunks, out, st.src_len) {
        return Err(
            "This crate file could not be read completely (truncated or damaged), \
             so reordering it would drop entries. Nothing was changed — repair or \
             rebuild the crate in Serato first."
                .into(),
        );
    }
    serato_backup_and_write(&st.path, &crate::core::serato::write_chunks(out))?;
    let paths: Vec<String> =
        out.iter().filter_map(crate::core::serato::otrk_path).collect();
    // A permutation adds and drops nothing, so it has no new membership to
    // record — and rewriting intent here would replace a pending Serato-clobber
    // recovery record before the user ever gets to run Sync. Sync diffs SETS,
    // so an unchanged set means there is genuinely nothing to write.
    let recorded = serato_intent::read_intent(&intent_path(letter))
        .get(file)
        .cloned()
        .unwrap_or_default();
    let d = serato_intent::diff(&recorded, &paths);
    if !d.spindex_only.is_empty() || !d.serato_only.is_empty() {
        record_crate_intent(letter, file, paths);
    }
    Ok(())
}

/// Drops the selected tracks before `before_index` in the crate's own order.
#[tauri::command]
pub async fn serato_reorder_tracks(
    letter: String,
    file: String,
    track_ids: Vec<i64>,
    before_index: usize,
    app: AppHandle,
) -> Result<(), String> {
    // The Serato check, the crate read and the USB write all run off the main
    // thread: a sync command freezes the whole window until it returns.
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        // checked before taking the SQLite mutex: the process snapshot must
        // not run with the whole database locked
        if serato_running() {
            return Err("Close Serato first — it rewrites its crates on exit.".into());
        }
        let state = app.state::<AppState>();
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        let st = serato_crate_state(&conn, &letter, &file)?;
        let moved = crate::core::crate_order::insert_block(&st.order, &track_ids, before_index);
        let out = crate::core::serato::reorder_otrk(&st.chunks, &st.resolved, &moved);
        // nothing moved (e.g. ▼ on the last track, which move_block clamps):
        // no backup, no write, no intent churn — nor a failure on a
        // write-protected stick
        if out == st.chunks {
            return Ok(());
        }
        serato_write_order(&st, &letter, &file, &out)
    })
    .await
    .map_err(|e| format!("reorder task failed: {e}"))?
}

/// Re-numbers a whole Serato crate by the Sort menu's `mode`.
#[tauri::command]
pub async fn serato_sort_tracks(
    letter: String,
    file: String,
    mode: String,
    app: AppHandle,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        if serato_running() {
            return Err("Close Serato first — it rewrites its crates on exit.".into());
        }
        let state = app.state::<AppState>();
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        let st = serato_crate_state(&conn, &letter, &file)?;
        let sorted = sorted_order(&conn, &st.order, &mode)?;
        let out = crate::core::serato::reorder_otrk(&st.chunks, &st.resolved, &sorted);
        if out == st.chunks {
            return Ok(());
        }
        serato_write_order(&st, &letter, &file, &out)
    })
    .await
    .map_err(|e| format!("sort task failed: {e}"))?
}

/// Moves the selected tracks one slot up (delta < 0) or down (delta > 0).
#[tauri::command]
pub async fn serato_move_tracks(
    letter: String,
    file: String,
    track_ids: Vec<i64>,
    delta: i32,
    app: AppHandle,
) -> Result<(), String> {
    // The Serato check, the crate read and the USB write all run off the main
    // thread: a sync command freezes the whole window until it returns.
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        // checked before taking the SQLite mutex: the process snapshot must
        // not run with the whole database locked
        if serato_running() {
            return Err("Close Serato first — it rewrites its crates on exit.".into());
        }
        let state = app.state::<AppState>();
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        let st = serato_crate_state(&conn, &letter, &file)?;
        let moved = crate::core::crate_order::move_block(&st.order, &track_ids, delta);
        let out = crate::core::serato::reorder_otrk(&st.chunks, &st.resolved, &moved);
        // nothing moved (e.g. ▼ on the last track, which move_block clamps):
        // no backup, no write, no intent churn — nor a failure on a
        // write-protected stick
        if out == st.chunks {
            return Ok(());
        }
        serato_write_order(&st, &letter, &file, &out)
    })
    .await
    .map_err(|e| format!("reorder task failed: {e}"))?
}

/// Creates a new, empty Serato crate file.
#[tauri::command]
pub fn serato_create_crate(letter: String, name: String) -> Result<(), String> {
    let name = copying::validate_folder_name(&name).map_err(err)?;
    if name.contains('%') {
        return Err("crate name may not contain '%'".into());
    }
    let path = serato_crate_path(&letter, &format!("{name}.crate"))?;
    if path.exists() {
        return Err(format!("Serato crate \"{name}\" already exists"));
    }
    let Some(dir) = path.parent() else {
        return Err("bad crate path".into());
    };
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    serato_backup_and_write(&path, &crate::core::serato::empty_crate())?;
    record_crate_intent(&letter, &format!("{name}.crate"), Vec::new());
    Ok(())
}

/// Renames a Serato crate file (a nested "Parent%%Child" keeps its parent).
#[tauri::command]
pub fn serato_rename_crate(
    letter: String,
    file: String,
    new_name: String,
) -> Result<String, String> {
    let new_name = copying::validate_folder_name(&new_name).map_err(err)?;
    if new_name.contains('%') {
        return Err("crate name may not contain '%'".into());
    }
    let old_path = serato_crate_path(&letter, &file)?;
    let stem = file.strip_suffix(".crate").unwrap_or(&file);
    let new_file = match stem.rsplit_once("%%") {
        Some((parent, _)) => format!("{parent}%%{new_name}.crate"),
        None => format!("{new_name}.crate"),
    };
    let new_path = serato_crate_path(&letter, &new_file)?;
    if new_file.to_lowercase() != file.to_lowercase() && new_path.exists() {
        return Err(format!("Serato crate \"{new_name}\" already exists"));
    }
    std::fs::rename(&old_path, &new_path).map_err(|e| e.to_string())?;
    let ip = intent_path(&letter);
    let mut map = serato_intent::read_intent(&ip);
    if let Some(v) = map.remove(&file) {
        map.insert(new_file.clone(), v);
        let _ = serato_intent::write_intent(&ip, &map);
    }
    Ok(new_file)
}

/// Deletes a Serato crate file. Only the crate link file goes (backed up
/// first) — no audio file is touched.
#[tauri::command]
pub fn serato_delete_crate(letter: String, file: String) -> Result<(), String> {
    let path = serato_crate_path(&letter, &file)?;
    let bytes = std::fs::read(&path).map_err(|e| format!("reading crate: {e}"))?;
    // routes through the backup logic, then removes the live file
    serato_backup_and_write(&path, &bytes)?;
    std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    let ip = intent_path(&letter);
    let mut map = serato_intent::read_intent(&ip);
    if map.remove(&file).is_some() {
        let _ = serato_intent::write_intent(&ip, &map);
    }
    Ok(())
}

/// Repoints `old_rel` -> `new_rel` (LIBRARY-relative, forward-slash — the
/// same shape `track.rel_path` uses) across every `.crate` on the device
/// holding `library_id`, applying the same first-touch backup + atomic write
/// + intent-recording sequence as `serato_remove_tracks`. No-op (`Ok(0)`)
/// when the library isn't on a device with a `_Serato_/Subcrates` folder, or
/// when the library root isn't drive-letter-rooted. Called by the enhance
/// worker (Task 5) after a cross-extension replace so Serato's own crates
/// keep pointing at the (renamed) track.
///
/// `.crate` `ptrk` entries are STICK-relative (drive-root-relative,
/// including the library's own folder segment — e.g. "Library MP3/track.mp3"),
/// not library-relative, so both paths are converted via
/// `serato::stick_relative_path` before the exact-match compare in
/// `replace_track_path` — comparing library-relative paths against
/// stick-relative crate entries silently never matched (Ok(0) on every
/// device-folder library, the common case).
pub(crate) fn repoint_serato_links(
    conn: &rusqlite::Connection,
    library_id: i64,
    old_rel: &str,
    new_rel: &str,
) -> anyhow::Result<usize> {
    let root: String = conn
        .query_row(
            "SELECT root_path FROM library WHERE id = ?1",
            params![library_id],
            |r| r.get(0),
        )
        .map_err(|e| anyhow::anyhow!("resolving library {library_id}: {e}"))?;
    let Some(letter) = root.get(0..2).filter(|s| s.as_bytes()[1] == b':') else {
        return Ok(0);
    };
    let letter = letter.to_string();
    let Some(old_stick) = crate::core::serato::stick_relative_path(&root, old_rel) else {
        return Ok(0);
    };
    let Some(new_stick) = crate::core::serato::stick_relative_path(&root, new_rel) else {
        return Ok(0);
    };
    let dir = PathBuf::from(format!("{letter}\\_Serato_\\Subcrates"));
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(0);
    };
    let mut total = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("crate") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let mut chunks = crate::core::serato::read_chunks(&bytes);
        let n = crate::core::serato::replace_track_path(&mut chunks, &old_stick, &new_stick);
        if n == 0 {
            continue;
        }
        serato_backup_and_write(&path, &crate::core::serato::write_chunks(&chunks))
            .map_err(|e| anyhow::anyhow!(e))?;
        let file = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let paths: Vec<String> =
            chunks.iter().filter_map(crate::core::serato::otrk_path).collect();
        record_crate_intent(&letter, &file, paths);
        total += n;
    }
    Ok(total)
}

#[derive(Serialize)]
pub struct SyncEntry {
    pub path: String,
    pub title: Option<String>,
    pub artist: Option<String>,
}

#[derive(Serialize)]
pub struct SeratoSyncPlan {
    /// intended, missing from Serato — offer to add (recovers clobbered edits)
    pub spindex_only: Vec<SyncEntry>,
    /// in Serato, not intended — offer keep (adopt) or remove link
    pub serato_only: Vec<SyncEntry>,
}

/// Resolves a stick-relative path to (title, artist) for display, using the
/// same longest-root-first library match as `list_serato_crates`. Unresolved
/// paths (file not indexed) return (None, None) and show by raw path.
fn resolve_display(
    conn: &rusqlite::Connection,
    libs: &[(i64, String)],
    prefix: &str,
    rel: &str,
) -> (Option<String>, Option<String>) {
    let full = format!("{prefix}{}", rel.replace('/', "\\"));
    for (id, root) in libs {
        if let Some(tail) = full.strip_prefix(root.as_str()) {
            if let Ok(Some(tid)) = db::track_id_by_rel(conn, *id, &tail.replace('\\', "/")) {
                if let Ok(row) = conn.query_row(
                    "SELECT title, artist FROM track WHERE id = ?1",
                    params![tid],
                    |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)),
                ) {
                    return row;
                }
            }
        }
    }
    (None, None)
}

/// Compares SpinDex's intent for a crate with the live `.crate` and returns the
/// two-way difference. Refuses if Serato is running (it would clobber a sync).
#[tauri::command]
pub fn serato_sync_preview(
    letter: String,
    file: String,
    state: State<AppState>,
) -> Result<SeratoSyncPlan, String> {
    if serato_running() {
        return Err("Close Serato DJ first — it overwrites crate edits when it exits.".into());
    }
    let path = serato_crate_path(&letter, &file)?;
    let bytes = std::fs::read(&path).map_err(|e| format!("reading crate: {e}"))?;
    let crate_paths = crate::core::serato::parse_crate(&bytes);
    let intent = serato_intent::read_intent(&intent_path(&letter))
        .get(&file)
        .cloned()
        .unwrap_or_default();
    let d = serato_intent::diff(&intent, &crate_paths);

    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let prefix = format!("{letter}\\");
    let mut libs: Vec<(i64, String)> = db::list_libraries(&conn)
        .map_err(err)?
        .into_iter()
        .filter(|l| l.root_path.starts_with(&prefix))
        .map(|l| (l.id, l.root_path))
        .collect();
    libs.sort_by_key(|(_, root)| std::cmp::Reverse(root.len()));

    let entry = |rel: String| -> SyncEntry {
        let (title, artist) = resolve_display(&conn, &libs, &prefix, &rel);
        SyncEntry { path: rel, title, artist }
    };
    Ok(SeratoSyncPlan {
        spindex_only: d.spindex_only.into_iter().map(entry).collect(),
        serato_only: d.serato_only.into_iter().map(entry).collect(),
    })
}

/// Applies the user's reconcile choices: add the chosen intended paths to the
/// `.crate`, remove the chosen link entries (links only — audio untouched),
/// then rewrite intent to the final `.crate` contents so both agree.
#[tauri::command]
pub fn serato_sync_apply(
    letter: String,
    file: String,
    add: Vec<String>,
    remove: Vec<String>,
) -> Result<SeratoEditResult, String> {
    if serato_running() {
        return Err("Close Serato DJ first — it overwrites crate edits when it exits.".into());
    }
    let path = serato_crate_path(&letter, &file)?;
    let bytes = std::fs::read(&path).map_err(|e| format!("reading crate: {e}"))?;
    let mut chunks = crate::core::serato::read_chunks(&bytes);

    let added = crate::core::serato::add_tracks(&mut chunks, &add) as u64;
    let removed = crate::core::serato::remove_tracks(&mut chunks, &remove) as u64;
    serato_backup_and_write(&path, &crate::core::serato::write_chunks(&chunks))?;

    let paths: Vec<String> = chunks.iter().filter_map(crate::core::serato::otrk_path).collect();
    record_crate_intent(&letter, &file, paths);

    Ok(SeratoEditResult {
        added,
        removed,
        copied: 0,
        skipped: 0,
        failed: 0,
        rescan_library: None,
    })
}

#[tauri::command]
pub fn list_crates(
    library_id: i64,
    state: State<AppState>,
) -> Result<Vec<db::CrateFolder>, String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    db::crate_folders(&conn, library_id).map_err(err)
}

#[tauri::command]
pub fn find_duplicates(
    library_id: i64,
    state: State<AppState>,
) -> Result<crate::core::dupes::DupeReport, String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    crate::core::dupes::find_duplicates(&conn, library_id).map_err(err)
}

#[tauri::command]
pub fn library_health(
    library_id: Option<i64>,
    state: State<AppState>,
) -> Result<db::HealthCounts, String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    db::library_health(&conn, library_id).map_err(err)
}

/// Distinct genres across the whole index, for the metadata editor's
/// genre pick-list. Read-only.
#[tauri::command]
pub fn list_genres(state: State<AppState>) -> Result<Vec<db::GenreCount>, String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    db::list_genres(&conn).map_err(err)
}

/// Creates parent/name as a fresh (or empty existing) folder and registers
/// it as a library. Non-empty existing folders are rejected — those go
/// through the normal "Add existing folder" path.
#[tauri::command]
pub fn create_library(
    parent: String,
    name: String,
    state: State<AppState>,
) -> Result<db::LibraryInfo, String> {
    let name = copying::validate_folder_name(&name).map_err(err)?;
    let parent_path = PathBuf::from(&parent);
    if !parent_path.is_dir() {
        return Err(format!("not a folder: {parent}"));
    }
    let target = parent_path.join(&name);
    if target.exists() {
        let occupied = std::fs::read_dir(&target)
            .map_err(|e| format!("reading {}: {e}", target.display()))?
            .next()
            .is_some();
        if occupied {
            return Err(
                "folder already exists and is not empty — use Add existing folder".into(),
            );
        }
    } else {
        std::fs::create_dir_all(&target)
            .map_err(|e| format!("creating {}: {e}", target.display()))?;
    }
    let canonical = target
        .canonicalize()
        .map_err(|e| format!("resolving {}: {e}", target.display()))?;
    let root = scanner::clean_path_string(&canonical);

    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let id = db::ensure_library(&conn, &name, &root).map_err(err)?;
    db::list_libraries(&conn)
        .map_err(err)?
        .into_iter()
        .find(|l| l.id == id)
        .ok_or_else(|| "library not found after insert".to_string())
}

#[derive(Default)]
pub struct CopyState {
    pub running: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Serialize, Clone)]
pub struct CopyProgress {
    pub done: u64,
    pub skipped: u64,
    pub failed: u64,
    pub total: u64,
}

#[tauri::command]
pub fn cancel_copy(copy_state: State<CopyState>) {
    copy_state.cancel.store(true, Ordering::SeqCst);
}

#[derive(serde::Serialize)]
pub struct DeleteReport {
    pub deleted: usize,
    pub failed: Vec<String>,
}

/// Delete-target triage for a selection, split by what's actually on disk.
struct DeleteTargets {
    /// File still present: goes through the Recycle Bin as normal.
    existing: Vec<(i64, std::path::PathBuf)>,
    /// File already gone AND the library root is reachable: nothing for the
    /// shell to do, the DB row can be dropped directly. This is the case that
    /// used to make Delete silently do nothing — a track whose file was
    /// deleted outside SpinDex (by hand, or as an enhance/stabilize leftover)
    /// never matched `p.is_file()`, so the old code filtered it out before
    /// ever looking at the row.
    confirmed_gone: Vec<i64>,
    /// File absent AND the library root itself isn't reachable right now
    /// (e.g. an unplugged USB stick) — left alone entirely. Purging these
    /// would break reconnect/rescan bookkeeping for a device that's merely
    /// disconnected, not gone.
    unreachable: Vec<(i64, String)>,
}

fn classify_delete_targets(conn: &rusqlite::Connection, ids: &[i64]) -> DeleteTargets {
    let mut existing = Vec::new();
    let mut confirmed_gone = Vec::new();
    let mut unreachable = Vec::new();
    for &id in ids {
        let Ok((root, abs)) = db::track_root_and_abs_path(conn, id) else {
            continue;
        };
        if abs.is_file() {
            existing.push((id, abs));
        } else if root.is_dir() {
            confirmed_gone.push(id);
        } else {
            let name = abs
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| abs.display().to_string());
            unreachable.push((id, name));
        }
    }
    DeleteTargets { existing, confirmed_gone, unreachable }
}

/// Sends existing files to the Recycle Bin (permanent on USB sticks), and
/// drops the DB row for any track whose file is gone — whether the shell just
/// removed it, or it was already gone (see `DeleteTargets`). The filesystem is
/// the ground truth: rows survive for any file the shell failed to remove, and
/// for any track whose library isn't currently reachable.
#[tauri::command]
pub fn delete_tracks(track_ids: Vec<i64>, state: State<AppState>) -> Result<DeleteReport, String> {
    let targets = {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        classify_delete_targets(&conn, &track_ids)
    };
    let mut gone_ids = targets.confirmed_gone;
    let mut failed: Vec<String> =
        targets.unreachable.into_iter().map(|(_, name)| name).collect();

    if !targets.existing.is_empty() {
        let refs: Vec<&std::path::Path> =
            targets.existing.iter().map(|(_, p)| p.as_path()).collect();
        let shell = trash::recycle(&refs);

        let before = gone_ids.len();
        for (id, p) in &targets.existing {
            if p.exists() {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string());
                failed.push(name.unwrap_or_else(|| p.display().to_string()));
            } else {
                gone_ids.push(*id);
            }
        }
        // Nothing got deleted anywhere AND the shell errored: surface it.
        if gone_ids.len() == before {
            if let Err(e) = shell {
                if gone_ids.is_empty() {
                    return Err(err(e));
                }
            }
        }
    }

    if gone_ids.is_empty() && failed.is_empty() {
        return Ok(DeleteReport { deleted: 0, failed: Vec::new() });
    }
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    delete_rows_and_prune_order(&conn, &gone_ids).map_err(err)?;
    Ok(DeleteReport { deleted: gone_ids.len(), failed })
}

/// Removes the given tracks' rows, then prunes their names out of whatever
/// folder-crate order files listed them — otherwise a deleted track's name
/// lingers in `.spindex-order.json` forever (`reconcile_order` only drops it
/// from the resolved sequence in memory, it never rewrites the file itself).
/// Captures (library_id, rel_path) BEFORE deleting: a row that's already gone
/// has nothing left to look its rel_path up from.
fn delete_rows_and_prune_order(conn: &rusqlite::Connection, ids: &[i64]) -> anyhow::Result<()> {
    let entries: Vec<(i64, String)> = ids
        .iter()
        .filter_map(|id| {
            conn.query_row(
                "SELECT library_id, rel_path FROM track WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
        })
        .collect();
    db::delete_track_rows(conn, ids)?;
    crate_order::prune_deleted_tracks(conn, &entries);
    Ok(())
}

/// Explorer's `/select` argument must carry its own quotes around the path
/// (`/select,"C:\a b\c.mp3"`). Passing it through `Command::arg` instead
/// quotes the WHOLE argument when the path contains spaces
/// (`"/select,C:\a b\c.mp3"`), which Explorer cannot parse — it silently
/// falls back to opening Documents. So the callers below hand this string to
/// `raw_arg`, bypassing Rust's quoting.
pub fn explorer_select_arg(path: &str) -> String {
    format!("/select,\"{path}\"")
}

fn spawn_explorer_select(path: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("explorer")
        .raw_arg(explorer_select_arg(path))
        .spawn()
        .map_err(|e| format!("opening Explorer: {e}"))?;
    Ok(())
}

/// Opens Windows Explorer with the track's file selected.
#[tauri::command]
pub fn show_in_explorer(track_id: i64, state: State<AppState>) -> Result<(), String> {
    let abs = {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        db::track_abs_path(&conn, track_id).map_err(err)?
    };
    if !abs.is_file() {
        return Err(format!("file not found: {}", abs.display()));
    }
    spawn_explorer_select(&abs.display().to_string())
}

/// Reveals an absolute path in Explorer — used for files that have no track
/// row yet (e.g. a just-created "(Enhanced)" file before its rescan lands).
#[tauri::command]
pub fn reveal_path(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.is_file() {
        return Err(format!("file not found: {path}"));
    }
    spawn_explorer_select(&path)
}

/// Copies the given tracks flat into the target library's folder on a
/// background thread. Existing destinations and tracks already in the
/// target are skipped; nothing is ever overwritten. Progress arrives via
/// copy:* events; the frontend rescans the target on copy:done.
#[tauri::command]
pub fn copy_tracks(
    track_ids: Vec<i64>,
    target_library_id: i64,
    dest_folder: Option<String>,
    app: AppHandle,
    state: State<AppState>,
    copy_state: State<CopyState>,
) -> Result<u64, String> {
    if let Some(ref f) = dest_folder {
        safe_crate_path(f)?;
    }
    if copy_state.running.swap(true, Ordering::SeqCst) {
        return Err("a copy is already running".into());
    }
    copy_state.cancel.store(false, Ordering::SeqCst);

    let (target_root, items) = {
        let conn = match state.conn.lock() {
            Ok(c) => c,
            Err(_) => {
                copy_state.running.store(false, Ordering::SeqCst);
                return Err(LOCK_ERR.to_string());
            }
        };
        let root = db::library_root(&conn, target_library_id)
            .map_err(err)
            .inspect_err(|_| copy_state.running.store(false, Ordering::SeqCst))?;
        let items = db::tracks_for_copy(&conn, &track_ids)
            .map_err(err)
            .inspect_err(|_| copy_state.running.store(false, Ordering::SeqCst))?;
        // Copies may land inside a crate folder of the target library.
        let root = match dest_folder {
            Some(ref f) if !f.is_empty() => {
                let dir = Path::new(&root).join(f);
                if !dir.is_dir() {
                    copy_state.running.store(false, Ordering::SeqCst);
                    return Err("target crate folder does not exist".into());
                }
                dir
            }
            _ => PathBuf::from(root),
        };
        (root, items)
    };

    let total = items.len() as u64;
    if total == 0 {
        copy_state.running.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "copy:done",
            CopyProgress { done: 0, skipped: 0, failed: 0, total: 0 },
        );
        return Ok(0);
    }

    let running = copy_state.running.clone();
    let cancel = copy_state.cancel.clone();
    std::thread::spawn(move || {
        let (mut done, mut skipped, mut failed) = (0u64, 0u64, 0u64);
        for (lib_id, source) in &items {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            if *lib_id == target_library_id {
                skipped += 1;
            } else {
                match copying::copy_one(source, &target_root) {
                    copying::CopyOutcome::Copied => done += 1,
                    copying::CopyOutcome::SkippedExists => skipped += 1,
                    copying::CopyOutcome::Failed => failed += 1,
                }
            }
            let _ = app.emit(
                "copy:progress",
                CopyProgress { done, skipped, failed, total },
            );
        }
        running.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "copy:done",
            CopyProgress { done, skipped, failed, total },
        );
    });
    Ok(total)
}

/// Copies a whole crate folder (incl. nested subcrates) into another
/// library: `<target_root>/<crate name>/<subtree>`. Existing files are
/// skipped — a same-named target folder is a merge, never an overwrite.
/// Reuses the copy worker + copy:* events, so the Copying pill and cancel
/// apply unchanged; the frontend rescans the target on copy:done.
#[tauri::command]
pub fn copy_crate(
    source_library_id: i64,
    folder: String,
    target_library_id: i64,
    app: AppHandle,
    state: State<AppState>,
    copy_state: State<CopyState>,
) -> Result<u64, String> {
    safe_crate_path(&folder)?;
    if folder.is_empty() {
        return Err("cannot copy the library root as a crate".into());
    }
    if source_library_id == target_library_id {
        return Err("source and target library are the same".into());
    }
    if copy_state.running.swap(true, Ordering::SeqCst) {
        return Err("a copy is already running".into());
    }
    copy_state.cancel.store(false, Ordering::SeqCst);

    let (target_root, items) = {
        let conn = match state.conn.lock() {
            Ok(c) => c,
            Err(_) => {
                copy_state.running.store(false, Ordering::SeqCst);
                return Err(LOCK_ERR.to_string());
            }
        };
        let root = db::library_root(&conn, target_library_id)
            .map_err(err)
            .inspect_err(|_| copy_state.running.store(false, Ordering::SeqCst))?;
        let items = db::tracks_in_subtree(&conn, source_library_id, &folder)
            .map_err(err)
            .inspect_err(|_| copy_state.running.store(false, Ordering::SeqCst))?;
        (PathBuf::from(root), items)
    };

    // resolve destinations up front; anything unmappable is skipped
    let jobs: Vec<(PathBuf, PathBuf)> = items
        .into_iter()
        .filter_map(|(src, rel)| {
            copying::crate_dest_rel(&folder, &rel).map(|d| (src, target_root.join(d)))
        })
        .collect();

    let total = jobs.len() as u64;
    if total == 0 {
        copy_state.running.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "copy:done",
            CopyProgress { done: 0, skipped: 0, failed: 0, total: 0 },
        );
        return Ok(0);
    }

    let running = copy_state.running.clone();
    let cancel = copy_state.cancel.clone();
    std::thread::spawn(move || {
        let (mut done, mut skipped, mut failed) = (0u64, 0u64, 0u64);
        for (src, dest) in &jobs {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            match copying::copy_to(src, dest) {
                copying::CopyOutcome::Copied => done += 1,
                copying::CopyOutcome::SkippedExists => skipped += 1,
                copying::CopyOutcome::Failed => failed += 1,
            }
            let _ = app.emit(
                "copy:progress",
                CopyProgress { done, skipped, failed, total },
            );
        }
        // Each copied folder takes its hand-made order along, so the copy
        // does not land in add order. Best-effort: the files are there either
        // way, and a missing order only means add order.
        let folders: std::collections::BTreeSet<(PathBuf, PathBuf)> = jobs
            .iter()
            .filter_map(|(src, dest)| Some((src.parent()?.to_path_buf(), dest.parent()?.to_path_buf())))
            .collect();
        for (src_dir, dest_dir) in &folders {
            if let Err(e) = crate_order::carry_order(src_dir, dest_dir) {
                tracing::warn!("carrying crate order to {}: {e}", dest_dir.display());
            }
        }
        running.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "copy:done",
            CopyProgress { done, skipped, failed, total },
        );
    });
    Ok(total)
}

#[derive(Default)]
pub struct BatchState {
    pub running: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Serialize, Clone)]
pub struct BatchProgress {
    pub done: u64,
    pub failed: u64,
    pub total: u64,
}

#[derive(Serialize, Clone)]
pub struct BatchDone {
    pub done: u64,
    pub failed: u64,
    pub total: u64,
    pub failed_names: Vec<String>,
    /// Files a repair sweep checked and left alone. Always 0 for a batch edit,
    /// which writes every track it is given.
    #[serde(default)]
    pub skipped: u64,
}

#[tauri::command]
pub fn cancel_batch(batch_state: State<BatchState>) {
    batch_state.cancel.store(true, Ordering::SeqCst);
}

const BATCH_FIELDS: [&str; 9] =
    ["title", "artist", "album", "genre", "label", "year", "bpm", "key", "comment"];

/// Applies a partial metadata update to many tracks on a background thread:
/// only the ticked `apply` fields are written, everything else untouched.
/// File first, DB after, per track. Emits batch:* progress; failures (locked/
/// read-only/missing) are counted and named for the summary toast.
#[tauri::command]
pub fn update_tracks_metadata(
    track_ids: Vec<i64>,
    update: metadata::TagUpdate,
    apply: Vec<String>,
    app: AppHandle,
    state: State<AppState>,
    batch_state: State<BatchState>,
) -> Result<u64, String> {
    let update = update.normalized();
    let apply: Vec<String> = apply
        .into_iter()
        .filter(|f| BATCH_FIELDS.contains(&f.as_str()))
        .collect();
    if apply.is_empty() || track_ids.is_empty() {
        let _ = app.emit(
            "batch:done",
            BatchDone { done: 0, failed: 0, total: 0, failed_names: Vec::new(), skipped: 0 },
        );
        return Ok(0);
    }
    if batch_state.running.swap(true, Ordering::SeqCst) {
        return Err("a metadata update is already running".into());
    }
    batch_state.cancel.store(false, Ordering::SeqCst);

    let targets: Vec<(i64, PathBuf)> = {
        let conn = match state.conn.lock() {
            Ok(c) => c,
            Err(_) => {
                batch_state.running.store(false, Ordering::SeqCst);
                return Err(LOCK_ERR.to_string());
            }
        };
        track_ids
            .iter()
            .filter_map(|id| db::track_abs_path(&conn, *id).ok().map(|p| (*id, p)))
            .collect()
    };
    let total = targets.len() as u64;
    if total == 0 {
        batch_state.running.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "batch:done",
            BatchDone { done: 0, failed: 0, total: 0, failed_names: Vec::new(), skipped: 0 },
        );
        return Ok(0);
    }

    let db_path = state.db_path.clone();
    let running = batch_state.running.clone();
    let cancel = batch_state.cancel.clone();
    std::thread::spawn(move || {
        let conn = match db::open(&db_path) {
            Ok(c) => c,
            Err(_) => {
                running.store(false, Ordering::SeqCst);
                let _ = app.emit(
                    "batch:done",
                    BatchDone { done: 0, failed: total, total, failed_names: vec!["database".into()], skipped: 0 },
                );
                return;
            }
        };
        let (mut done, mut failed) = (0u64, 0u64);
        let mut failed_names: Vec<String> = Vec::new();
        for (id, path) in &targets {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            let res = metadata::apply_partial(path, &update, &apply).and_then(|_| {
                let fs_meta = std::fs::metadata(path)?;
                let modified_at = fs_meta
                    .modified()
                    .map(scanner::mtime_string)
                    .unwrap_or_default();
                db::apply_partial_metadata(&conn, *id, &update, &apply, fs_meta.len() as i64, &modified_at)
            });
            match res {
                Ok(_) => done += 1,
                Err(_) => {
                    failed += 1;
                    failed_names.push(
                        path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string()),
                    );
                }
            }
            let _ = app.emit("batch:progress", BatchProgress { done, failed, total });
        }
        running.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "batch:done",
            BatchDone { done, failed, total, failed_names, skipped: 0 },
        );
    });
    Ok(total)
}

// ---------------------------------------------------------------------
// Crate-wide tag repair
// ---------------------------------------------------------------------

/// Writes the metadata SpinDex holds into each track's file, so other players
/// — Serato above all — see what SpinDex shows.
///
/// This is the EXPORT direction, database to file. It matters most for BPM and
/// key: analysis stores those only in the database, so before this existed an
/// analyzed Camelot key could never reach Serato at all.
///
/// Two rules keep it safe. Only fields SpinDex actually holds are written
/// (`fields_with_values`), so an unanalyzed track's empty BPM never blanks the
/// file's own tag — that would be destroying data, not exporting it. And a
/// file already carrying SpinDex's values, needing no structural repair, is
/// skipped: rewriting a WAV rewrites the whole file, so a sweep over a crate
/// on a USB stick must converge to zero work rather than move gigabytes.
///
/// Going through `apply_partial` also applies every file-level repair on the
/// way (picture encoding, ID3 chunk consolidation, the AIFF pad fix, the
/// per-container ID3 version), so files written by older builds heal here too.
///
/// Shares `BatchState` and the `batch:*` events with the batch editor: the
/// sidebar pill, its cancel button and the summary toast all work unchanged,
/// and the two can never run at once — both write tags.
fn spawn_tag_repair(
    targets: Vec<(i64, PathBuf)>,
    app: AppHandle,
    db_path: PathBuf,
    batch_state: &BatchState,
) -> u64 {
    let total = targets.len() as u64;
    if total == 0 {
        batch_state.running.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "batch:done",
            BatchDone { done: 0, failed: 0, total: 0, failed_names: Vec::new(), skipped: 0 },
        );
        return 0;
    }
    let running = batch_state.running.clone();
    let cancel = batch_state.cancel.clone();
    std::thread::spawn(move || {
        let conn = match db::open(&db_path) {
            Ok(c) => c,
            Err(_) => {
                running.store(false, Ordering::SeqCst);
                let _ = app.emit(
                    "batch:done",
                    BatchDone {
                        done: 0,
                        failed: total,
                        total,
                        failed_names: vec!["database".into()],
                        skipped: 0,
                    },
                );
                return;
            }
        };
        let (mut done, mut failed, mut skipped) = (0u64, 0u64, 0u64);
        let mut failed_names: Vec<String> = Vec::new();
        for (id, path) in &targets {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            let res = (|| -> anyhow::Result<bool> {
                let update = db::track_tag_update(&conn, *id)?.normalized();
                let apply = metadata::fields_with_values(&update);
                let meta = metadata::read(path)?;
                // Nothing to do when the file already carries SpinDex's values
                // AND needs no structural repair — rewriting a WAV rewrites
                // the whole file, so the sweep must converge to zero work.
                if metadata::tags_already_match(&meta, &update, &apply)
                    && !metadata::needs_tag_repair(path)
                {
                    return Ok(false);
                }
                metadata::apply_partial(path, &update, &apply)?;
                let fs_meta = std::fs::metadata(path)?;
                let modified_at = fs_meta
                    .modified()
                    .map(scanner::mtime_string)
                    .unwrap_or_default();
                db::update_artwork_flag(
                    &conn,
                    *id,
                    meta.has_artwork,
                    fs_meta.len() as i64,
                    &modified_at,
                )?;
                Ok(true)
            })();
            let res = match res {
                Ok(false) => {
                    skipped += 1;
                    let _ = app.emit(
                        "batch:progress",
                        BatchProgress { done: done + skipped, failed, total },
                    );
                    continue;
                }
                Ok(true) => Ok(()),
                Err(e) => Err(e),
            };
            match res {
                Ok(_) => done += 1,
                Err(_) => {
                    failed += 1;
                    failed_names.push(
                        path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string()),
                    );
                }
            }
            let _ = app.emit(
                "batch:progress",
                BatchProgress { done: done + skipped, failed, total },
            );
        }
        running.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "batch:done",
            BatchDone { done, failed, total, failed_names, skipped },
        );
    });
    total
}

/// Resolves the targets and starts the sweep, or releases the single-flight
/// flag if resolution failed.
fn start_tag_repair(
    targets: Result<Vec<(i64, PathBuf)>, String>,
    app: AppHandle,
    db_path: PathBuf,
    batch_state: &BatchState,
) -> Result<u64, String> {
    match targets {
        Ok(t) => Ok(spawn_tag_repair(t, app, db_path, batch_state)),
        Err(e) => {
            batch_state.running.store(false, Ordering::SeqCst);
            Err(e)
        }
    }
}

/// Repairs the tags of every track directly inside one folder-crate.
#[tauri::command]
pub fn repair_folder_crate_tags(
    library_id: i64,
    folder: String,
    app: AppHandle,
    state: State<AppState>,
    batch_state: State<BatchState>,
) -> Result<u64, String> {
    safe_crate_path(&folder)?;
    if batch_state.running.swap(true, Ordering::SeqCst) {
        return Err("a metadata update is already running".into());
    }
    batch_state.cancel.store(false, Ordering::SeqCst);
    let targets = (|| -> Result<Vec<(i64, PathBuf)>, String> {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        // the crate's own membership, not the loaded page — a crate past the
        // 500-row page size would otherwise have its tail silently skipped
        let rows = db::crate_track_files(&conn, library_id, &folder).map_err(err)?;
        Ok(rows
            .iter()
            .filter_map(|t| db::track_abs_path(&conn, t.id).ok().map(|p| (t.id, p)))
            .collect())
    })();
    start_tag_repair(targets, app, state.db_path.clone(), &batch_state)
}

/// Repairs the tags of every track a Serato crate links to.
#[tauri::command]
pub fn repair_serato_crate_tags(
    letter: String,
    file: String,
    app: AppHandle,
    state: State<AppState>,
    batch_state: State<BatchState>,
) -> Result<u64, String> {
    if batch_state.running.swap(true, Ordering::SeqCst) {
        return Err("a metadata update is already running".into());
    }
    batch_state.cancel.store(false, Ordering::SeqCst);
    let targets = (|| -> Result<Vec<(i64, PathBuf)>, String> {
        let path = serato_crate_path(&letter, &file)?;
        let bytes = std::fs::read(&path).map_err(|e| format!("reading crate: {e}"))?;
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        // same resolution as the crate viewer, so the sweep covers exactly the
        // tracks the crate shows
        let (_, ids, _) = resolve_crate_entries(
            &conn,
            &letter,
            crate::core::serato::parse_crate(&bytes).into_iter().map(Some),
        );
        Ok(ids
            .iter()
            .filter_map(|id| db::track_abs_path(&conn, *id).ok().map(|p| (*id, p)))
            .collect())
    })();
    start_tag_repair(targets, app, state.db_path.clone(), &batch_state)
}

// ---------------------------------------------------------------------
// Track sets: libraries, folder crates, Serato crates, Rekordbox playlists
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrackSetRef {
    Library { id: i64 },
    Folder { library_id: i64, path: String },
    Serato { letter: String, file: String },
    Rekordbox { letter: String, playlist_id: u32 },
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackSetInfo {
    pub set: TrackSetRef,
    pub name: String,
    pub kind_label: &'static str,
    pub track_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetComparison {
    pub target: TrackSetRef,
    pub name: String,
    pub kind_label: &'static str,
    #[serde(flatten)]
    pub counts: crate::core::dupes::SetCounts,
}

/// Drive letters ("D:") that hold at least one library.
fn library_letters(conn: &rusqlite::Connection) -> Result<Vec<String>, String> {
    let mut letters: Vec<String> = db::list_libraries(conn)
        .map_err(err)?
        .into_iter()
        .filter_map(|l| l.root_path.get(..2).map(str::to_string))
        .filter(|l| l.ends_with(':'))
        .collect();
    letters.sort();
    letters.dedup();
    Ok(letters)
}

/// Every set the comparison can use, with its tracks resolved. Serato and
/// Rekordbox sets appear only for drives where their data is readable.
fn track_sets_with_ids(
    conn: &rusqlite::Connection,
) -> Result<Vec<(TrackSetInfo, Vec<i64>)>, String> {
    let mut out = Vec::new();
    for lib in db::list_libraries(conn).map_err(err)? {
        let set = TrackSetRef::Library { id: lib.id };
        let ids = resolve_set(conn, &set)?;
        out.push((
            TrackSetInfo { set, name: lib.name.clone(), kind_label: "Library", track_count: ids.len() },
            ids,
        ));
        for c in db::crate_folders(conn, lib.id).map_err(err)? {
            if c.track_count == 0 {
                continue;
            }
            let set = TrackSetRef::Folder { library_id: lib.id, path: c.path.clone() };
            let ids = resolve_set(conn, &set)?;
            out.push((
                TrackSetInfo {
                    set,
                    name: format!("{} / {}", lib.name, c.path),
                    kind_label: "Folder",
                    track_count: ids.len(),
                },
                ids,
            ));
        }
    }
    for letter in library_letters(conn)? {
        let subcrates = PathBuf::from(format!("{letter}\\_Serato_\\Subcrates"));
        for (file, entries) in crate::core::djlib::serato::read_crate_files(&subcrates) {
            let (_, ids, _) = crate::core::stick_path::resolve_crate_entries(
                conn,
                &letter,
                entries.into_iter().map(Some),
            );
            let name = format!("{} ({letter})", crate::core::serato::crate_display_name(&file));
            out.push((
                TrackSetInfo {
                    set: TrackSetRef::Serato { letter: letter.clone(), file },
                    name,
                    kind_label: "Serato",
                    track_count: ids.len(),
                },
                ids,
            ));
        }
        let pdb = PathBuf::from(format!("{letter}\\PIONEER\\rekordbox\\export.pdb"));
        if let Ok(bytes) = std::fs::read(&pdb) {
            if let Ok(playlists) = crate::core::rekordbox::resolve_playlists(conn, &letter, &bytes) {
                for p in playlists.into_iter().filter(|p| !p.is_folder) {
                    let name = format!("{} ({letter})", p.name);
                    out.push((
                        TrackSetInfo {
                            set: TrackSetRef::Rekordbox { letter: letter.clone(), playlist_id: p.id },
                            name,
                            kind_label: "Rekordbox",
                            track_count: p.track_ids.len(),
                        },
                        p.track_ids,
                    ));
                }
            }
        }
    }
    Ok(out)
}

fn track_sets(conn: &rusqlite::Connection) -> Result<Vec<TrackSetInfo>, String> {
    Ok(track_sets_with_ids(conn)?.into_iter().map(|(i, _)| i).collect())
}

/// The present track ids of one set, resolved the same way the sidebar shows it.
fn resolve_set(conn: &rusqlite::Connection, set: &TrackSetRef) -> Result<Vec<i64>, String> {
    match set {
        TrackSetRef::Library { id } => {
            let mut stmt = conn
                .prepare("SELECT id FROM track WHERE library_id = ?1 AND is_missing = 0")
                .map_err(|e| e.to_string())?;
            let ids = stmt
                .query_map([id], |r| r.get::<_, i64>(0))
                .map_err(|e| e.to_string())?
                .flatten()
                .collect();
            Ok(ids)
        }
        TrackSetRef::Folder { library_id, path } => Ok(db::crate_track_files(conn, *library_id, path)
            .map_err(err)?
            .into_iter()
            .map(|t| t.id)
            .collect()),
        TrackSetRef::Serato { letter, file } => {
            let path = serato_crate_path(letter, file)?;
            let bytes = std::fs::read(&path).map_err(|e| format!("reading crate: {e}"))?;
            let (_, ids, _) = crate::core::stick_path::resolve_crate_entries(
                conn,
                letter,
                crate::core::serato::parse_crate(&bytes).into_iter().map(Some),
            );
            Ok(ids)
        }
        TrackSetRef::Rekordbox { letter, playlist_id } => {
            let pdb = PathBuf::from(format!("{letter}\\PIONEER\\rekordbox\\export.pdb"));
            let bytes =
                std::fs::read(&pdb).map_err(|e| format!("reading Rekordbox export: {e}"))?;
            crate::core::rekordbox::resolve_playlists(conn, letter, &bytes)
                .map_err(err)?
                .into_iter()
                .find(|p| p.id == *playlist_id)
                .map(|p| p.track_ids)
                .ok_or_else(|| "that Rekordbox playlist no longer exists".to_string())
        }
    }
}

/// Every other set ranked by how much of the source it holds. The source is
/// loaded once; each candidate is loaded and compared in memory.
fn compare_set_rows(
    conn: &rusqlite::Connection,
    source: &TrackSetRef,
) -> Result<Vec<SetComparison>, String> {
    use crate::core::dupes::{counts_between, load_scope, Scope};
    let src_ids = resolve_set(conn, source)?;
    let src = load_scope(conn, Scope::Ids(&src_ids)).map_err(err)?;
    let mut rows = Vec::new();
    for (info, ids) in track_sets_with_ids(conn)? {
        if &info.set == source {
            continue;
        }
        let target = load_scope(conn, Scope::Ids(&ids)).map_err(err)?;
        let counts = counts_between(&src, &target);
        rows.push(SetComparison { target: info.set, name: info.name, kind_label: info.kind_label, counts });
    }
    rows.sort_by(|a, b| {
        b.counts
            .pct_of_source
            .partial_cmp(&a.counts.pct_of_source)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.name.cmp(&b.name))
    });
    Ok(rows)
}

/// Runs `f` on its own SQLite connection on a blocking thread: set commands
/// read crate files and export.pdb from USB and compare many sets, which must
/// neither stall the UI thread nor hold the shared connection's lock.
async fn on_own_conn<T: Send + 'static>(
    db_path: PathBuf,
    f: impl FnOnce(&rusqlite::Connection) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = db::open(&db_path).map_err(err)?;
        f(&conn)
    })
    .await
    .map_err(|e| format!("task failed: {e}"))?
}

#[tauri::command]
pub async fn list_track_sets(state: State<'_, AppState>) -> Result<Vec<TrackSetInfo>, String> {
    on_own_conn(state.db_path.clone(), |conn| track_sets(conn)).await
}

#[tauri::command]
pub async fn compare_set(
    source: TrackSetRef,
    state: State<'_, AppState>,
) -> Result<Vec<SetComparison>, String> {
    on_own_conn(state.db_path.clone(), move |conn| compare_set_rows(conn, &source)).await
}

#[tauri::command]
pub async fn compare_set_details(
    source: TrackSetRef,
    target: TrackSetRef,
    level: Option<String>,
    limit: i64,
    offset: i64,
    state: State<'_, AppState>,
) -> Result<crate::core::dupes::MatchPage, String> {
    use crate::core::dupes::{compare_details_scoped, Scope};
    on_own_conn(state.db_path.clone(), move |conn| {
        let s = resolve_set(conn, &source)?;
        let t = resolve_set(conn, &target)?;
        compare_details_scoped(
            conn,
            Scope::Ids(&s),
            Scope::Ids(&t),
            level.as_deref(),
            limit.clamp(1, 5000),
            offset.max(0),
        )
        .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn compare_set_unmatched(
    source: TrackSetRef,
    target: TrackSetRef,
    state: State<'_, AppState>,
) -> Result<crate::core::dupes::Unmatched, String> {
    use crate::core::dupes::{unmatched_scoped, Scope};
    on_own_conn(state.db_path.clone(), move |conn| {
        let s = resolve_set(conn, &source)?;
        let t = resolve_set(conn, &target)?;
        unmatched_scoped(conn, Scope::Ids(&s), Scope::Ids(&t), 2000).map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn find_duplicates_in_set(
    set: TrackSetRef,
    state: State<'_, AppState>,
) -> Result<crate::core::dupes::DupeReport, String> {
    use crate::core::dupes::{find_duplicates_scoped, Scope};
    on_own_conn(state.db_path.clone(), move |conn| {
        let ids = resolve_set(conn, &set)?;
        find_duplicates_scoped(conn, Scope::Ids(&ids)).map_err(err)
    })
    .await
}

// ---------------------------------------------------------------------
// DJ sync hub: Serato → Rekordbox XML
// ---------------------------------------------------------------------

#[derive(Default)]
pub struct SyncState {
    pub running: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Serialize, Clone)]
pub struct SyncProgress {
    pub done: u64,
    pub total: u64,
}

#[derive(Serialize, Clone, Default)]
pub struct SyncDone {
    pub ok: bool,
    pub cancelled: bool,
    pub error: Option<String>,
    pub path: Option<String>,
    pub tracks: u64,
    pub playlists: u64,
    pub grids: u64,
    pub cues: u64,
    pub loops: u64,
    pub unreadable: Vec<String>,
    pub dangling: u64,
    pub losses: Vec<crate::core::djlib::capabilities::Loss>,
}

#[derive(Serialize)]
pub struct SyncPreview {
    pub tracks: u64,
    pub crates: u64,
    pub out_path: String,
    pub capability_losses: Vec<String>,
}

/// The stick's Rekordbox XML: it travels with the stick, and SpinDex reads it
/// back to show the "Prepared for Rekordbox" playlists. Rekordbox's own files
/// under PIONEER\ are never touched.
fn export_path(letter: &str) -> PathBuf {
    PathBuf::from(format!("{letter}\\SpinDex\\rekordbox.xml"))
}

/// The playlist tree of a SpinDex-written rekordbox.xml, resolved against the
/// stick's indexed tracks, in the same shape as the export.pdb viewer.
fn prepared_crates(
    conn: &rusqlite::Connection,
    letter: &str,
    xml: &str,
) -> Result<Vec<RekordboxCrateInfo>, String> {
    let libs = crate::core::stick_path::libraries(conn, letter)?;
    let nodes = crate::core::rekordbox_xml::read_playlists(xml).map_err(err)?;
    Ok(nodes
        .into_iter()
        .map(|n| {
            let mut ids = Vec::new();
            let mut missing = 0usize;
            for rel in &n.stick_paths {
                match crate::core::stick_path::resolve_with(&libs, conn, letter, rel) {
                    Some(id) => ids.push(id),
                    None => missing += 1,
                }
            }
            RekordboxCrateInfo {
                id: n.id,
                parent_id: n.parent_id,
                name: n.name,
                is_folder: n.is_folder,
                track_count: ids.len(),
                track_ids: ids,
                missing,
            }
        })
        .collect())
}

/// Playlists SpinDex has prepared for Rekordbox on this stick (read from its
/// rekordbox.xml). No file yet = an empty list, not an error.
#[tauri::command]
pub fn list_prepared_rekordbox(
    letter: String,
    state: State<AppState>,
) -> Result<Vec<RekordboxCrateInfo>, String> {
    let Ok(xml) = std::fs::read_to_string(export_path(&letter)) else {
        return Ok(Vec::new());
    };
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    prepared_crates(&conn, &letter, &xml)
}

#[tauri::command]
pub fn sync_preview(
    letter: String,
    crates: Option<Vec<String>>,
    state: State<AppState>,
) -> Result<SyncPreview, String> {
    use crate::core::djlib::capabilities::{capability_gaps, REKORDBOX_XML, SERATO};
    let (tracks, crates) = {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        export_counts(&conn, &letter, crates.as_deref())?
    };
    Ok(SyncPreview {
        tracks,
        crates,
        out_path: export_path(&letter).display().to_string(),
        capability_losses: capability_gaps(&SERATO, &REKORDBOX_XML),
    })
}

/// (tracks, crates) an export covers: every track on the stick and every
/// crate, or only the named crates and the tracks they reference.
fn export_counts(
    conn: &rusqlite::Connection,
    letter: &str,
    only: Option<&[String]>,
) -> Result<(u64, u64), String> {
    use crate::core::djlib::serato::{crate_track_ids, read_crate_files, select_crates};
    let subcrates = PathBuf::from(format!("{letter}\\_Serato_\\Subcrates"));
    let picked = select_crates(read_crate_files(&subcrates), only);
    let stick = db::stick_tracks(conn, letter).map_err(|e| e.to_string())?;
    let tracks = match only {
        None => stick.len(),
        Some(_) => {
            let resolved: Vec<(String, Vec<i64>)> = picked
                .iter()
                .map(|(file, entries)| {
                    let (_, ids, _) = crate::core::stick_path::resolve_crate_entries(
                        conn,
                        letter,
                        entries.iter().cloned().map(Some),
                    );
                    (file.clone(), ids)
                })
                .collect();
            let keep = crate_track_ids(&resolved);
            stick.iter().filter(|t| keep.contains(&t.id)).count()
        }
    };
    Ok((tracks as u64, picked.len() as u64))
}

#[tauri::command]
pub fn cancel_sync(sync_state: State<SyncState>) {
    sync_state.cancel.store(true, Ordering::SeqCst);
}

/// Reads the stick through the Serato adapter and writes Rekordbox XML, on a
/// background thread with its own SQLite connection. Read-only towards the
/// stick and every audio file.
#[tauri::command]
pub fn sync_to_rekordbox(
    letter: String,
    crates: Option<Vec<String>>,
    app: AppHandle,
    state: State<AppState>,
    sync_state: State<SyncState>,
) -> Result<u64, String> {
    use crate::core::djlib::{
        capabilities::{losses, REKORDBOX_XML},
        endpoint::{DjEndpoint, CANCELLED},
        rekordbox::RekordboxXmlEndpoint,
        serato::SeratoEndpoint,
    };
    if sync_state.running.swap(true, Ordering::SeqCst) {
        return Err("an export is already running".into());
    }
    sync_state.cancel.store(false, Ordering::SeqCst);
    let out_path = export_path(&letter);
    let total = {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string());
        match conn.and_then(|c| export_counts(&c, &letter, crates.as_deref())) {
            Ok((t, _)) => t,
            Err(e) => {
                sync_state.running.store(false, Ordering::SeqCst);
                return Err(e);
            }
        }
    };
    let running = sync_state.running.clone();
    let cancel = sync_state.cancel.clone();
    let db_path = state.db_path.clone();
    let version = app.package_info().version.to_string();
    std::thread::spawn(move || {
        // Released on every exit, a panic included: a stuck flag would refuse
        // every later export until the app restarts.
        let guard = RunningGuard(running);
        let run = || -> SyncDone {
            let conn = match db::open(&db_path) {
                Ok(c) => c,
                Err(e) => return SyncDone { error: Some(e.to_string()), ..Default::default() },
            };
            let source =
                SeratoEndpoint { conn: &conn, letter: letter.clone(), only_crates: crates.clone() };
            let emit_progress = |done: usize, total: usize| {
                let _ = app.emit("sync:progress", SyncProgress { done: done as u64, total: total as u64 });
            };
            let read = match source.read(&emit_progress, &cancel) {
                Ok(r) => r,
                Err(e) if e.to_string() == CANCELLED => {
                    return SyncDone { cancelled: true, ..Default::default() }
                }
                Err(e) => return SyncDone { error: Some(format!("{e:#}")), ..Default::default() },
            };
            let target = RekordboxXmlEndpoint { out_path: out_path.clone(), product_version: version.clone() };
            let lib = &read.library;
            let mut done = SyncDone {
                tracks: lib.tracks.len() as u64,
                playlists: lib.playlist_count() as u64,
                grids: lib.tracks.iter().filter(|t| !t.grid.is_empty()).count() as u64,
                cues: lib.tracks.iter().map(|t| t.cues.len() as u64).sum(),
                loops: lib.tracks.iter().map(|t| t.loops.len() as u64).sum(),
                unreadable: read.unreadable.clone(),
                dangling: read.dangling as u64,
                losses: losses(lib, &REKORDBOX_XML),
                ..Default::default()
            };
            match target.write(lib, &cancel) {
                Ok(w) => {
                    done.ok = true;
                    done.path = Some(w.written.display().to_string());
                }
                Err(e) if e.to_string() == CANCELLED => done.cancelled = true,
                Err(e) => done.error = Some(format!("{e:#}")),
            }
            done
        };
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|_| {
            SyncDone { error: Some("the export stopped unexpectedly".into()), ..Default::default() }
        });
        drop(guard); // free before announcing, so a follow-up export is accepted
        let _ = app.emit("sync:done", done);
    });
    Ok(total)
}

/// Clears the single-flight flag when dropped, including during a panic.
struct RunningGuard(Arc<AtomicBool>);

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------
// DJ sync hub SP2: Rekordbox XML → Serato (grids, cues, loops, colour, crates)
// ---------------------------------------------------------------------

#[derive(Serialize, Clone)]
pub struct RbImportProgress {
    pub done: u64,
    pub total: u64,
}

#[derive(Serialize, Clone, Default)]
pub struct RbImportDone {
    pub files_written: u64,
    pub failed_names: Vec<String>,
    pub crates_created: u64,
    pub crates_merged: u64,
    pub links_added: u64,
    pub cancelled: bool,
    pub error: Option<String>,
}

/// What importing `xml_path` onto this stick would write. Reads only; runs off
/// the event loop with its own connection, since it reads the Serato data of
/// every matched file on the stick.
#[tauri::command]
pub async fn rekordbox_import_preview(
    letter: String,
    xml_path: String,
    state: State<'_, AppState>,
) -> Result<crate::core::djlib::serato_import::ImportPreview, String> {
    let db_path = state.db_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let xml = std::fs::read_to_string(&xml_path).map_err(|e| format!("reading {xml_path}: {e}"))?;
        let conn = db::open(&db_path).map_err(err)?;
        crate::core::djlib::serato_import::prepare(&conn, &letter, &xml)
            .map(|p| p.preview)
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| format!("preview failed: {e}"))?
}

#[tauri::command]
pub fn cancel_rekordbox_import(batch_state: State<BatchState>) {
    batch_state.cancel.store(true, Ordering::SeqCst);
}

/// Writes the Rekordbox import onto the stick on a background thread: per
/// file a first-touch backup, then the grid and the Markers2 data (only the
/// gaps, decided by `prepare`), then the crates. Shares `BatchState` with the
/// other tag writers, so it never runs beside a batch edit or a sweep.
#[tauri::command]
pub async fn rekordbox_import(
    letter: String,
    xml_path: String,
    app: AppHandle,
    state: State<'_, AppState>,
    batch_state: State<'_, BatchState>,
) -> Result<u64, String> {
    // off the event loop: the XML can be tens of MB
    let (running_now, xml) = tauri::async_runtime::spawn_blocking(move || {
        let xml = std::fs::read_to_string(&xml_path).map_err(|e| format!("reading {xml_path}: {e}"));
        (serato_running(), xml)
    })
    .await
    .map_err(|e| format!("import failed: {e}"))?;
    if running_now {
        return Err("Close Serato first — it rewrites cues and crates from memory on exit".into());
    }
    let xml = xml?;
    if batch_state.running.swap(true, Ordering::SeqCst) {
        return Err("a metadata update is already running".into());
    }
    batch_state.cancel.store(false, Ordering::SeqCst);
    let running = batch_state.running.clone();
    let cancel = batch_state.cancel.clone();
    let db_path = state.db_path.clone();
    std::thread::spawn(move || {
        let guard = RunningGuard(running);
        let run = || -> RbImportDone {
            let conn = match db::open(&db_path) {
                Ok(c) => c,
                Err(e) => return RbImportDone { error: Some(e.to_string()), ..Default::default() },
            };
            let is_cancelled = || cancel.load(Ordering::SeqCst);
            let prepared =
                match crate::core::djlib::serato_import::prepare_cancellable(&conn, &letter, &xml, &is_cancelled) {
                    Ok(Some(p)) => p,
                    Ok(None) => return RbImportDone { cancelled: true, ..Default::default() },
                    Err(e) => return RbImportDone { error: Some(format!("{e:#}")), ..Default::default() },
                };
            if !prepared.preview.fits() {
                return RbImportDone {
                    error: Some("not enough free space on the stick for the backups".into()),
                    ..Default::default()
                };
            }
            let total = prepared.tracks.len() as u64;
            let mut done = RbImportDone::default();
            for (i, t) in prepared.tracks.iter().enumerate() {
                if cancel.load(Ordering::SeqCst) {
                    done.cancelled = true;
                    return done;
                }
                // before every file: the process-snapshot check is cheap, and
                // Serato opened mid-run would rewrite what we just wrote
                if serato_running() {
                    done.error = Some("Serato was opened during the import; stopped".into());
                    return done;
                }
                let res = (|| -> anyhow::Result<()> {
                    let target = grid_target(&conn, t.track_id).context("track not found")?;
                    let backup = crate::core::enhance::backup_path_for(&target.root, &target.rel);
                    crate::core::enhance::ensure_backup(&t.abs, &backup)?;
                    // grid and cues in one save: a rewrite copies the whole file
                    metadata::write_serato_data(
                        &t.abs,
                        t.edit.grid.as_ref(),
                        t.edit.markers.as_deref(),
                    )?;
                    if t.edit.grid.is_some() {
                        db::stamp_serato_grid_written(&conn, t.track_id)?;
                        // a grid the DJ made in Rekordbox is theirs: protect it
                        // from the regenerate sweep, like a hand-set grid
                        db::set_serato_grid_source(&conn, t.track_id, "manual")?;
                    }
                    Ok(())
                })();
                match res {
                    Ok(()) => done.files_written += 1,
                    Err(_) => done.failed_names.push(
                        t.abs
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| t.abs.display().to_string()),
                    ),
                }
                let _ = app.emit("rbimport:progress", RbImportProgress { done: i as u64 + 1, total });
            }
            if cancel.load(Ordering::SeqCst) {
                done.cancelled = true;
                return done;
            }
            if serato_running() {
                done.error = Some("Serato was opened during the import; crates not written".into());
                return done;
            }
            for c in &prepared.crates {
                let res = (|| -> Result<Vec<String>, String> {
                    let path = serato_crate_path(&letter, &c.file)?;
                    if let Some(dir) = path.parent() {
                        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                    }
                    let mut chunks = if path.exists() {
                        let bytes = std::fs::read(&path).map_err(|e| format!("reading crate: {e}"))?;
                        if !crate::core::serato::reads_to_the_end(&bytes) {
                            // writing it back would drop the unreadable tail's links
                            return Err("the crate file looks damaged; left as it is".into());
                        }
                        crate::core::serato::read_chunks(&bytes)
                    } else {
                        crate::core::serato::read_chunks(&crate::core::serato::empty_crate())
                    };
                    crate::core::serato::add_tracks(&mut chunks, &c.add);
                    serato_backup_and_write(&path, &crate::core::serato::write_chunks(&chunks))?;
                    Ok(chunks.iter().filter_map(crate::core::serato::otrk_path).collect())
                })();
                match res {
                    Ok(paths) => {
                        record_crate_intent(&letter, &c.file, paths);
                        if c.exists {
                            done.crates_merged += (!c.add.is_empty()) as u64;
                        } else {
                            done.crates_created += 1;
                        }
                        done.links_added += c.add.len() as u64;
                    }
                    Err(e) => done.failed_names.push(format!("{} ({e})", c.file)),
                }
            }
            done
        };
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|_| {
            RbImportDone { error: Some("the import stopped unexpectedly".into()), ..Default::default() }
        });
        drop(guard);
        let _ = app.emit("rbimport:done", done);
    });
    Ok(0)
}

// ---------------------------------------------------------------------
// Serato beat-grid regeneration
// ---------------------------------------------------------------------

/// One track queued for a Serato beat-grid write.
pub(crate) struct GridTarget {
    id: i64,
    abs: PathBuf,
    root: PathBuf,
    rel: String,
}

/// Resolves a track id to a grid target, or `None` when the row can't be
/// mapped to a reachable file. Shared with the crate-sweep commands (Task 9).
fn grid_target(conn: &rusqlite::Connection, id: i64) -> Option<GridTarget> {
    let (root, abs) = db::track_root_and_abs_path(conn, id).ok()?;
    let rel = abs
        .strip_prefix(&root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    Some(GridTarget { id, abs, root, rel })
}

/// Builds and writes a Serato beat grid into each target file on a background
/// thread. Reuses `BatchState` + the `batch:*` events (pill, cancel, toast)
/// exactly like the tag-repair sweep — both write tags and must never run at
/// once. Per track: build the grid from the stored downbeats, first-touch
/// backup, write the GEOB/Vorbis frame, stamp the row, and keep `track.bpm`
/// in step with a constant grid. A track with too few downbeats is skipped,
/// not failed.
fn spawn_grid_run(
    targets: Vec<GridTarget>,
    serato_running_now: bool,
    app: AppHandle,
    db_path: PathBuf,
    batch_state: &BatchState,
) -> u64 {
    let total = targets.len() as u64;
    if total == 0 {
        batch_state.running.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "batch:done",
            BatchDone { done: 0, failed: 0, total: 0, failed_names: Vec::new(), skipped: 0 },
        );
        return 0;
    }
    let running = batch_state.running.clone();
    let cancel = batch_state.cancel.clone();
    std::thread::spawn(move || {
        let conn = match db::open(&db_path) {
            Ok(c) => c,
            Err(_) => {
                running.store(false, Ordering::SeqCst);
                let _ = app.emit(
                    "batch:done",
                    BatchDone {
                        done: 0,
                        failed: total,
                        total,
                        failed_names: vec!["database".into()],
                        skipped: 0,
                    },
                );
                return;
            }
        };
        let (mut done, mut failed, mut skipped) = (0u64, 0u64, 0u64);
        let mut failed_names: Vec<String> = Vec::new();
        for t in &targets {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            let downs = db::downbeat_grid(&conn, t.id).ok().flatten().unwrap_or_default();
            // Below the grid minimum (or a stored-but-empty blob for a short
            // track): nothing to write, and not a failure.
            if downs.len() < crate::core::serato_beatgrid::MIN_DOWNBEATS {
                skipped += 1;
                let _ = app.emit(
                    "batch:progress",
                    BatchProgress { done: done + skipped, failed, total },
                );
                continue;
            }
            let res = (|| -> anyhow::Result<()> {
                if serato_running_now {
                    anyhow::bail!("close Serato first");
                }
                let beats = db::beat_grid(&conn, t.id)?.unwrap_or_default();
                let drift: Option<f64> = conn.query_row(
                    "SELECT tempo_drift_pct FROM track WHERE id = ?1",
                    params![t.id],
                    |r| r.get(0),
                )?;
                let grid = crate::core::serato_beatgrid::build(
                    &downs,
                    &beats,
                    drift,
                    crate::core::serato_beatgrid::timing_offset(&t.abs),
                )?;
                let backup = crate::core::enhance::backup_path_for(&t.root, &t.rel);
                crate::core::enhance::ensure_backup(&t.abs, &backup)?;
                metadata::write_serato_beatgrid(&t.abs, &grid)?;
                db::stamp_serato_grid_written(&conn, t.id)?;
                db::set_serato_grid_source(&conn, t.id, "auto")?;
                // keep the DB bpm in step with a constant grid
                if let crate::core::serato_beatgrid::Beatgrid::Constant { bpm, .. } = &grid {
                    let _ = conn.execute(
                        "UPDATE track SET bpm = ?2 WHERE id = ?1",
                        params![t.id, *bpm as f64],
                    );
                }
                Ok(())
            })();
            match res {
                Ok(()) => done += 1,
                Err(_) => {
                    failed += 1;
                    failed_names.push(
                        t.abs
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| t.abs.display().to_string()),
                    );
                }
            }
            let _ = app.emit(
                "batch:progress",
                BatchProgress { done: done + skipped, failed, total },
            );
        }
        running.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "batch:done",
            BatchDone { done, failed, total, failed_names, skipped },
        );
    });
    total
}

/// Builds and writes a Serato beat grid for the selected tracks. `rewrite_all`
/// re-writes even tracks already gridded and not re-analyzed since.
#[tauri::command]
pub fn regenerate_serato_grid(
    track_ids: Vec<i64>,
    rewrite_all: bool,
    app: AppHandle,
    state: State<AppState>,
    batch_state: State<BatchState>,
) -> Result<u64, String> {
    if track_ids.is_empty() {
        return Ok(0);
    }
    if batch_state.running.swap(true, Ordering::SeqCst) {
        return Err("a metadata update is already running".into());
    }
    batch_state.cancel.store(false, Ordering::SeqCst);
    let serato_running_now = serato_running();
    let targets = (|| -> Result<Vec<GridTarget>, String> {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        let ids = db::tracks_needing_grid(&conn, &track_ids, rewrite_all).map_err(err)?;
        Ok(ids.iter().filter_map(|id| grid_target(&conn, *id)).collect())
    })();
    match targets {
        Ok(t) => Ok(spawn_grid_run(
            t,
            serato_running_now,
            app,
            state.db_path.clone(),
            &batch_state,
        )),
        Err(e) => {
            batch_state.running.store(false, Ordering::SeqCst);
            Err(e)
        }
    }
}

/// The grid the deck's grid editor opens with: the file's own Serato grid,
/// else SpinDex's detected grid, else the stored BPM. Never fails on an
/// unreadable file grid — it falls back and says so.
#[tauri::command]
pub fn track_grid(
    track_id: i64,
    state: State<AppState>,
) -> Result<grid_edit::StartingGrid, String> {
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let (_, abs) = db::track_root_and_abs_path(&conn, track_id).map_err(err)?;
    let file = metadata::read_serato_beatgrid(&abs).map_err(|_| ());
    let beats = db::beat_grid(&conn, track_id).map_err(err)?.unwrap_or_default();
    let downs = db::downbeat_grid(&conn, track_id).map_err(err)?.unwrap_or_default();
    let detected = crate::core::serato_beatgrid::build(
        &downs,
        &beats,
        None,
        crate::core::serato_beatgrid::timing_offset(&abs),
    )
    .ok();
    let stored_bpm: Option<f64> = conn
        .query_row("SELECT bpm FROM track WHERE id = ?1", params![track_id], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    Ok(grid_edit::starting_grid(file, detected, stored_bpm, beats.first().copied()))
}

/// Decodes a track at the grid editor's rate, off the event loop and without
/// holding the DB lock.
async fn decode_for_grid(track_id: i64, state: &AppState) -> Result<std::sync::Arc<Vec<f32>>, String> {
    let abs = {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        db::track_abs_path(&conn, track_id).map_err(err)?
    };
    tauri::async_runtime::spawn_blocking(move || -> Result<std::sync::Arc<Vec<f32>>, String> {
        let ffmpeg = analysis::find_ffmpeg()
            .ok_or("ffmpeg is not installed — run install-components.ps1")?;
        let pcm = analysis::decode_pcm(&ffmpeg, &abs, grid_edit::DETAIL_SAMPLE_RATE).map_err(err)?;
        Ok(std::sync::Arc::new(pcm))
    })
    .await
    .map_err(|e| format!("decode task failed: {e}"))?
}

/// The grid editor's waveform, laid out by `grid_edit::detail_payload`. The
/// decode is kept for `grid_colours`. Raw bytes reach the webview as an
/// ArrayBuffer (as JSON numbers this would be several MB per track).
#[tauri::command]
pub async fn grid_waveform(
    track_id: i64,
    state: State<'_, AppState>,
) -> Result<tauri::ipc::Response, String> {
    let pcm = decode_for_grid(track_id, &state).await?;
    if let Ok(mut cache) = state.grid_pcm.lock() {
        *cache = Some((track_id, pcm.clone()));
    }
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        grid_edit::detail_payload(&pcm, grid_edit::DETAIL_SAMPLE_RATE)
    })
    .await
    .map_err(|e| format!("waveform task failed: {e}"))?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// The grid editor's drum and harmony colourings (`grid_edit::colour_payload`),
/// asked for after the waveform so the waveform never waits on them. Reuses the
/// waveform's decode when it is this track's, and releases it.
#[tauri::command]
pub async fn grid_colours(
    track_id: i64,
    state: State<'_, AppState>,
) -> Result<tauri::ipc::Response, String> {
    // take the cached decode only when it is this track's: a stale request for
    // a track the editor already left must not discard the current one's
    let cached = state.grid_pcm.lock().ok().and_then(|mut c| {
        if c.as_ref().is_some_and(|(id, _)| *id == track_id) {
            c.take().map(|(_, pcm)| pcm)
        } else {
            None
        }
    });
    let pcm = match cached {
        Some(p) => p,
        None => decode_for_grid(track_id, &state).await?,
    };
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        grid_edit::colour_payload(&pcm, grid_edit::DETAIL_SAMPLE_RATE)
    })
    .await
    .map_err(|e| format!("colour task failed: {e}"))?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// Writes the grid editor's grid into the file (first-touch backup, then file,
/// then row) and marks it manual: one section is a constant grid, several a
/// tempo-changing one. Refused while Serato runs — checked before the DB lock
/// so the process snapshot never holds it.
#[tauri::command]
pub fn save_track_grid(
    track_id: i64,
    sections: Vec<grid_edit::GridSection>,
    state: State<AppState>,
) -> Result<(), String> {
    if serato_running() {
        return Err("Close Serato first — it rewrites grids from memory on exit".into());
    }
    let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
    let t = grid_target(&conn, track_id).ok_or("track not found")?;
    grid_edit::save_grid(&conn, t.id, &t.root, &t.rel, &t.abs, &sections).map_err(err)
}

/// Regenerates Serato beat grids for every track directly inside one
/// folder-crate. Resolves the crate's own membership (not the loaded page),
/// so a crate past the 500-row page size still has its tail covered.
/// `rewrite_all` re-writes even tracks already gridded and not re-analyzed
/// since. Returns the number of tracks queued.
#[tauri::command]
pub fn regenerate_folder_crate_grids(
    library_id: i64,
    folder: String,
    rewrite_all: bool,
    app: AppHandle,
    state: State<AppState>,
    batch_state: State<BatchState>,
) -> Result<u64, String> {
    safe_crate_path(&folder)?;
    if batch_state.running.swap(true, Ordering::SeqCst) {
        return Err("a metadata update is already running".into());
    }
    batch_state.cancel.store(false, Ordering::SeqCst);
    let serato_running_now = serato_running();
    let targets = (|| -> Result<Vec<GridTarget>, String> {
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        let rows = db::crate_track_files(&conn, library_id, &folder).map_err(err)?;
        let ids: Vec<i64> = rows.iter().map(|t| t.id).collect();
        let need = db::tracks_needing_grid(&conn, &ids, rewrite_all).map_err(err)?;
        Ok(need.iter().filter_map(|id| grid_target(&conn, *id)).collect())
    })();
    match targets {
        Ok(t) => Ok(spawn_grid_run(
            t,
            serato_running_now,
            app,
            state.db_path.clone(),
            &batch_state,
        )),
        Err(e) => {
            batch_state.running.store(false, Ordering::SeqCst);
            Err(e)
        }
    }
}

/// Regenerates Serato beat grids for every track a Serato crate links to.
/// Same resolution as the crate viewer, so the sweep covers exactly the
/// tracks the crate shows. Returns the number of tracks queued.
#[tauri::command]
pub fn regenerate_serato_crate_grids(
    letter: String,
    file: String,
    rewrite_all: bool,
    app: AppHandle,
    state: State<AppState>,
    batch_state: State<BatchState>,
) -> Result<u64, String> {
    if batch_state.running.swap(true, Ordering::SeqCst) {
        return Err("a metadata update is already running".into());
    }
    batch_state.cancel.store(false, Ordering::SeqCst);
    let serato_running_now = serato_running();
    let targets = (|| -> Result<Vec<GridTarget>, String> {
        let path = serato_crate_path(&letter, &file)?;
        let bytes = std::fs::read(&path).map_err(|e| format!("reading crate: {e}"))?;
        let conn = state.conn.lock().map_err(|_| LOCK_ERR.to_string())?;
        let (_, ids, _) = resolve_crate_entries(
            &conn,
            &letter,
            crate::core::serato::parse_crate(&bytes).into_iter().map(Some),
        );
        let need = db::tracks_needing_grid(&conn, &ids, rewrite_all).map_err(err)?;
        Ok(need.iter().filter_map(|id| grid_target(&conn, *id)).collect())
    })();
    match targets {
        Ok(t) => Ok(spawn_grid_run(
            t,
            serato_running_now,
            app,
            state.db_path.clone(),
            &batch_state,
        )),
        Err(e) => {
            batch_state.running.store(false, Ordering::SeqCst);
            Err(e)
        }
    }
}

// ---------------------------------------------------------------------
// Loudness enhancement (SP2)
// ---------------------------------------------------------------------

#[derive(Default)]
pub struct EnhanceState {
    pub running: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Serialize, Clone)]
pub struct EnhanceProgress {
    pub done: u64,
    pub failed: u64,
    pub skipped: u64,
    pub total: u64,
}

#[derive(Serialize, Clone)]
pub struct EnhanceDone {
    pub done: u64,
    pub failed: u64,
    pub skipped: u64,
    pub total: u64,
    pub failed_names: Vec<String>,
    /// Track ids the worker actually wrote a DB row for (Replace mode only —
    /// NewFile mode makes no DB row, the frontend rescans instead). The
    /// frontend re-analyzes these (bpm/waveform survive, but the audio
    /// samples changed).
    pub enhanced_ids: Vec<i64>,
    /// Libraries touched by at least one successful swap-in, so the frontend
    /// can rescan each once (NewFile's new sibling file, or Replace's
    /// refreshed stamps, both need a rescan to show up/settle).
    pub library_ids: Vec<i64>,
    /// Absolute paths of files created by NewFile mode this run, so the
    /// frontend can reveal the first one in Explorer (empty for Replace).
    pub new_files: Vec<String>,
    /// One entry per skipped track, naming it and WHY it was skipped
    /// ("already at -9 LUFS" / "(Enhanced) file already exists").
    pub skipped_names: Vec<String>,
}

impl EnhanceDone {
    /// Nothing to do: an empty selection, or none of the ids resolved.
    pub fn empty() -> Self {
        Self {
            done: 0, failed: 0, skipped: 0, total: 0,
            failed_names: Vec::new(), enhanced_ids: Vec::new(), library_ids: Vec::new(),
            new_files: Vec::new(), skipped_names: Vec::new(),
        }
    }

    /// The run never started — every queued track counts as failed, under one
    /// shared reason.
    pub fn failure(total: u64, reason: &str) -> Self {
        Self {
            done: 0, failed: total, skipped: 0, total,
            failed_names: vec![reason.to_string()], enhanced_ids: Vec::new(),
            library_ids: Vec::new(), new_files: Vec::new(), skipped_names: Vec::new(),
        }
    }
}

#[tauri::command]
pub fn cancel_enhance(enhance_state: State<EnhanceState>) {
    enhance_state.cancel.store(true, Ordering::SeqCst);
}

/// The `_Serato_/Subcrates` folder for the device holding `root`, if `root`
/// is a driveletter-rooted path (`D:\...`). Same letter-derivation as
/// `repoint_serato_links`.
fn root_serato_dir(root: &Path) -> Option<PathBuf> {
    let s = root.to_string_lossy().to_string();
    let letter = s.get(0..2).filter(|seg| seg.as_bytes()[1] == b':')?;
    Some(PathBuf::from(format!("{letter}\\_Serato_\\Subcrates")))
}

fn device_has_serato_crates(root: &Path) -> bool {
    root_serato_dir(root).map(|d| d.is_dir()).unwrap_or(false)
}

/// True when repointing this library's Serato links right now would be lost:
/// the device has crates AND Serato is currently running (it rewrites every
/// `.crate` from memory on exit). Enhance and ML-restore both check this once
/// up front, but a run can last hours — the workers re-check per track before
/// each repoint, since the answer changes the moment the user opens Serato.
fn serato_repoint_blocked(root: &Path) -> bool {
    device_has_serato_crates(root) && serato_running()
}

fn emit_enhance_progress(app: &AppHandle, done: u64, failed: u64, skipped: u64, total: u64) {
    let _ = app.emit("enhance:progress", EnhanceProgress { done, failed, skipped, total });
}

/// Rewrites the first exact `-map 0` pair in an enhance ffmpeg arg vector to
/// `-map 0:a` (stream-select instead of "everything") — a fallback retry for
/// sources where `-map 0` fails to pick a playable stream (e.g. a video-only
/// "album art" attachment stream some taggers embed as a second stream).
/// Leaves `-map_metadata 0` alone (different flag, exact string compare only
/// matches "-map").
fn retry_with_map_0a(args: &[std::ffi::OsString]) -> Vec<std::ffi::OsString> {
    let mut out = args.to_vec();
    let mut i = 0;
    while i + 1 < out.len() {
        if out[i].as_os_str() == std::ffi::OsStr::new("-map")
            && out[i + 1].as_os_str() == std::ffi::OsStr::new("0")
        {
            out[i + 1] = std::ffi::OsString::from("0:a");
            break;
        }
        i += 1;
    }
    out
}

/// Queues loudness enhancement for `track_ids` on a background worker thread
/// (mirrors `update_tracks_metadata`'s state-swap/own-connection/thread
/// pattern). Returns the queued count immediately; progress/completion ride
/// on `enhance:progress`/`enhance:done`.
///
/// Safety ordering per track, enforced by the worker loop below:
/// `clean_orphan_tmps` runs before ffmpeg is spawned (never concurrently with
/// an in-flight job — one worker thread makes this automatic); in Replace
/// mode `ensure_backup` must succeed before ffmpeg ever writes the tmp file
/// and before `swap_in` touches the destination; any per-track failure
/// deletes that track's tmp file and moves on to the next track.
///
/// Flac-out replaces change the file extension, which requires repointing
/// any Serato `.crate` links afterwards. Because a mid-run refusal would
/// leave some tracks enhanced and others not, that refusal happens up front,
/// here, before the worker is spawned: if any selected track would need
/// repointing on a device that has Serato crates and Serato is currently
/// running, the whole run is refused.
#[tauri::command]
pub fn enhance_tracks(
    track_ids: Vec<i64>,
    mode: enhance::EnhanceMode,
    format: enhance::OutputFormat,
    dest_dir: Option<String>,
    app: AppHandle,
    state: State<AppState>,
    enhance_state: State<EnhanceState>,
    ml_state: State<MlRestoreState>,
    stabilize_state: State<StabilizeState>,
) -> Result<u64, String> {
    // NewFile only: an explicit destination folder must exist up front.
    let dest_dir: Option<std::path::PathBuf> = match dest_dir {
        Some(d) if mode == enhance::EnhanceMode::NewFile => {
            let p = std::path::PathBuf::from(&d);
            if !p.is_dir() {
                return Err(format!("destination folder not found: {d}"));
            }
            Some(p)
        }
        _ => None,
    };
    if track_ids.is_empty() {
        let _ = app.emit(
            "enhance:done",
            EnhanceDone {
                done: 0, failed: 0, skipped: 0, total: 0,
                failed_names: Vec::new(), enhanced_ids: Vec::new(), library_ids: Vec::new(),
                new_files: Vec::new(), skipped_names: Vec::new(),
            },
        );
        return Ok(0);
    }
    if stabilize_state.running.load(Ordering::SeqCst) {
        return Err("a tempo run is active; wait for it to finish".into());
    }
    if ml_state.running.load(Ordering::SeqCst) {
        return Err("a restore run is active; wait for it to finish".into());
    }
    if enhance_state.running.swap(true, Ordering::SeqCst) {
        return Err("an enhance run is already active".into());
    }
    enhance_state.cancel.store(false, Ordering::SeqCst);

    let targets = {
        let conn = match state.conn.lock() {
            Ok(c) => c,
            Err(_) => {
                enhance_state.running.store(false, Ordering::SeqCst);
                return Err(LOCK_ERR.to_string());
            }
        };
        match db::tracks_for_enhance(&conn, &track_ids) {
            Ok(t) => t,
            Err(e) => {
                enhance_state.running.store(false, Ordering::SeqCst);
                return Err(err(e));
            }
        }
    };

    if targets.is_empty() {
        enhance_state.running.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "enhance:done",
            EnhanceDone {
                done: 0, failed: 0, skipped: 0, total: 0,
                failed_names: Vec::new(), enhanced_ids: Vec::new(), library_ids: Vec::new(),
                new_files: Vec::new(), skipped_names: Vec::new(),
            },
        );
        return Ok(0);
    }

    // Up-front Serato refusal (see doc comment above): only Replace + Flac-out
    // can change an extension, and only tracks not already flac need it.
    if mode == enhance::EnhanceMode::Replace && format == enhance::OutputFormat::Flac {
        let needs_repoint = targets.iter().any(|t| !t.format.eq_ignore_ascii_case("flac"));
        if needs_repoint {
            let any_serato_device = targets.iter().any(|t| device_has_serato_crates(&t.root_path));
            if any_serato_device && serato_running() {
                enhance_state.running.store(false, Ordering::SeqCst);
                return Err(
                    "Close Serato first — this run would change file extensions and repoint Serato's crate links.".into(),
                );
            }
        }
    }

    let Some(ffmpeg) = analysis::find_ffmpeg() else {
        enhance_state.running.store(false, Ordering::SeqCst);
        return Err("ffmpeg not found".into());
    };

    let total = targets.len() as u64;
    let db_path = state.db_path.clone();
    let running = enhance_state.running.clone();
    let cancel = enhance_state.cancel.clone();

    std::thread::spawn(move || {
        let conn = match db::open(&db_path) {
            Ok(c) => c,
            Err(_) => {
                running.store(false, Ordering::SeqCst);
                let _ = app.emit(
                    "enhance:done",
                    EnhanceDone {
                        done: 0, failed: total, skipped: 0, total,
                        failed_names: vec!["database".into()],
                        enhanced_ids: Vec::new(), library_ids: Vec::new(),
                        new_files: Vec::new(), skipped_names: Vec::new(),
                    },
                );
                return;
            }
        };

        let (mut done, mut failed, mut skipped) = (0u64, 0u64, 0u64);
        let mut failed_names: Vec<String> = Vec::new();
        let mut enhanced_ids: Vec<i64> = Vec::new();
        let mut new_files: Vec<String> = Vec::new();
        let mut skipped_names: Vec<String> = Vec::new();
        let mut library_ids: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();

        for t in &targets {
            if cancel.load(Ordering::SeqCst) {
                break;
            }

            let src_abs = {
                let mut p = t.root_path.clone();
                for seg in t.rel_path.split('/') {
                    p.push(seg);
                }
                p
            };
            let name = src_abs
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| t.rel_path.clone());

            // (2) clean_orphan_tmps BEFORE spawning ffmpeg for this track —
            // never concurrent with an in-flight job, the single worker
            // thread makes that automatic.
            let dir = src_abs.parent().unwrap_or(&t.root_path).to_path_buf();
            enhance::clean_orphan_tmps(&dir);

            let sample_rate = t.sample_rate.filter(|r| *r > 0).unwrap_or(44_100) as u32;
            let bit_depth = metadata::bit_depth(&src_abs);

            // (3) Measure.
            let measured = analysis::decode_pcm_stereo(&ffmpeg, &src_abs, sample_rate)
                .ok()
                .and_then(|interleaved| {
                    let n = interleaved.len() / 2;
                    let mut l = Vec::with_capacity(n);
                    let mut r = Vec::with_capacity(n);
                    for frame in interleaved.chunks_exact(2) {
                        l.push(frame[0]);
                        r.push(frame[1]);
                    }
                    quality::integrated_lufs(&[&l, &r], sample_rate)
                });
            let Some(lufs) = measured else {
                failed += 1;
                failed_names.push(format!("{name}: could not measure loudness"));
                emit_enhance_progress(&app, done, failed, skipped, total);
                continue;
            };

            // (4) Plan.
            let job = match enhance::plan_enhancement(
                &t.root_path, &t.rel_path, &t.format, bit_depth, lufs, mode, format,
                dest_dir.as_deref(),
            ) {
                enhance::EnhancePlan::AlreadyLoudEnough => {
                    skipped += 1;
                    skipped_names.push(format!(
                        "{name}: already at target ({lufs:.1} LUFS, target {})",
                        enhance::TARGET_LUFS
                    ));
                    emit_enhance_progress(&app, done, failed, skipped, total);
                    continue;
                }
                enhance::EnhancePlan::Unsupported(msg) => {
                    failed += 1;
                    failed_names.push(format!("{name}: {msg}"));
                    emit_enhance_progress(&app, done, failed, skipped, total);
                    continue;
                }
                enhance::EnhancePlan::Job(job) => job,
            };

            // (5) NewFile: never overwrite an existing "(Enhanced)" sibling.
            if mode == enhance::EnhanceMode::NewFile && job.dest_path.exists() {
                skipped += 1;
                skipped_names.push(format!("{name}: (Enhanced) file already exists"));
                emit_enhance_progress(&app, done, failed, skipped, total);
                continue;
            }

            // (6) Replace + extension change (Flac-out on a non-flac source):
            // dest is a DIFFERENT path than the source. If some other,
            // unrelated file already lives there, fail loudly BEFORE
            // touching anything — swap_in would otherwise delete a stranger's
            // file and update_track_file would then collide on
            // UNIQUE(library_id, rel_path).
            if mode == enhance::EnhanceMode::Replace
                && job.new_rel_path.is_some()
                && job.dest_path.exists()
            {
                failed += 1;
                let dest_name = job
                    .dest_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| job.dest_path.display().to_string());
                failed_names.push(format!("{name}: a different file already exists at {dest_name}"));
                emit_enhance_progress(&app, done, failed, skipped, total);
                continue;
            }

            // (7) Replace: ensure_backup MUST succeed before ffmpeg ever
            // writes the tmp file or swap_in touches the destination.
            //
            // Stem-aware first-touch: if this track was already backed up
            // under a different extension (a prior FLAC-out replace), that
            // existing backup counts — creating a second, stem-colliding
            // backup file here would make restore_tracks' own stem search
            // ambiguous and refuse to restore ever again.
            if mode == enhance::EnhanceMode::Replace {
                if let Some(backup) = &job.backup_path {
                    let existing = enhance::existing_backup_for(&t.root_path, &t.rel_path);
                    if existing.is_none() {
                        if let Err(e) = enhance::ensure_backup(&src_abs, backup) {
                            failed += 1;
                            failed_names.push(format!("{name}: backup failed: {e:#}"));
                            emit_enhance_progress(&app, done, failed, skipped, total);
                            continue;
                        }
                    }
                }
            }

            let first = analysis::command_no_window(&ffmpeg)
                .args(&job.ffmpeg_args)
                .stdin(std::process::Stdio::null())
                .output();
            let mut ffmpeg_failure: Option<String> = None;
            match first {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    let original_stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    // Some sources (e.g. a video-only "album art" attachment
                    // stream some taggers embed) make `-map 0` fail to pick a
                    // stream; retry once with `-map 0:a` before giving up.
                    let retry_args = retry_with_map_0a(&job.ffmpeg_args);
                    let retried = analysis::command_no_window(&ffmpeg)
                        .args(&retry_args)
                        .stdin(std::process::Stdio::null())
                        .output();
                    match retried {
                        Ok(r) if r.status.success() => {
                            tracing::warn!(
                                "{name}: ffmpeg -map 0 failed, retry with -map 0:a succeeded ({original_stderr})"
                            );
                        }
                        _ => {
                            ffmpeg_failure = Some(format!("ffmpeg failed: {original_stderr}"));
                        }
                    }
                }
                Err(e) => {
                    ffmpeg_failure = Some(format!("spawning ffmpeg: {e}"));
                }
            }
            if let Some(msg) = ffmpeg_failure {
                // (9) per-track failure: delete the tmp, name the file, continue.
                failed += 1;
                failed_names.push(format!("{name}: {msg}"));
                let _ = std::fs::remove_file(&job.tmp_path);
                emit_enhance_progress(&app, done, failed, skipped, total);
                continue;
            }

            let old_src_to_remove = if mode == enhance::EnhanceMode::Replace && job.new_rel_path.is_some() {
                Some(src_abs.as_path())
            } else {
                None
            };
            // allow_replace: Replace mode intentionally overwrites (its own
            // same-extension file, or a cross-extension dest already
            // confirmed absent above); NewFile must not, closing the TOCTOU
            // race against the `dest_path.exists()` check above.
            let allow_replace = mode == enhance::EnhanceMode::Replace;
            if let Err(e) = enhance::swap_in(&job.tmp_path, &job.dest_path, old_src_to_remove, allow_replace) {
                failed += 1;
                failed_names.push(format!("{name}: {e:#}"));
                let _ = std::fs::remove_file(&job.tmp_path);
                emit_enhance_progress(&app, done, failed, skipped, total);
                continue;
            }
            library_ids.insert(t.library_id);
            if mode == enhance::EnhanceMode::NewFile {
                if let Some(dd) = &dest_dir {
                    // an explicit destination may live in a different library;
                    // rescan that one too so the new file shows up in the app
                    if let Ok(libs) = db::list_libraries(&conn) {
                        let dds = dd.to_string_lossy().to_lowercase();
                        for l in libs {
                            let root = l.root_path.to_lowercase();
                            if dds.starts_with(root.trim_end_matches('\\')) {
                                library_ids.insert(l.id);
                            }
                        }
                    }
                }
            }

            if mode == enhance::EnhanceMode::Replace {
                let fs_meta = match std::fs::metadata(&job.dest_path) {
                    Ok(m) => m,
                    Err(e) => {
                        failed += 1;
                        failed_names.push(format!("{name}: stamping after swap: {e}"));
                        emit_enhance_progress(&app, done, failed, skipped, total);
                        continue;
                    }
                };
                let mtime = fs_meta
                    .modified()
                    .map(scanner::mtime_string)
                    .unwrap_or_default();
                let new_format = if format == enhance::OutputFormat::Flac { Some("flac") } else { None };
                // mark_enhanced runs BEFORE update_track_file: the file is
                // already swapped and backed up at this point, so if the DB
                // update below fails the track is still discoverable via
                // Restore (the enhanced flag doesn't depend on rel_path
                // being current).
                if let Err(e) = db::mark_enhanced(&conn, t.id) {
                    tracing::warn!("marking enhanced for {name}: {e:#}");
                }
                if let Err(e) = db::update_track_file(
                    &conn, t.id, job.new_rel_path.as_deref(), new_format, fs_meta.len() as i64, &mtime,
                ) {
                    failed += 1;
                    failed_names.push(format!("{name}: db update: {e:#}"));
                    emit_enhance_progress(&app, done, failed, skipped, total);
                    continue;
                }
                enhanced_ids.push(t.id);
                if let Some(new_rel) = &job.new_rel_path {
                    // The run-start Serato check can be minutes stale by now;
                    // re-check per track, because writing a .crate while Serato
                    // is open loses the edit when Serato rewrites from memory.
                    if serato_repoint_blocked(&t.root_path) {
                        failed += 1;
                        failed_names.push(format!(
                            "{name}: enhanced, but Serato opened mid-run — close it and use Serato Sync to repoint the crate link"
                        ));
                        emit_enhance_progress(&app, done, failed, skipped, total);
                        continue;
                    }
                    if let Err(e) = repoint_serato_links(&conn, t.library_id, &t.rel_path, new_rel) {
                        // The file swap already happened; this is a softer,
                        // separately-worded failure — the track IS enhanced.
                        failed += 1;
                        failed_names.push(format!("{name}: enhanced but Serato links not updated: {e:#}"));
                        emit_enhance_progress(&app, done, failed, skipped, total);
                        continue;
                    }
                }
                done += 1;
            } else {
                // (8) NewFile: no DB row — the frontend rescans the library.
                new_files.push(job.dest_path.display().to_string());
                done += 1;
            }

            emit_enhance_progress(&app, done, failed, skipped, total);
        }

        running.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "enhance:done",
            EnhanceDone {
                done, failed, skipped, total,
                failed_names,
                enhanced_ids,
                library_ids: library_ids.into_iter().collect(),
                new_files,
                skipped_names,
            },
        );
    });

    Ok(total)
}

// ---------------------------------------------------------------------
// ML restoration (SP3, Apollo)
// ---------------------------------------------------------------------

/// Handle to the in-flight Apollo process, shared between the restore worker
/// and the app-exit hook in lib.rs.
pub type ApolloChild = Arc<std::sync::Mutex<Option<std::process::Child>>>;

/// Per-process counter behind the temp-file names, so an orphan left by a
/// previous app instance (different PID) can never be mistaken for this run's
/// output.
static RESTORE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Default)]
pub struct MlRestoreState {
    pub running: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
    pub child: ApolloChild,
}

impl MlRestoreState {
    /// Kills an in-flight Apollo process. Called from the app-exit hook: an
    /// inference runs for ~30 minutes, so quitting SpinDex without this leaves
    /// a detached Python pegging a core long after the window is gone.
    pub fn kill_child(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Ok(mut slot) = self.child.lock() {
            if let Some(child) = slot.as_mut() {
                let _ = child.kill();
            }
        }
    }
}

#[derive(Serialize, Clone)]
pub struct MlRestoreProgress {
    pub done: u64,
    pub failed: u64,
    pub skipped: u64,
    pub total: u64,
    /// Within-track inference progress (chunks), 0/0 outside inference.
    pub chunk_done: u64,
    pub chunk_total: u64,
    /// File name of the track currently being restored.
    pub current: String,
}

#[tauri::command]
pub fn apollo_ready() -> bool {
    restore::find_apollo_dir()
        .map(|d| restore::apollo_ready(&d))
        .unwrap_or(false)
}

#[tauri::command]
pub fn cancel_ml_restore(ml_state: State<MlRestoreState>) {
    ml_state.cancel.store(true, Ordering::SeqCst);
}

/// Runs Apollo inference on `in_wav` -> `out_wav`. Streams `PROGRESS n/m`
/// stdout lines to `on_chunk`, checks `cancel` about once a second (kills
/// the process on cancel), and kills a stalled process that has printed
/// nothing for `STALL_LIMIT` (model load + one 10s chunk are both minutes on
/// CPU, so the limit is generous). Returns Err with stderr on any failure.
fn run_apollo_inference(
    apollo_dir: &Path,
    in_wav: &Path,
    out_wav: &Path,
    cancel: &Arc<AtomicBool>,
    child_slot: &ApolloChild,
    mut on_chunk: impl FnMut(u64, u64),
) -> anyhow::Result<()> {
    use std::io::{BufRead, BufReader, Read};
    use std::sync::mpsc;
    const STALL_LIMIT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

    let python = apollo_dir.join("venv").join("Scripts").join("python.exe");
    let script = apollo_dir.join("run_restore.py");
    let child = analysis::command_background(&python)
        .arg(&script)
        .arg("--in_wav")
        .arg(in_wav)
        .arg("--out_wav")
        .arg(out_wav)
        .current_dir(apollo_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("spawning Apollo python")?;

    let mut child = child;
    let stdout = child.stdout.take().context("taking stdout")?;
    let mut stderr = child.stderr.take().context("taking stderr")?;
    // Publish the handle so the app-exit hook can kill it: quitting SpinDex
    // mid-restore must not leave a detached Python burning CPU for half an
    // hour. The lock is only ever held for these short operations, never
    // across a blocking wait, so the hook can never dead-lock against it.
    match child_slot.lock() {
        Ok(mut slot) => *slot = Some(child),
        Err(_) => {
            let _ = child.kill();
            anyhow::bail!("restore process slot poisoned");
        }
    }
    let with_child = |f: &mut dyn FnMut(&mut std::process::Child)| {
        if let Ok(mut slot) = child_slot.lock() {
            if let Some(c) = slot.as_mut() {
                f(c);
            }
        }
    };
    let (tx, rx) = mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    // Drain stderr on its own thread so a chatty process can't dead-lock
    // against a full pipe buffer.
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf);
        buf
    });

    let mut last_line = std::time::Instant::now();
    let mut killed: Option<&'static str> = None;
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(line) => {
                last_line = std::time::Instant::now();
                if let Some((n, m)) = restore::parse_progress(&line) {
                    on_chunk(n, m);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if cancel.load(Ordering::SeqCst) {
                    killed = Some("cancelled");
                    with_child(&mut |c| {
                        let _ = c.kill();
                    });
                    break;
                }
                if last_line.elapsed() > STALL_LIMIT {
                    killed = Some("inference stalled (no output for 15 min)");
                    with_child(&mut |c| {
                        let _ = c.kill();
                    });
                    break;
                }
                // still running? a finished process closed the channel below
                let mut exited = false;
                with_child(&mut |c| exited = matches!(c.try_wait(), Ok(Some(_))));
                if exited {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = reader.join();
    // Take the handle out before waiting — `wait` blocks, and the exit hook
    // must never find the mutex held for the length of an inference.
    let taken = child_slot.lock().ok().and_then(|mut slot| slot.take());
    let status = match taken {
        Some(mut c) => c.wait().context("waiting for Apollo python")?,
        None => anyhow::bail!("Apollo process handle lost"),
    };
    let err_out = stderr_thread.join().unwrap_or_default();
    if let Some(why) = killed {
        anyhow::bail!("{why}");
    }
    if !status.success() {
        let tail: String = err_out.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev()
            .collect::<Vec<_>>().join(" | ");
        anyhow::bail!("Apollo failed: {tail}");
    }
    if !out_wav.exists() {
        anyhow::bail!("Apollo produced no output file");
    }
    Ok(())
}

fn emit_ml_progress(
    app: &AppHandle,
    done: u64,
    failed: u64,
    skipped: u64,
    total: u64,
    chunk: (u64, u64),
    current: &str,
) {
    let _ = app.emit(
        "mlrestore:progress",
        MlRestoreProgress {
            done,
            failed,
            skipped,
            total,
            chunk_done: chunk.0,
            chunk_total: chunk.1,
            current: current.to_string(),
        },
    );
}

/// Queues ML restoration for `track_ids` on a background worker thread.
/// Same shape and safety ordering as `enhance_tracks` (see its doc comment):
/// up-front dest-dir validation, single-flight, up-front Serato refusal for
/// Replace+Flac, per-track clean_orphan_tmps -> backup-before-write ->
/// atomic swap -> DB -> Serato repoint; per-track failures never abort the
/// queue. The extra stage: ffmpeg decode to 16-bit 44.1k wav -> Apollo
/// inference (chunk progress, cancel-kill, stall watchdog) -> ffmpeg encode
/// with metadata+artwork mapped from the ORIGINAL file (retry without the
/// artwork mapping for muxers that refuse video streams).
#[tauri::command]
pub fn ml_restore_tracks(
    track_ids: Vec<i64>,
    mode: enhance::EnhanceMode,
    format: enhance::OutputFormat,
    dest_dir: Option<String>,
    app: AppHandle,
    state: State<AppState>,
    ml_state: State<MlRestoreState>,
    enhance_state: State<EnhanceState>,
    stabilize_state: State<StabilizeState>,
) -> Result<u64, String> {
    let Some(apollo_dir) = restore::find_apollo_dir().filter(|d| restore::apollo_ready(d)) else {
        return Err("Apollo is not set up — run scripts/setup-apollo.ps1 first".into());
    };
    let dest_dir: Option<std::path::PathBuf> = match dest_dir {
        Some(d) if mode == enhance::EnhanceMode::NewFile => {
            let p = std::path::PathBuf::from(&d);
            if !p.is_dir() {
                return Err(format!("destination folder not found: {d}"));
            }
            Some(p)
        }
        _ => None,
    };
    let empty_done = || EnhanceDone {
        done: 0, failed: 0, skipped: 0, total: 0,
        failed_names: Vec::new(), enhanced_ids: Vec::new(), library_ids: Vec::new(),
        new_files: Vec::new(), skipped_names: Vec::new(),
    };
    if track_ids.is_empty() {
        let _ = app.emit("mlrestore:done", empty_done());
        return Ok(0);
    }
    if enhance_state.running.load(Ordering::SeqCst) {
        return Err("an enhance run is active; wait for it to finish".into());
    }
    if stabilize_state.running.load(Ordering::SeqCst) {
        return Err("a tempo run is active; wait for it to finish".into());
    }
    if ml_state.running.swap(true, Ordering::SeqCst) {
        return Err("a restore run is already active".into());
    }
    ml_state.cancel.store(false, Ordering::SeqCst);

    let targets = {
        let conn = match state.conn.lock() {
            Ok(c) => c,
            Err(_) => {
                ml_state.running.store(false, Ordering::SeqCst);
                return Err(LOCK_ERR.to_string());
            }
        };
        match db::tracks_for_enhance(&conn, &track_ids) {
            Ok(t) => t,
            Err(e) => {
                ml_state.running.store(false, Ordering::SeqCst);
                return Err(err(e));
            }
        }
    };
    if targets.is_empty() {
        ml_state.running.store(false, Ordering::SeqCst);
        let _ = app.emit("mlrestore:done", empty_done());
        return Ok(0);
    }

    // Up-front Serato refusal — identical rule to enhance_tracks.
    if mode == enhance::EnhanceMode::Replace && format == enhance::OutputFormat::Flac {
        let needs_repoint = targets.iter().any(|t| !t.format.eq_ignore_ascii_case("flac"));
        if needs_repoint {
            let any_serato_device = targets.iter().any(|t| device_has_serato_crates(&t.root_path));
            if any_serato_device && serato_running() {
                ml_state.running.store(false, Ordering::SeqCst);
                return Err(
                    "Close Serato first — this run would change file extensions and repoint Serato's crate links.".into(),
                );
            }
        }
    }

    let Some(ffmpeg) = analysis::find_ffmpeg() else {
        ml_state.running.store(false, Ordering::SeqCst);
        return Err("ffmpeg not found".into());
    };

    let total = targets.len() as u64;
    let db_path = state.db_path.clone();
    let running = ml_state.running.clone();
    let cancel = ml_state.cancel.clone();
    let child = ml_state.child.clone();

    std::thread::spawn(move || {
        let conn = match db::open(&db_path) {
            Ok(c) => c,
            Err(_) => {
                running.store(false, Ordering::SeqCst);
                let _ = app.emit("mlrestore:done", EnhanceDone {
                    done: 0, failed: total, skipped: 0, total,
                    failed_names: vec!["database".into()],
                    enhanced_ids: Vec::new(), library_ids: Vec::new(),
                    new_files: Vec::new(), skipped_names: Vec::new(),
                });
                return;
            }
        };

        let (mut done, mut failed, mut skipped) = (0u64, 0u64, 0u64);
        let mut failed_names: Vec<String> = Vec::new();
        let mut enhanced_ids: Vec<i64> = Vec::new();
        let mut new_files: Vec<String> = Vec::new();
        let mut skipped_names: Vec<String> = Vec::new();
        let mut library_ids: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();

        for t in &targets {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            let src_abs = {
                let mut p = t.root_path.clone();
                for seg in t.rel_path.split('/') {
                    p.push(seg);
                }
                p
            };
            let name = src_abs
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| t.rel_path.clone());
            let dir = src_abs.parent().unwrap_or(&t.root_path).to_path_buf();
            enhance::clean_orphan_tmps(&dir);
            emit_ml_progress(&app, done, failed, skipped, total, (0, 0), &name);

            // Decode to the temp wav Apollo expects (16-bit PCM 44.1k stereo).
            // PID + counter, not the track id: a hard-killed previous run can
            // leave an out-wav behind, and a same-track retry must never pick
            // up that stale file as if it were this run's restoration.
            let token = format!(
                "{}-{}",
                std::process::id(),
                RESTORE_SEQ.fetch_add(1, Ordering::SeqCst)
            );
            let in_wav = std::env::temp_dir().join(format!("spindex-mlrestore-in-{token}.wav"));
            let out_wav = std::env::temp_dir().join(format!("spindex-mlrestore-out-{token}.wav"));
            let cleanup_wavs = |a: &Path, b: &Path| {
                let _ = std::fs::remove_file(a);
                let _ = std::fs::remove_file(b);
            };
            let decode = analysis::command_no_window(&ffmpeg)
                .args(["-v", "error", "-y", "-i"])
                .arg(&src_abs)
                .args(["-ac", "2", "-ar", "44100", "-c:a", "pcm_s16le"])
                .arg(&in_wav)
                .stdin(std::process::Stdio::null())
                .output();
            match decode {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    failed += 1;
                    failed_names.push(format!(
                        "{name}: decode failed: {}",
                        String::from_utf8_lossy(&o.stderr).trim()
                    ));
                    cleanup_wavs(&in_wav, &out_wav);
                    emit_ml_progress(&app, done, failed, skipped, total, (0, 0), &name);
                    continue;
                }
                Err(e) => {
                    failed += 1;
                    failed_names.push(format!("{name}: spawning ffmpeg: {e}"));
                    emit_ml_progress(&app, done, failed, skipped, total, (0, 0), &name);
                    continue;
                }
            }

            // Inference (the ~50 min part). Cancel kills the process.
            let inference = run_apollo_inference(&apollo_dir, &in_wav, &out_wav, &cancel, &child, |n, m| {
                emit_ml_progress(&app, done, failed, skipped, total, (n, m), &name);
            });
            if let Err(e) = inference {
                cleanup_wavs(&in_wav, &out_wav);
                if cancel.load(Ordering::SeqCst) {
                    break; // cancelled: not a failure, just stop
                }
                failed += 1;
                failed_names.push(format!("{name}: {e:#}"));
                emit_ml_progress(&app, done, failed, skipped, total, (0, 0), &name);
                continue;
            }

            // Plan + encode.
            let bit_depth = metadata::bit_depth(&src_abs);
            let job = match restore::plan_restoration(
                &t.root_path, &t.rel_path, &t.format, bit_depth, mode, format,
                dest_dir.as_deref(), &out_wav,
            ) {
                restore::RestorePlan::Unsupported(msg) => {
                    failed += 1;
                    failed_names.push(format!("{name}: {msg}"));
                    cleanup_wavs(&in_wav, &out_wav);
                    emit_ml_progress(&app, done, failed, skipped, total, (0, 0), &name);
                    continue;
                }
                restore::RestorePlan::Job(job) => job,
            };
            if mode == enhance::EnhanceMode::NewFile && job.dest_path.exists() {
                skipped += 1;
                skipped_names.push(format!("{name}: (Restored) file already exists"));
                cleanup_wavs(&in_wav, &out_wav);
                emit_ml_progress(&app, done, failed, skipped, total, (0, 0), &name);
                continue;
            }
            if mode == enhance::EnhanceMode::Replace
                && job.new_rel_path.is_some()
                && job.dest_path.exists()
            {
                failed += 1;
                let dest_name = job.dest_path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| job.dest_path.display().to_string());
                failed_names.push(format!("{name}: a different file already exists at {dest_name}"));
                cleanup_wavs(&in_wav, &out_wav);
                emit_ml_progress(&app, done, failed, skipped, total, (0, 0), &name);
                continue;
            }
            if mode == enhance::EnhanceMode::Replace {
                if let Some(backup) = &job.backup_path {
                    let existing = enhance::existing_backup_for(&t.root_path, &t.rel_path);
                    if existing.is_none() {
                        if let Err(e) = enhance::ensure_backup(&src_abs, backup) {
                            failed += 1;
                            failed_names.push(format!("{name}: backup failed: {e:#}"));
                            cleanup_wavs(&in_wav, &out_wav);
                            emit_ml_progress(&app, done, failed, skipped, total, (0, 0), &name);
                            continue;
                        }
                    }
                }
            }

            let first = analysis::command_no_window(&ffmpeg)
                .args(&job.encode_args)
                .stdin(std::process::Stdio::null())
                .output();
            let mut encode_failure: Option<String> = None;
            match first {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    let original_stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    // wav/aiff muxers refuse video streams; other sources can
                    // trip on the optional artwork map too — retry without it.
                    let retry_args = restore::strip_artwork_maps(&job.encode_args);
                    let retried = analysis::command_no_window(&ffmpeg)
                        .args(&retry_args)
                        .stdin(std::process::Stdio::null())
                        .output();
                    match retried {
                        Ok(r) if r.status.success() => {
                            tracing::warn!("{name}: encode retry without artwork succeeded ({original_stderr})");
                        }
                        _ => encode_failure = Some(format!("encode failed: {original_stderr}")),
                    }
                }
                Err(e) => encode_failure = Some(format!("spawning ffmpeg: {e}")),
            }
            cleanup_wavs(&in_wav, &out_wav);
            if let Some(msg) = encode_failure {
                failed += 1;
                failed_names.push(format!("{name}: {msg}"));
                let _ = std::fs::remove_file(&job.tmp_path);
                emit_ml_progress(&app, done, failed, skipped, total, (0, 0), &name);
                continue;
            }

            // Finalize — identical to enhance_tracks from here on.
            let old_src_to_remove = if mode == enhance::EnhanceMode::Replace && job.new_rel_path.is_some() {
                Some(src_abs.as_path())
            } else {
                None
            };
            let allow_replace = mode == enhance::EnhanceMode::Replace;
            if let Err(e) = enhance::swap_in(&job.tmp_path, &job.dest_path, old_src_to_remove, allow_replace) {
                failed += 1;
                failed_names.push(format!("{name}: {e:#}"));
                let _ = std::fs::remove_file(&job.tmp_path);
                emit_ml_progress(&app, done, failed, skipped, total, (0, 0), &name);
                continue;
            }
            library_ids.insert(t.library_id);
            if mode == enhance::EnhanceMode::NewFile {
                if let Some(dd) = &dest_dir {
                    if let Ok(libs) = db::list_libraries(&conn) {
                        let dds = dd.to_string_lossy().to_lowercase();
                        for l in libs {
                            let root = l.root_path.to_lowercase();
                            if dds.starts_with(root.trim_end_matches('\\')) {
                                library_ids.insert(l.id);
                            }
                        }
                    }
                }
                new_files.push(job.dest_path.display().to_string());
                done += 1;
            } else {
                let fs_meta = match std::fs::metadata(&job.dest_path) {
                    Ok(m) => m,
                    Err(e) => {
                        failed += 1;
                        failed_names.push(format!("{name}: stamping after swap: {e}"));
                        emit_ml_progress(&app, done, failed, skipped, total, (0, 0), &name);
                        continue;
                    }
                };
                let mtime = fs_meta.modified().map(scanner::mtime_string).unwrap_or_default();
                let new_format = if format == enhance::OutputFormat::Flac { Some("flac") } else { None };
                if let Err(e) = db::mark_enhanced(&conn, t.id) {
                    tracing::warn!("marking enhanced for {name}: {e:#}");
                }
                if let Err(e) = db::update_track_file(
                    &conn, t.id, job.new_rel_path.as_deref(), new_format, fs_meta.len() as i64, &mtime,
                ) {
                    failed += 1;
                    failed_names.push(format!("{name}: db update: {e:#}"));
                    emit_ml_progress(&app, done, failed, skipped, total, (0, 0), &name);
                    continue;
                }
                enhanced_ids.push(t.id);
                if let Some(new_rel) = &job.new_rel_path {
                    // A restore run lasts ~30 min per track, so the run-start
                    // Serato check is long stale here; re-check per track or a
                    // Serato opened mid-run silently swallows the repoint.
                    if serato_repoint_blocked(&t.root_path) {
                        failed += 1;
                        failed_names.push(format!(
                            "{name}: restored, but Serato opened mid-run — close it and use Serato Sync to repoint the crate link"
                        ));
                        emit_ml_progress(&app, done, failed, skipped, total, (0, 0), &name);
                        continue;
                    }
                    if let Err(e) = repoint_serato_links(&conn, t.library_id, &t.rel_path, new_rel) {
                        failed += 1;
                        failed_names.push(format!("{name}: restored but Serato links not updated: {e:#}"));
                        emit_ml_progress(&app, done, failed, skipped, total, (0, 0), &name);
                        continue;
                    }
                }
                done += 1;
            }
            emit_ml_progress(&app, done, failed, skipped, total, (0, 0), &name);
        }

        running.store(false, Ordering::SeqCst);
        let _ = app.emit("mlrestore:done", EnhanceDone {
            done, failed, skipped, total,
            failed_names, enhanced_ids,
            library_ids: library_ids.into_iter().collect(),
            new_files, skipped_names,
        });
    });

    Ok(total)
}

// ---------------------------------------------------------------------
// Tempo normalization (warp to a constant grid)
// ---------------------------------------------------------------------

#[derive(Default)]
pub struct StabilizeState {
    pub running: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
}

/// Per-process counter behind the warp scratch directories, so two runs (or an
/// orphan left by a previous instance) can never share a command file.
static STABILIZE_SEQ: AtomicU64 = AtomicU64::new(0);

#[tauri::command]
pub fn cancel_stabilize(stabilize_state: State<StabilizeState>) {
    stabilize_state.cancel.store(true, Ordering::SeqCst);
}

fn emit_stabilize_progress(app: &AppHandle, done: u64, failed: u64, skipped: u64, total: u64) {
    let _ = app.emit("stabilize:progress", EnhanceProgress { done, failed, skipped, total });
}

/// Builds the ffmpeg invocation for one warp. Writes the `asendcmd` script
/// into `work_dir` — ffmpeg runs with that as its working directory, because
/// filter syntax treats `:` and backslash as separators and a Windows absolute
/// path cannot be embedded in a filter argument. Returns the args to pass.
fn warp_ffmpeg_args(
    src: &Path,
    dest_tmp: &Path,
    work_dir: &Path,
    segments: &[tempo::WarpSegment],
    encoder: &[&'static str],
) -> anyhow::Result<Vec<std::ffi::OsString>> {
    use std::ffi::OsString;
    let script = tempo::asendcmd_script(segments);
    let cmd_name = if script.is_empty() { None } else { Some("spindex-warp.txt") };
    if let Some(name) = cmd_name {
        std::fs::write(work_dir.join(name), &script)
            .with_context(|| format!("writing warp script in {}", work_dir.display()))?;
    }
    let first = segments.first().map(|s| s.ratio).unwrap_or(1.0);
    let mut args: Vec<OsString> = vec![
        "-v".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        src.as_os_str().to_owned(),
        "-map".into(),
        "0".into(),
        "-map_metadata".into(),
        "0".into(),
        "-filter:a".into(),
        tempo::filter_arg(first, cmd_name).into(),
    ];
    args.extend(encoder.iter().map(|s| OsString::from(*s)));
    args.push(dest_tmp.as_os_str().to_owned());
    Ok(args)
}

/// Warps each track onto a constant beat grid and writes the result through
/// SP2's output machinery. `target_bpm` None means "keep this track's own
/// dominant tempo" — flatten the drift without changing the groove.
///
/// The beat grid is measured HERE, with the sidecar's `--full-beats` flag,
/// never read from the database: the stored grid is sampled (four islands on
/// a long track), and holding one stretch ratio across a 30 s hole is
/// guessing, not warping.
///
/// Safety ordering per track is the enhance worker's, unchanged:
/// clean_orphan_tmps -> plan -> NewFile dest-exists skip -> cross-extension
/// collision refusal -> backup -> ffmpeg -> swap_in -> DB -> Serato repoint.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn stabilize_tracks(
    track_ids: Vec<i64>,
    target_bpm: Option<f64>,
    mode: enhance::EnhanceMode,
    format: enhance::OutputFormat,
    dest_dir: Option<String>,
    app: AppHandle,
    state: State<AppState>,
    stabilize_state: State<StabilizeState>,
    enhance_state: State<EnhanceState>,
    ml_state: State<MlRestoreState>,
) -> Result<u64, String> {
    let dest_dir: Option<std::path::PathBuf> = match dest_dir {
        Some(d) if mode == enhance::EnhanceMode::NewFile => {
            let p = std::path::PathBuf::from(&d);
            if !p.is_dir() {
                return Err(format!("destination folder not found: {d}"));
            }
            Some(p)
        }
        _ => None,
    };
    if track_ids.is_empty() {
        let _ = app.emit("stabilize:done", EnhanceDone::empty());
        return Ok(0);
    }
    // The three writers share tmp and backup paths, so only one may run.
    if enhance_state.running.load(Ordering::SeqCst) {
        return Err("an enhance run is active; wait for it to finish".into());
    }
    if ml_state.running.load(Ordering::SeqCst) {
        return Err("a restore run is active; wait for it to finish".into());
    }
    if stabilize_state.running.swap(true, Ordering::SeqCst) {
        return Err("a tempo run is already active".into());
    }
    stabilize_state.cancel.store(false, Ordering::SeqCst);

    let targets = {
        let conn = match state.conn.lock() {
            Ok(c) => c,
            Err(_) => {
                stabilize_state.running.store(false, Ordering::SeqCst);
                return Err(LOCK_ERR.to_string());
            }
        };
        match db::tracks_for_enhance(&conn, &track_ids) {
            Ok(t) => t,
            Err(e) => {
                stabilize_state.running.store(false, Ordering::SeqCst);
                return Err(err(e));
            }
        }
    };
    if targets.is_empty() {
        stabilize_state.running.store(false, Ordering::SeqCst);
        let _ = app.emit("stabilize:done", EnhanceDone::empty());
        return Ok(0);
    }

    // Up-front Serato refusal: only Replace + Flac-out changes an extension,
    // and a mid-run refusal would leave the selection half-warped.
    if mode == enhance::EnhanceMode::Replace && format == enhance::OutputFormat::Flac {
        let needs_repoint = targets.iter().any(|t| !t.format.eq_ignore_ascii_case("flac"));
        if needs_repoint
            && targets.iter().any(|t| device_has_serato_crates(&t.root_path))
            && serato_running()
        {
            stabilize_state.running.store(false, Ordering::SeqCst);
            return Err(
                "Close Serato first — this run would change file extensions and repoint Serato's crate links.".into(),
            );
        }
    }

    let Some(sidecars) = analysis::find_sidecars() else {
        stabilize_state.running.store(false, Ordering::SeqCst);
        return Err("the analyzer or its models are missing — tempo needs both".into());
    };

    let total = targets.len() as u64;
    let db_path = state.db_path.clone();
    let running = stabilize_state.running.clone();
    let cancel = stabilize_state.cancel.clone();

    std::thread::spawn(move || {
        let conn = match db::open(&db_path) {
            Ok(c) => c,
            Err(_) => {
                running.store(false, Ordering::SeqCst);
                let _ = app.emit("stabilize:done", EnhanceDone::failure(total, "database"));
                return;
            }
        };

        // One scratch directory per run for the asendcmd command files, so
        // nothing is ever written into the user's music folders.
        let work_dir = std::env::temp_dir().join(format!(
            "spindex-warp-{}-{}",
            std::process::id(),
            STABILIZE_SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        if let Err(e) = std::fs::create_dir_all(&work_dir) {
            running.store(false, Ordering::SeqCst);
            let _ = app.emit(
                "stabilize:done",
                EnhanceDone::failure(total, &format!("temp folder: {e}")),
            );
            return;
        }

        let (mut done, mut failed, mut skipped) = (0u64, 0u64, 0u64);
        let mut failed_names: Vec<String> = Vec::new();
        let mut skipped_names: Vec<String> = Vec::new();
        let mut enhanced_ids: Vec<i64> = Vec::new();
        let mut new_files: Vec<String> = Vec::new();
        let mut library_ids: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();

        for t in &targets {
            if cancel.load(Ordering::SeqCst) {
                break;
            }

            let src_abs = {
                let mut p = t.root_path.clone();
                for seg in t.rel_path.split('/') {
                    p.push(seg);
                }
                p
            };
            let name = src_abs
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| t.rel_path.clone());

            let dir = src_abs.parent().unwrap_or(&t.root_path).to_path_buf();
            enhance::clean_orphan_tmps(&dir);

            // Measure this track's own continuous grid.
            let beats = match analysis::analyze_beats_full(&sidecars, &src_abs) {
                Ok(res) => res.beats.unwrap_or_default(),
                Err(e) => {
                    failed += 1;
                    failed_names.push(format!("{name}: beat detection failed: {e:#}"));
                    emit_stabilize_progress(&app, done, failed, skipped, total);
                    continue;
                }
            };
            if beats.len() < tempo::MIN_BEATS {
                failed += 1;
                failed_names.push(format!("{name}: no usable beat grid detected"));
                emit_stabilize_progress(&app, done, failed, skipped, total);
                continue;
            }
            let target = match target_bpm.or_else(|| tempo::dominant_bpm(&beats)) {
                Some(b) if b > 0.0 => b,
                _ => {
                    failed += 1;
                    failed_names.push(format!("{name}: no usable tempo detected"));
                    emit_stabilize_progress(&app, done, failed, skipped, total);
                    continue;
                }
            };
            let segments = match tempo::plan_warp(&beats, target) {
                tempo::WarpPlan::Segments(s) => s,
                tempo::WarpPlan::NotEnoughBeats => {
                    failed += 1;
                    failed_names.push(format!("{name}: no usable beat grid detected"));
                    emit_stabilize_progress(&app, done, failed, skipped, total);
                    continue;
                }
                tempo::WarpPlan::OutOfRange(r) => {
                    failed += 1;
                    failed_names.push(format!(
                        "{name}: {target:.1} BPM would need a {:.0}% stretch — too far",
                        (r - 1.0) * 100.0
                    ));
                    emit_stabilize_progress(&app, done, failed, skipped, total);
                    continue;
                }
            };
            // Already constant at the requested tempo: nothing to do.
            if segments.len() == 1 && (segments[0].ratio - 1.0).abs() < tempo::SEGMENT_EPS {
                skipped += 1;
                skipped_names.push(format!("{name}: already steady at {target:.1} BPM"));
                emit_stabilize_progress(&app, done, failed, skipped, total);
                continue;
            }

            let bit_depth = metadata::bit_depth(&src_abs);
            let Some(paths) = enhance::plan_paths(
                &t.root_path,
                &t.rel_path,
                &t.format,
                bit_depth,
                mode,
                format,
                dest_dir.as_deref(),
                enhance::STEADY_SUFFIX,
            ) else {
                failed += 1;
                failed_names.push(format!("{name}: unsupported format: {}", t.format));
                emit_stabilize_progress(&app, done, failed, skipped, total);
                continue;
            };

            if mode == enhance::EnhanceMode::NewFile && paths.dest_path.exists() {
                skipped += 1;
                skipped_names.push(format!("{name}: (Steady) file already exists"));
                emit_stabilize_progress(&app, done, failed, skipped, total);
                continue;
            }
            // Replace + extension change: refuse if an unrelated file already
            // occupies the destination (swap_in would delete a stranger's file).
            if mode == enhance::EnhanceMode::Replace
                && paths.new_rel_path.is_some()
                && paths.dest_path.exists()
            {
                failed += 1;
                let dest_name = paths
                    .dest_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| paths.dest_path.display().to_string());
                failed_names.push(format!("{name}: a different file already exists at {dest_name}"));
                emit_stabilize_progress(&app, done, failed, skipped, total);
                continue;
            }
            if mode == enhance::EnhanceMode::Replace {
                if let Some(backup) = &paths.backup_path {
                    if enhance::existing_backup_for(&t.root_path, &t.rel_path).is_none() {
                        if let Err(e) = enhance::ensure_backup(&src_abs, backup) {
                            failed += 1;
                            failed_names.push(format!("{name}: backup failed: {e:#}"));
                            emit_stabilize_progress(&app, done, failed, skipped, total);
                            continue;
                        }
                    }
                }
            }

            let args = match warp_ffmpeg_args(
                &src_abs,
                &paths.tmp_path,
                &work_dir,
                &segments,
                &paths.encoder,
            ) {
                Ok(a) => a,
                Err(e) => {
                    failed += 1;
                    failed_names.push(format!("{name}: {e:#}"));
                    emit_stabilize_progress(&app, done, failed, skipped, total);
                    continue;
                }
            };
            let run_ffmpeg = |a: &[std::ffi::OsString]| {
                analysis::command_no_window(&sidecars.ffmpeg)
                    .args(a)
                    .current_dir(&work_dir)
                    .stdin(std::process::Stdio::null())
                    .output()
            };
            let mut ffmpeg_failure: Option<String> = None;
            match run_ffmpeg(&args) {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    let original_stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    // Same `-map 0` fallback the enhance worker uses.
                    match run_ffmpeg(&retry_with_map_0a(&args)) {
                        Ok(r) if r.status.success() => {
                            tracing::warn!(
                                "{name}: ffmpeg -map 0 failed, retry with -map 0:a succeeded ({original_stderr})"
                            );
                        }
                        _ => ffmpeg_failure = Some(format!("ffmpeg failed: {original_stderr}")),
                    }
                }
                Err(e) => ffmpeg_failure = Some(format!("spawning ffmpeg: {e}")),
            }
            if let Some(msg) = ffmpeg_failure {
                failed += 1;
                failed_names.push(format!("{name}: {msg}"));
                let _ = std::fs::remove_file(&paths.tmp_path);
                emit_stabilize_progress(&app, done, failed, skipped, total);
                continue;
            }

            let old_src_to_remove =
                if mode == enhance::EnhanceMode::Replace && paths.new_rel_path.is_some() {
                    Some(src_abs.as_path())
                } else {
                    None
                };
            let allow_replace = mode == enhance::EnhanceMode::Replace;
            if let Err(e) = enhance::swap_in(
                &paths.tmp_path,
                &paths.dest_path,
                old_src_to_remove,
                allow_replace,
            ) {
                failed += 1;
                failed_names.push(format!("{name}: {e:#}"));
                let _ = std::fs::remove_file(&paths.tmp_path);
                emit_stabilize_progress(&app, done, failed, skipped, total);
                continue;
            }
            library_ids.insert(t.library_id);
            if mode == enhance::EnhanceMode::NewFile {
                if let Some(dd) = &dest_dir {
                    // an explicit destination may live in a different library;
                    // rescan that one too so the new file shows up in the app
                    if let Ok(libs) = db::list_libraries(&conn) {
                        let dds = dd.to_string_lossy().to_lowercase();
                        for l in libs {
                            let root = l.root_path.to_lowercase();
                            if dds.starts_with(root.trim_end_matches('\\')) {
                                library_ids.insert(l.id);
                            }
                        }
                    }
                }
            }

            // The audio is now at a constant `target`; make the tag agree. A
            // tag failure is a warning, never a per-track failure — the audio
            // is already correct, only its tag lags.
            let bpm_tag = metadata::TagUpdate {
                title: None,
                artist: None,
                album: None,
                genre: None,
                year: None,
                bpm: Some(target),
                key: None,
                comment: None,
                label: None,
                rating: None,
            };
            if let Err(e) = metadata::apply_partial(&paths.dest_path, &bpm_tag, &["bpm".to_string()])
            {
                tracing::warn!("{name}: stabilized but BPM tag not updated: {e:#}");
            }

            if mode == enhance::EnhanceMode::Replace {
                let fs_meta = match std::fs::metadata(&paths.dest_path) {
                    Ok(m) => m,
                    Err(e) => {
                        failed += 1;
                        failed_names.push(format!("{name}: stamping after swap: {e}"));
                        emit_stabilize_progress(&app, done, failed, skipped, total);
                        continue;
                    }
                };
                let mtime = fs_meta.modified().map(scanner::mtime_string).unwrap_or_default();
                let new_format =
                    if format == enhance::OutputFormat::Flac { Some("flac") } else { None };
                if let Err(e) = db::mark_enhanced(&conn, t.id) {
                    tracing::warn!("marking enhanced for {name}: {e:#}");
                }
                if let Err(e) = db::update_track_file(
                    &conn,
                    t.id,
                    paths.new_rel_path.as_deref(),
                    new_format,
                    fs_meta.len() as i64,
                    &mtime,
                ) {
                    failed += 1;
                    failed_names.push(format!("{name}: db update: {e:#}"));
                    emit_stabilize_progress(&app, done, failed, skipped, total);
                    continue;
                }
                enhanced_ids.push(t.id);
                if let Some(new_rel) = &paths.new_rel_path {
                    // A warp run can last a long time, so the run-start Serato
                    // check goes stale; re-check immediately before writing.
                    if serato_repoint_blocked(&t.root_path) {
                        failed += 1;
                        failed_names.push(format!(
                            "{name}: stabilized, but Serato opened mid-run — close it and use Serato Sync to repoint the crate link"
                        ));
                        emit_stabilize_progress(&app, done, failed, skipped, total);
                        continue;
                    }
                    if let Err(e) = repoint_serato_links(&conn, t.library_id, &t.rel_path, new_rel) {
                        failed += 1;
                        failed_names
                            .push(format!("{name}: stabilized but Serato links not updated: {e:#}"));
                        emit_stabilize_progress(&app, done, failed, skipped, total);
                        continue;
                    }
                }
                done += 1;
            } else {
                new_files.push(paths.dest_path.display().to_string());
                done += 1;
            }

            emit_stabilize_progress(&app, done, failed, skipped, total);
        }

        let _ = std::fs::remove_dir_all(&work_dir);
        running.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "stabilize:done",
            EnhanceDone {
                done,
                failed,
                skipped,
                total,
                failed_names,
                enhanced_ids,
                library_ids: library_ids.into_iter().collect(),
                new_files,
                skipped_names,
            },
        );
    });

    Ok(total)
}

/// Restores tracks to their pre-enhancement original. Runs synchronously
/// (plain file copies, no ffmpeg) and refuses while an enhance run is active
/// so it never races the worker's file ops.
///
/// The original rel_path/format was never stored anywhere — a Flac-out
/// replace rewrites the DB row's rel_path/format in place. Instead this
/// looks in the mirrored `_SpinDexBackups_/<same subfolder>` for a file whose
/// stem matches the current file's stem; exactly one candidate is expected
/// (the current file plus its one first-touch backup share a stem, differing
/// only in extension when Flac-out changed it). Zero or multiple candidates
/// is reported back as a named failure rather than guessing which backup is
/// the right one.
///
/// Returns the names of any tracks that failed to restore (empty = all ok).
#[tauri::command]
pub fn restore_tracks(
    track_ids: Vec<i64>,
    state: State<AppState>,
    enhance_state: State<EnhanceState>,
    ml_state: State<MlRestoreState>,
    stabilize_state: State<StabilizeState>,
) -> Vec<String> {
    if enhance_state.running.load(Ordering::SeqCst) {
        return vec!["an enhance run is active; try again once it finishes".to_string()];
    }
    if ml_state.running.load(Ordering::SeqCst) {
        return vec!["a restore run is active; try again once it finishes".to_string()];
    }
    if stabilize_state.running.load(Ordering::SeqCst) {
        return vec!["a tempo run is active; try again once it finishes".to_string()];
    }
    let conn = match state.conn.lock() {
        Ok(c) => c,
        Err(_) => return vec![LOCK_ERR.to_string()],
    };

    struct Plan {
        id: i64,
        library_id: i64,
        root: PathBuf,
        current_rel: String,
        current_abs: PathBuf,
        backup: PathBuf,
        original_rel: String,
        original_abs: PathBuf,
        ext_changed: bool,
        name: String,
    }

    let mut plans: Vec<Plan> = Vec::new();
    let mut failed: Vec<String> = Vec::new();

    for id in &track_ids {
        let row: Result<(i64, String, String), _> = conn.query_row(
            "SELECT t.library_id, l.root_path, t.rel_path FROM track t
             JOIN library l ON l.id = t.library_id WHERE t.id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        );
        let (library_id, root, rel) = match row {
            Ok(v) => v,
            Err(_) => {
                failed.push(format!("track {id}: not found"));
                continue;
            }
        };
        let root = PathBuf::from(root);
        let current_abs = {
            let mut p = root.clone();
            for seg in rel.split('/') {
                p.push(seg);
            }
            p
        };
        let name = current_abs
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| rel.clone());

        let (subfolder, _) = rel
            .rsplit_once('/')
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .unwrap_or_default();
        let mut candidates: Vec<PathBuf> = enhance::backup_candidates_for(&root, &rel);
        match candidates.len() {
            0 => {
                failed.push(format!("{name}: no backup found"));
                continue;
            }
            1 => {}
            n => {
                failed.push(format!("{name}: ambiguous backup ({n} candidates)"));
                continue;
            }
        }
        let backup = candidates.remove(0);
        let orig_filename = backup
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let original_rel = if subfolder.is_empty() {
            orig_filename.clone()
        } else {
            format!("{subfolder}/{orig_filename}")
        };
        let original_abs = {
            let mut p = root.clone();
            for seg in original_rel.split('/') {
                p.push(seg);
            }
            p
        };
        let ext_changed = original_rel != rel;
        // Refuse rather than overwrite a genuinely different live sibling:
        // if restoring implies a filename that currently exists on disk and
        // isn't this track's own current file, the discovered backup is a
        // stem collision with another track, not a prior backup of this one.
        if ext_changed && original_abs.exists() && original_abs != current_abs {
            let overwrite_name = original_abs
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| original_rel.clone());
            failed.push(format!("{name}: restore would overwrite {overwrite_name}"));
            continue;
        }
        plans.push(Plan {
            id: *id, library_id, root, current_rel: rel, current_abs,
            backup, original_rel, original_abs, ext_changed, name,
        });
    }

    // Up-front Serato refusal, mirroring enhance_tracks: refuse the whole
    // call before touching any file if a restore would repoint a Serato
    // device's crate links while Serato is running.
    if plans.iter().any(|p| p.ext_changed) {
        let any_serato_device = plans
            .iter()
            .any(|p| p.ext_changed && device_has_serato_crates(&p.root));
        if any_serato_device && serato_running() {
            return vec![
                "Close Serato first — restoring would repoint Serato's crate links.".to_string(),
            ];
        }
    }

    for p in plans {
        if let Err(e) = enhance::restore_from_backup(&p.backup, &p.original_abs, &p.current_abs) {
            failed.push(format!("{}: {e:#}", p.name));
            continue;
        }
        let fs_meta = match std::fs::metadata(&p.original_abs) {
            Ok(m) => m,
            Err(e) => {
                failed.push(format!("{}: stamping after restore: {e}", p.name));
                continue;
            }
        };
        let mtime = fs_meta
            .modified()
            .map(scanner::mtime_string)
            .unwrap_or_default();
        let new_format = p
            .original_abs
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        if let Err(e) = db::update_track_file(
            &conn, p.id, Some(&p.original_rel), new_format.as_deref(), fs_meta.len() as i64, &mtime,
        ) {
            failed.push(format!("{}: db update: {e:#}", p.name));
            continue;
        }
        if let Err(e) = db::clear_enhanced(&conn, p.id) {
            tracing::warn!("clearing enhanced flag for {}: {e:#}", p.name);
        }
        if p.ext_changed {
            if let Err(e) = repoint_serato_links(&conn, p.library_id, &p.current_rel, &p.original_rel) {
                failed.push(format!("{}: restored but Serato links not updated: {e:#}", p.name));
            }
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_serato_process_matches_image_names_case_insensitively() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(any_serato_process(names(&["explorer.exe", "Serato DJ Pro.exe"])));
        assert!(any_serato_process(names(&["SERATO DJ LITE.EXE"])));
        assert!(!any_serato_process(names(&["explorer.exe", "spindex.exe"])));
        assert!(!any_serato_process(Vec::new()));
    }

    /// The snapshot must actually see processes — an always-empty list would
    /// silently turn the "close Serato first" guard off. The test runner
    /// itself is always in the list.
    #[cfg(windows)]
    #[test]
    fn process_names_sees_the_running_test_binary() {
        let me = std::env::current_exe().unwrap();
        let me = me.file_name().unwrap().to_string_lossy().to_lowercase();
        let names = process_names();
        assert!(names.len() > 5, "snapshot returned {} processes", names.len());
        assert!(names.iter().any(|n| n.to_lowercase() == me), "{me} not in snapshot");
    }

    #[test]
    fn track_set_ref_json_matches_the_frontend() {
        let s = TrackSetRef::Serato { letter: "D:".into(), file: "A%%B.crate".into() };
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            r#"{"kind":"serato","letter":"D:","file":"A%%B.crate"}"#
        );
        let r: TrackSetRef =
            serde_json::from_str(r#"{"kind":"rekordbox","letter":"D:","playlist_id":3}"#).unwrap();
        assert_eq!(r, TrackSetRef::Rekordbox { letter: "D:".into(), playlist_id: 3 });
        let f: TrackSetRef =
            serde_json::from_str(r#"{"kind":"folder","library_id":1,"path":"House"}"#).unwrap();
        assert_eq!(f, TrackSetRef::Folder { library_id: 1, path: "House".into() });
    }

    fn seed_sets_db(name: &str) -> rusqlite::Connection {
        let dir = std::env::temp_dir().join(format!("spindex-sets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        let _ = std::fs::remove_file(&p);
        let conn = db::open(&p).unwrap();
        // "Z:" never exists, so no Serato/Rekordbox data is found for it
        conn.execute("INSERT INTO library (id, name, root_path) VALUES (1, 'Lib', 'Z:\\Lib')", [])
            .unwrap();
        for (id, rel) in [(1, "House/a.mp3"), (2, "House/b.mp3"), (3, "c.mp3")] {
            conn.execute(
                "INSERT INTO track (id, library_id, rel_path, file_size, modified_at)
                 VALUES (?1, 1, ?2, 1, '0')",
                rusqlite::params![id, rel],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn list_track_sets_without_devices_lists_libraries_and_folders() {
        let conn = seed_sets_db("list.db");
        let sets = track_sets(&conn).unwrap();
        assert!(sets.iter().any(|s| s.set == TrackSetRef::Library { id: 1 } && s.track_count == 3));
        assert!(sets.iter().any(|s| s.set
            == TrackSetRef::Folder { library_id: 1, path: "House".into() }
            && s.track_count == 2));
    }

    #[test]
    fn folder_set_resolves_to_its_direct_tracks() {
        let conn = seed_sets_db("folder.db");
        let mut ids =
            resolve_set(&conn, &TrackSetRef::Folder { library_id: 1, path: "House".into() }).unwrap();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn compare_candidates_exclude_the_source() {
        let conn = seed_sets_db("candidates.db");
        let src = TrackSetRef::Folder { library_id: 1, path: "House".into() };
        let rows = compare_set_rows(&conn, &src).unwrap();
        assert!(rows.iter().all(|r| r.target != src));
        assert!(rows.iter().any(|r| r.target == TrackSetRef::Library { id: 1 }));
    }

    #[test]
    fn running_flag_is_released_even_when_the_worker_panics() {
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        let _ = std::thread::spawn(move || {
            let _guard = RunningGuard(r);
            panic!("worker died");
        })
        .join();
        assert!(!running.load(Ordering::SeqCst));
    }

    #[test]
    fn export_path_is_on_the_stick() {
        assert_eq!(export_path("D:"), PathBuf::from("D:\\SpinDex\\rekordbox.xml"));
    }

    #[test]
    fn prepared_playlists_resolve_to_the_sticks_tracks() {
        use crate::core::djlib::model::{DjLibrary, DjPlaylistNode, DjTrack};
        let conn = seed_sets_db("prepared.db"); // library 'Z:\Lib' with House/a.mp3, House/b.mp3, c.mp3
        let track = |rel: &str| DjTrack {
            abs_path: PathBuf::from(format!("Z:\\Lib\\{}", rel.replace('/', "\\"))),
            ..Default::default()
        };
        let lib = DjLibrary {
            tracks: vec![track("House/b.mp3"), track("c.mp3"), track("gone.mp3")],
            playlists: vec![DjPlaylistNode::Folder {
                name: "Sets".into(),
                children: vec![DjPlaylistNode::Playlist { name: "Warmup".into(), track_refs: vec![0, 1, 2] }],
            }],
        };
        let xml = crate::core::rekordbox_xml::write_xml(&lib, "test");
        let nodes = prepared_crates(&conn, "Z:", &xml).unwrap();
        assert_eq!(nodes.len(), 2);
        assert!(nodes[0].is_folder && nodes[0].name == "Sets");
        let warm = &nodes[1];
        assert_eq!((warm.parent_id, warm.name.as_str()), (nodes[0].id, "Warmup"));
        assert_eq!(warm.track_ids, vec![2, 3], "in playlist order");
        assert_eq!(warm.missing, 1, "gone.mp3 is not indexed");
    }

    /// The path (not the whole argument) must be quoted, or Explorer opens
    /// Documents instead of the target folder on any path with spaces.
    #[test]
    fn explorer_select_arg_quotes_the_path_itself() {
        assert_eq!(
            explorer_select_arg(r"D:\MP3\Track (Enhanced).flac"),
            r#"/select,"D:\MP3\Track (Enhanced).flac""#
        );
        assert_eq!(explorer_select_arg(r"C:\a.mp3"), r#"/select,"C:\a.mp3""#);
    }

    /// Each aiff preview gets its own file: ffmpeg truncates its output on
    /// open, and the deck is still streaming the previous preview while the
    /// next one transcodes, so one shared file broke the old track mid-play.
    #[test]
    fn preview_paths_are_unique_per_call() {
        let dir = Path::new("cache");
        let a = preview_wav_path(dir, 1);
        let b = preview_wav_path(dir, 2);
        assert_ne!(a, b);
        assert!(a.starts_with(dir) && b.starts_with(dir));
        assert!(is_preview_wav(&a) && is_preview_wav(&b));
        assert!(!is_preview_wav(Path::new("cache/spindex-art-7.jpg")));
    }

    #[test]
    fn prune_previews_keeps_only_the_given_previews() {
        let dir = std::env::temp_dir().join(format!("spindex-prune-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = preview_wav_path(&dir, 1);
        let prev = preview_wav_path(&dir, 2);
        let cur = preview_wav_path(&dir, 3);
        let art = dir.join("spindex-art-7.jpg");
        for p in [&old, &prev, &cur, &art] {
            std::fs::write(p, b"x").unwrap();
        }
        prune_previews(&dir, &[cur.clone(), prev.clone()]);
        assert!(!old.exists(), "an older preview is removed");
        assert!(prev.exists() && cur.exists(), "current and previous stay");
        assert!(art.exists(), "artwork is never touched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A later-issued `prepare_playback` call always wins, even if it
    /// finishes first; an earlier-issued call that finishes AFTER the one it
    /// was superseded by must never be allowed to revoke the file the
    /// winning call left readable.
    #[test]
    fn play_seq_wins_only_for_the_most_recently_issued_call() {
        // Normal case: calls apply in the order they were issued.
        assert!(play_seq_wins(1, 0));
        assert!(play_seq_wins(2, 1));

        // Out-of-order completion: an older call (seq 1, e.g. a slow aiff
        // transcode) finishing after a newer one (seq 2) already applied
        // must not win and must not be allowed to forbid seq 2's file.
        assert!(!play_seq_wins(1, 2));

        // A call finishing exactly when it was issued relative to the
        // latest applied one (itself) still applies — this is the first
        // call in the session, or a repeat of the same track.
        assert!(play_seq_wins(1, 1));
    }

    fn device(letter: &str, serial: Option<&str>) -> crate::core::devices::Device {
        crate::core::devices::Device {
            letter: letter.to_string(),
            label: "KINGSTON".to_string(),
            serial: serial.map(str::to_string),
        }
    }

    /// A stick already indexed once, now unplugged: its tracks are flagged
    /// missing and it was last seen on D:.
    fn arrival_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::core::db::SCHEMA).unwrap();
        crate::core::db::migrate(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO library (id, name, root_path, is_removable, volume_serial) VALUES
                 (1, 'MP3 (KINGSTON)', 'D:\\MP3\\', 1, 'AABBCCDD');
             INSERT INTO track (id, library_id, rel_path, file_size, modified_at, is_missing)
                 VALUES (10, 1, 'a.mp3', 1, '0', 1);",
        )
        .unwrap();
        conn
    }

    /// The payload is the whole point: without it the frontend never learns a
    /// stick appeared, so the sidebar stays stale and the known libraries are
    /// never rescanned. It also carries the reconnect side effects.
    #[test]
    fn an_arriving_known_stick_reports_its_libraries_and_follows_the_new_letter() {
        let conn = arrival_db();
        let payload = reconcile_arrival(&conn, &device("E:", Some("AABBCCDD")));

        assert_eq!(payload.letter, "E:");
        assert_eq!(payload.label, "KINGSTON");
        assert_eq!(
            payload.known_library_ids,
            vec![1],
            "the frontend rescans exactly these"
        );
        assert_eq!(
            crate::core::db::library_root(&conn, 1).unwrap(),
            "E:\\MP3\\",
            "the library follows its stick to the new drive letter"
        );
        let missing: i64 = conn
            .query_row("SELECT is_missing FROM track WHERE id = 10", [], |r| r.get(0))
            .unwrap();
        assert_eq!(missing, 0, "reconnected tracks are no longer missing");
    }

    /// An unknown stick still has to be announced — that event is what makes
    /// it show up under Devices, ready to be indexed.
    #[test]
    fn an_arriving_unknown_stick_is_still_announced_with_no_libraries() {
        let conn = arrival_db();
        let payload = reconcile_arrival(&conn, &device("F:", Some("11223344")));
        assert_eq!(payload.letter, "F:");
        assert!(payload.known_library_ids.is_empty());
    }


    /// The whole point of `resolve_crate_entries`: what the table shows and
    /// what the writer permutes must be the SAME sequence, or a drop index
    /// computed on screen addresses a different slot in the file.
    #[test]
    fn viewer_and_writer_index_the_same_sequence() {
        use crate::core::serato;
        // a temp folder standing in for a drive letter: serato_crate_path and
        // stick_libraries only ever build/compare "<letter>\..." strings
        let letter = std::env::temp_dir()
            .join(format!("spindex-serato-order-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let subcrates = std::path::PathBuf::from(format!("{letter}\\_Serato_\\Subcrates"));
        std::fs::create_dir_all(&subcrates).unwrap();

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::core::db::SCHEMA).unwrap();
        crate::core::db::migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO library (id, name, root_path) VALUES (2, 'mp3', ?1)",
            params![format!("{letter}\\MP3\\")],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO track (id, library_id, rel_path, file_size, modified_at) VALUES
                 (10, 2, 'a.mp3', 1, '0'),
                 (20, 2, 'b.mp3', 1, '0'),
                 (30, 2, 'c.mp3', 1, '0');",
        )
        .unwrap();

        // header + a, a dangling link, b, a SECOND link to a, c
        let chunks = vec![
            serato::Chunk { tag: *b"vrsn", payload: vec![0, 1] },
            serato::make_otrk("MP3/a.mp3"),
            serato::make_otrk("MP3/gone.mp3"),
            serato::make_otrk("MP3/b.mp3"),
            serato::make_otrk("MP3/a.mp3"),
            serato::make_otrk("MP3/c.mp3"),
        ];
        let bytes = serato::write_chunks(&chunks);
        let file = "Test.crate";
        std::fs::write(subcrates.join(file), &bytes).unwrap();

        // the viewer's path: parse_crate -> ids + missing count
        let (_, viewer_ids, missing) = resolve_crate_entries(
            &conn,
            &letter,
            serato::parse_crate(&bytes).into_iter().map(Some),
        );
        // the writer's path: read the file off disk exactly as the commands do
        let st = serato_crate_state(&conn, &letter, file).unwrap();

        assert_eq!(viewer_ids, vec![10, 20, 30], "duplicate collapses to its first entry");
        assert_eq!(missing, 1, "only the unresolvable link counts as missing");
        assert_eq!(st.order, viewer_ids, "screen and file must index one sequence");
        // the writer's per-chunk resolution is that same sequence, so the
        // duplicate entry sits out the reorder like a dangling link
        assert_eq!(
            st.resolved,
            vec![None, Some(10), None, Some(20), None, Some(30)]
        );
        assert_eq!(st.src_len, bytes.len());

        let _ = std::fs::remove_dir_all(&letter);
    }

    /// A track whose file is already gone (e.g. deleted outside SpinDex, or an
    /// enhance/stabilize leftover cleaned up by hand) but whose library root is
    /// still reachable on disk: this is the case that used to make "Delete"
    /// silently do nothing (`delete_tracks` filtered it out via `p.is_file()`
    /// before ever looking at the row). It must be recognized as confirmed-gone
    /// so the DB row can be dropped without a Recycle Bin call.
    #[test]
    fn classify_delete_targets_treats_a_missing_file_in_a_reachable_library_as_confirmed_gone() {
        let dir = std::env::temp_dir()
            .join(format!("spindex-delete-classify-{}-a", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::core::db::SCHEMA).unwrap();
        crate::core::db::migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO library (id, name, root_path) VALUES (1, 'FLAC', ?1)",
            params![dir.to_string_lossy().to_string() + "\\"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO track (id, library_id, rel_path, file_size, modified_at) \
             VALUES (10, 1, 'gone.flac', 1, '0')",
            [],
        )
        .unwrap();

        let result = classify_delete_targets(&conn, &[10]);

        assert!(result.existing.is_empty());
        assert_eq!(result.confirmed_gone, vec![10]);
        assert!(result.unreachable.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The opposite case: the library's root itself isn't reachable right now
    /// (e.g. a USB stick that's unplugged). The track must NOT be treated as
    /// confirmed-gone here — doing so would purge rows for a device that is
    /// merely disconnected, breaking the existing reconnect/rescan bookkeeping.
    #[test]
    fn classify_delete_targets_leaves_a_track_alone_when_its_library_root_is_unreachable() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::core::db::SCHEMA).unwrap();
        crate::core::db::migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO library (id, name, root_path, is_removable) VALUES \
             (1, 'MP3 (KINGSTON)', 'Z:\\definitely-not-mounted\\', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO track (id, library_id, rel_path, file_size, modified_at) \
             VALUES (10, 1, 'a.mp3', 1, '0')",
            [],
        )
        .unwrap();

        let result = classify_delete_targets(&conn, &[10]);

        assert!(result.existing.is_empty());
        assert!(result.confirmed_gone.is_empty());
        assert_eq!(result.unreachable.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![10]);
    }

    /// A track whose file is still actually present keeps going through the
    /// normal Recycle Bin path, unaffected by the new classification.
    #[test]
    fn classify_delete_targets_treats_an_existing_file_as_a_normal_target() {
        let dir = std::env::temp_dir()
            .join(format!("spindex-delete-classify-{}-b", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("here.flac");
        std::fs::write(&file, b"data").unwrap();

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::core::db::SCHEMA).unwrap();
        crate::core::db::migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO library (id, name, root_path) VALUES (1, 'FLAC', ?1)",
            params![dir.to_string_lossy().to_string() + "\\"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO track (id, library_id, rel_path, file_size, modified_at) \
             VALUES (10, 1, 'here.flac', 1, '0')",
            [],
        )
        .unwrap();

        let result = classify_delete_targets(&conn, &[10]);

        assert_eq!(result.existing, vec![(10, file)]);
        assert!(result.confirmed_gone.is_empty());
        assert!(result.unreachable.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The wiring `delete_tracks` relies on: deleting a track's row also
    /// prunes its name from the folder-crate order file it was listed in, so
    /// `.spindex-order.json` doesn't keep naming files that no longer exist.
    #[test]
    fn delete_rows_and_prune_order_removes_the_row_and_the_stale_order_entry() {
        let dir = std::env::temp_dir()
            .join(format!("spindex-delete-prune-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("House")).unwrap();
        crate::core::crate_order::write_order(
            &dir.join("House"),
            &["a.flac".to_string(), "b.flac".to_string()],
        )
        .unwrap();

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::core::db::SCHEMA).unwrap();
        crate::core::db::migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO library (id, name, root_path) VALUES (1, 'FLAC', ?1)",
            params![dir.to_string_lossy().to_string() + "\\"],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO track (id, library_id, rel_path, file_size, modified_at) VALUES
                 (10, 1, 'House/a.flac', 1, '0'),
                 (20, 1, 'House/b.flac', 1, '0');",
        )
        .unwrap();

        delete_rows_and_prune_order(&conn, &[10]).unwrap();

        let remaining: i64 =
            conn.query_row("SELECT COUNT(*) FROM track WHERE id = 10", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining, 0, "the row must be gone");
        assert_eq!(
            crate::core::crate_order::read_order(&dir.join("House")).unwrap(),
            vec!["b.flac"],
            "the deleted track's name must be pruned from the order file",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
