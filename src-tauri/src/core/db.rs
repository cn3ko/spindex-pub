use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::core::metadata::TrackMeta;

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS library (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    root_path TEXT NOT NULL UNIQUE,
    is_removable INTEGER NOT NULL DEFAULT 0,
    last_scanned_at TEXT
);

CREATE TABLE IF NOT EXISTS track (
    id INTEGER PRIMARY KEY,
    library_id INTEGER NOT NULL REFERENCES library(id) ON DELETE CASCADE,
    rel_path TEXT NOT NULL,
    file_size INTEGER NOT NULL,
    modified_at TEXT NOT NULL,
    sha256 TEXT,
    title TEXT, artist TEXT, album TEXT, genre TEXT, label TEXT,
    year INTEGER, bpm REAL, key_camelot TEXT,
    rating INTEGER, comment TEXT,
    duration_secs REAL, bitrate_kbps INTEGER, sample_rate INTEGER,
    format TEXT,
    has_artwork INTEGER NOT NULL DEFAULT 0,
    is_missing INTEGER NOT NULL DEFAULT 0,
    UNIQUE(library_id, rel_path)
);

CREATE TABLE IF NOT EXISTS crate (
    id INTEGER PRIMARY KEY,
    library_id INTEGER NOT NULL REFERENCES library(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    color TEXT,
    UNIQUE(library_id, name)
);

CREATE TABLE IF NOT EXISTS crate_track (
    crate_id INTEGER NOT NULL REFERENCES crate(id) ON DELETE CASCADE,
    track_id INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY (crate_id, track_id)
);

CREATE INDEX IF NOT EXISTS idx_track_sha256 ON track(sha256);
"#;

/// FTS index shape (v7): tag text plus rel_path, so filenames and crate
/// folder names are searchable — a dropped-in file with empty tags must be
/// findable by name. Shared between `open` (fresh database) and the v7
/// migration (drop + recreate + rebuild); the two must stay identical.
const FTS_SCHEMA: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS track_fts USING fts5(
    title, artist, album, genre, label, comment, rel_path,
    content='track', content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS track_fts_ai AFTER INSERT ON track BEGIN
    INSERT INTO track_fts(rowid, title, artist, album, genre, label, comment, rel_path)
    VALUES (new.id, new.title, new.artist, new.album, new.genre, new.label, new.comment, new.rel_path);
END;

CREATE TRIGGER IF NOT EXISTS track_fts_ad AFTER DELETE ON track BEGIN
    INSERT INTO track_fts(track_fts, rowid, title, artist, album, genre, label, comment, rel_path)
    VALUES ('delete', old.id, old.title, old.artist, old.album, old.genre, old.label, old.comment, old.rel_path);
END;

CREATE TRIGGER IF NOT EXISTS track_fts_au AFTER UPDATE ON track BEGIN
    INSERT INTO track_fts(track_fts, rowid, title, artist, album, genre, label, comment, rel_path)
    VALUES ('delete', old.id, old.title, old.artist, old.album, old.genre, old.label, old.comment, old.rel_path);
    INSERT INTO track_fts(rowid, title, artist, album, genre, label, comment, rel_path)
    VALUES (new.id, new.title, new.artist, new.album, new.genre, new.label, new.comment, new.rel_path);
END;
"#;

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)
        .with_context(|| format!("opening database at {}", path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(SCHEMA).context("creating schema")?;
    // No-op when track_fts already exists in any shape (IF NOT EXISTS); the
    // v7 migration below replaces a pre-rel_path index.
    conn.execute_batch(FTS_SCHEMA).context("creating fts")?;
    migrate(&conn)?;
    Ok(conn)
}

/// Applies incremental schema migrations tracked via PRAGMA user_version.
/// Version 0 is a v1 database (or a freshly created one); v2 adds analysis columns.
pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 2 {
        conn.execute_batch(
            "ALTER TABLE track ADD COLUMN waveform_peaks BLOB;
             ALTER TABLE track ADD COLUMN analyzed_at TEXT;
             ALTER TABLE track ADD COLUMN analysis_error TEXT;
             PRAGMA user_version = 2;",
        )
        .context("migrating schema to v2")?;
    }
    if version < 3 {
        // added_at: epoch millis, same unit as modified_at (file mtime).
        // Pre-existing rows get "now" — the real date is unknowable.
        conn.execute_batch(
            "ALTER TABLE track ADD COLUMN added_at INTEGER;
             UPDATE track SET added_at = CAST(strftime('%s','now') AS INTEGER) * 1000
             WHERE added_at IS NULL;
             PRAGMA user_version = 3;",
        )
        .context("migrating schema to v3")?;
    }
    if version < 4 {
        // key provenance: 'analysis' | 'manual' (NULL = legacy, overwritable)
        conn.execute_batch(
            "ALTER TABLE track ADD COLUMN key_source TEXT;
             ALTER TABLE track ADD COLUMN key_confidence REAL;
             PRAGMA user_version = 4;",
        )
        .context("migrating schema to v4")?;
    }
    if version < 5 {
        // ranked key candidates from the segment vote, JSON [{"key","p"},…]
        conn.execute_batch(
            "ALTER TABLE track ADD COLUMN key_candidates TEXT;
             PRAGMA user_version = 5;",
        )
        .context("migrating schema to v5")?;
    }
    if version < 6 {
        // identity of a removable library's physical stick (8 hex digits),
        // so a different stick on the same drive letter is not mistaken for
        // it and the same stick is recognized on any letter. NULL = fixed
        // library or a legacy row from before this migration.
        conn.execute_batch(
            "ALTER TABLE library ADD COLUMN volume_serial TEXT;
             PRAGMA user_version = 6;",
        )
        .context("migrating schema to v6")?;
    }
    if version < 7 {
        // rel_path joins the FTS index (filenames + crate folders become
        // searchable). External-content FTS tables can't ALTER — drop the
        // old index, recreate the v7 shape, rebuild from the track table.
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS track_fts_ai;
             DROP TRIGGER IF EXISTS track_fts_ad;
             DROP TRIGGER IF EXISTS track_fts_au;
             DROP TABLE IF EXISTS track_fts;",
        )
        .context("dropping pre-v7 fts")?;
        conn.execute_batch(FTS_SCHEMA)
            .context("recreating fts for v7")?;
        conn.execute_batch(
            "INSERT INTO track_fts(track_fts) VALUES('rebuild');
             PRAGMA user_version = 7;",
        )
        .context("migrating schema to v7")?;
    }
    if version < 8 {
        // quality assessment (spec 2026-07-21-audio-quality-design.md):
        // measured during the Analyze pass; quality_score 0-100 derived from
        // cutoff/format/bitrate/clipping — loudness shown but never scored.
        // v8, not v7: a parallel branch claimed v7 for the FTS rebuild above,
        // and DBs migrated by that build must still gain these columns.
        conn.execute_batch(
            "ALTER TABLE track ADD COLUMN loudness_lufs REAL;
             ALTER TABLE track ADD COLUMN clip_ratio REAL;
             ALTER TABLE track ADD COLUMN cutoff_hz INTEGER;
             ALTER TABLE track ADD COLUMN spectrogram BLOB;
             ALTER TABLE track ADD COLUMN quality_score INTEGER;
             PRAGMA user_version = 8;",
        )
        .context("migrating schema to v8")?;
    }
    if version < 9 {
        // enhanced_at: stamped when the Enhance engine (spec
        // 2026-07-2x-enhance-engine-design.md) has processed this track, so
        // it isn't re-offered for the same enhancement repeatedly.
        conn.execute_batch(
            "ALTER TABLE track ADD COLUMN enhanced_at TEXT;
             PRAGMA user_version = 9;",
        )
        .context("migrating schema to v9")?;
    }
    if version < 10 {
        // waveform_bands: interleaved low,mid,high (3 x 400 u8) driving the
        // frequency-colored deck waveform; NULL until (re-)analyzed.
        conn.execute_batch(
            "ALTER TABLE track ADD COLUMN waveform_bands BLOB;
             PRAGMA user_version = 10;",
        )
        .context("migrating schema to v10")?;
    }
    if version < 11 {
        // crate_pos: 1-based position of the track inside its own parent
        // folder (its folder-crate). A cache of the folder's
        // .spindex-order.json — NULL until that crate is first listed.
        conn.execute_batch(
            "ALTER TABLE track ADD COLUMN crate_pos INTEGER;
             PRAGMA user_version = 11;",
        )
        .context("migrating schema to v11")?;
    }
    if version < 12 {
        // beat_grid: detected beat times as little-endian f32 seconds. The
        // stored grid is the SAMPLED one the normal Analyze pass produces
        // (four 30 s islands on a long track) — enough to measure drift, not
        // enough to warp from; the stabilize worker runs its own full pass.
        // tempo_drift_pct is derived from it in save_analysis so the table
        // can sort and the health panel can filter without decoding the blob.
        // Both NULL until (re-)analyzed by a sidecar that emits "beats".
        conn.execute_batch(
            "ALTER TABLE track ADD COLUMN beat_grid BLOB;
             ALTER TABLE track ADD COLUMN tempo_drift_pct REAL;
             PRAGMA user_version = 12;",
        )
        .context("migrating schema to v12")?;
    }
    if version < 13 {
        // downbeat_grid: detected downbeat (bar-1) times as little-endian f32
        // seconds, sampled like beat_grid. serato_grid_written_at: set when
        // SpinDex writes a Serato BeatGrid frame to the file — drives the crate
        // sweep's skip logic. Both NULL until (re-)analyzed / gridded.
        conn.execute_batch(
            "ALTER TABLE track ADD COLUMN downbeat_grid BLOB;
             ALTER TABLE track ADD COLUMN serato_grid_written_at TEXT;
             PRAGMA user_version = 13;",
        )
        .context("migrating schema to v13")?;
    }
    if version < 14 {
        // serato_grid_source: who wrote the file's Serato grid — 'auto' (the
        // regenerate sweep) or 'manual' (the deck's grid editor). NULL = never
        // written by SpinDex. A manual grid is protected from regeneration,
        // like a manually set key is protected from analysis.
        conn.execute_batch(
            "ALTER TABLE track ADD COLUMN serato_grid_source TEXT;
             PRAGMA user_version = 14;",
        )
        .context("migrating schema to v14")?;
    }
    Ok(())
}

/// Returns the library id for the given root path, creating it if needed.
/// An existing library keeps its name — rescans of a device library must not
/// replace its volume-label name with the derived folder name ("E:\").
pub fn ensure_library(conn: &Connection, name: &str, root_path: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO library (name, root_path) VALUES (?1, ?2)
         ON CONFLICT(root_path) DO NOTHING",
        params![name, root_path],
    )?;
    let id = conn.query_row(
        "SELECT id FROM library WHERE root_path = ?1",
        params![root_path],
        |row| row.get(0),
    )?;
    Ok(id)
}

pub fn touch_library_scanned(conn: &Connection, library_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE library SET last_scanned_at = datetime('now') WHERE id = ?1",
        params![library_id],
    )?;
    Ok(())
}

/// (file_size, modified_at) per rel_path, used to skip unchanged files on rescan.
pub fn load_stamps(
    conn: &Connection,
    library_id: i64,
) -> Result<std::collections::HashMap<String, (i64, String)>> {
    let mut stmt =
        conn.prepare("SELECT rel_path, file_size, modified_at FROM track WHERE library_id = ?1")?;
    let rows = stmt.query_map(params![library_id], |row| {
        Ok((row.get::<_, String>(0)?, (row.get(1)?, row.get(2)?)))
    })?;
    let mut map = std::collections::HashMap::new();
    for row in rows {
        let (rel_path, stamp) = row?;
        map.insert(rel_path, stamp);
    }
    Ok(map)
}

pub fn upsert_track(
    conn: &Connection,
    library_id: i64,
    rel_path: &str,
    file_size: i64,
    modified_at: &str,
    meta: &TrackMeta,
) -> Result<()> {
    conn.execute(
        "INSERT INTO track (
            library_id, rel_path, file_size, modified_at,
            title, artist, album, genre, label, year, bpm, key_camelot,
            rating, comment, duration_secs, bitrate_kbps, sample_rate,
            format, has_artwork, is_missing, added_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, 0,
                  CAST(strftime('%s','now') AS INTEGER) * 1000)
        ON CONFLICT(library_id, rel_path) DO UPDATE SET
            file_size = excluded.file_size,
            modified_at = excluded.modified_at,
            sha256 = NULL,
            title = excluded.title, artist = excluded.artist,
            album = excluded.album, genre = excluded.genre, label = excluded.label,
            year = excluded.year,
            -- BPM and key are the two fields SpinDex can produce itself, and
            -- it produces them into the database only — nothing writes them
            -- back to the file. Taking the tag's value unconditionally here
            -- therefore threw the analysis away on the next rescan of a
            -- changed file (a tag edit, an enhance, a stabilize), and wiped
            -- the value outright when the file carried no such tag. A value
            -- SpinDex derived wins; the tag only fills what we don't have.
            bpm = CASE
                    WHEN analyzed_at IS NOT NULL AND bpm IS NOT NULL THEN bpm
                    ELSE COALESCE(excluded.bpm, bpm)
                  END,
            key_camelot = CASE
                    WHEN key_source IS NOT NULL AND key_camelot IS NOT NULL THEN key_camelot
                    ELSE COALESCE(excluded.key_camelot, key_camelot)
                  END,
            rating = excluded.rating, comment = excluded.comment,
            duration_secs = excluded.duration_secs, bitrate_kbps = excluded.bitrate_kbps,
            sample_rate = excluded.sample_rate, format = excluded.format,
            has_artwork = excluded.has_artwork, is_missing = 0",
        params![
            library_id,
            rel_path,
            file_size,
            modified_at,
            meta.title,
            meta.artist,
            meta.album,
            meta.genre,
            meta.label,
            meta.year,
            meta.bpm,
            meta.key,
            meta.rating,
            meta.comment,
            meta.duration_secs,
            meta.bitrate_kbps,
            meta.sample_rate,
            meta.format,
            meta.has_artwork as i64,
        ],
    )?;
    Ok(())
}

pub fn track_count(conn: &Connection, library_id: i64) -> Result<i64> {
    let count = conn.query_row(
        "SELECT COUNT(*) FROM track WHERE library_id = ?1",
        params![library_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

#[derive(Debug, serde::Serialize)]
pub struct LibraryInfo {
    pub id: i64,
    pub name: String,
    pub root_path: String,
    pub is_removable: bool,
    pub last_scanned_at: Option<String>,
    pub track_count: i64,
    /// Set on removable libraries: identity of the physical stick, used by
    /// the sidebar to group a device's libraries under one header.
    pub volume_serial: Option<String>,
}

pub fn list_libraries(conn: &Connection) -> Result<Vec<LibraryInfo>> {
    let mut stmt = conn.prepare(
        "SELECT l.id, l.name, l.root_path, l.is_removable, l.last_scanned_at,
                (SELECT COUNT(*) FROM track t WHERE t.library_id = l.id) AS track_count,
                l.volume_serial
         FROM library l ORDER BY l.name",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(LibraryInfo {
            id: row.get(0)?,
            name: row.get(1)?,
            root_path: row.get(2)?,
            is_removable: row.get::<_, i64>(3)? != 0,
            last_scanned_at: row.get(4)?,
            track_count: row.get(5)?,
            volume_serial: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn library_root(conn: &Connection, library_id: i64) -> Result<String> {
    let root = conn.query_row(
        "SELECT root_path FROM library WHERE id = ?1",
        params![library_id],
        |row| row.get(0),
    )?;
    Ok(root)
}

#[derive(Debug, serde::Serialize)]
pub struct TrackRow {
    pub id: i64,
    pub library_id: i64,
    pub rel_path: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub label: Option<String>,
    pub year: Option<i64>,
    pub bpm: Option<f64>,
    pub key_camelot: Option<String>,
    pub rating: Option<i64>,
    pub comment: Option<String>,
    pub duration_secs: Option<f64>,
    pub bitrate_kbps: Option<i64>,
    pub format: Option<String>,
    pub has_artwork: bool,
    pub is_missing: bool,
    pub added_at: Option<i64>,
    pub modified_at: String,
    pub key_source: Option<String>,
    pub key_confidence: Option<f64>,
    /// JSON [{"key":"8A","p":0.54},…], voted winner first.
    pub key_candidates: Option<String>,
    pub loudness_lufs: Option<f64>,
    pub clip_ratio: Option<f64>,
    pub cutoff_hz: Option<i64>,
    pub quality_score: Option<i64>,
    pub enhanced_at: Option<String>,
    /// 1-based manual position inside the track's own folder-crate.
    pub crate_pos: Option<i64>,
    /// How far the tempo wanders, as a percentage of the dominant tempo.
    /// NULL until analyzed by a sidecar that emits a beat grid.
    pub tempo_drift_pct: Option<f64>,
    /// Who wrote the file's Serato grid: 'auto', 'manual', or NULL (never SpinDex).
    pub serato_grid_source: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct TrackPage {
    pub rows: Vec<TrackRow>,
    pub total: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CrateFolder {
    /// Forward-slash path relative to the library root, e.g. "House/Deep House".
    pub path: String,
    /// Last path segment, the display name.
    pub name: String,
    /// Nesting level: "House" = 0, "House/Deep House" = 1.
    pub depth: usize,
    /// Tracks sitting directly in this folder (0 for purely intermediate folders).
    pub track_count: i64,
}

/// Folder-crates are derived, never stored. The tree is the union of the
/// subfolders on disk (the live mirror — empty folders included, so a
/// freshly created crate is visible) and the folders derived from
/// track.rel_path (so the crates of an unplugged stick stay listed); counts
/// always come from the index and cover only the tracks *directly* inside a
/// folder. Sorted hierarchically (parents immediately before their children)
/// so the sidebar can render the tree by indentation alone.
pub fn crate_folders(conn: &Connection, library_id: i64) -> Result<Vec<CrateFolder>> {
    let mut stmt =
        conn.prepare("SELECT rel_path FROM track WHERE library_id = ?1 AND is_missing = 0")?;
    let rows = stmt.query_map(params![library_id], |r| r.get::<_, String>(0))?;
    let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for rel_path in rows {
        let rel_path = rel_path?;
        // Loose files in the library root belong to the library itself, not a crate.
        let Some((folder, _file)) = rel_path.rsplit_once('/') else {
            continue;
        };
        *counts.entry(folder.to_string()).or_insert(0) += 1;
        let mut prefix = folder;
        while let Some((parent, _)) = prefix.rsplit_once('/') {
            counts.entry(parent.to_string()).or_insert(0);
            prefix = parent;
        }
    }
    let root = std::path::PathBuf::from(library_root(conn, library_id)?);
    if root.is_dir() {
        for entry in walkdir::WalkDir::new(&root)
            .min_depth(1)
            .into_iter()
            .flatten()
            .filter(|e| e.file_type().is_dir())
        {
            if let Ok(rel) = entry.path().strip_prefix(&root) {
                if let Some(rel) = rel.to_str() {
                    counts.entry(rel.replace('\\', "/")).or_insert(0);
                }
            }
        }
    }
    let mut out: Vec<CrateFolder> = counts
        .into_iter()
        .map(|(path, track_count)| {
            let name = path
                .rsplit_once('/')
                .map(|(_, n)| n)
                .unwrap_or(path.as_str())
                .to_string();
            let depth = path.matches('/').count();
            CrateFolder { path, name, depth, track_count }
        })
        .collect();
    // Compare segment-wise, not byte-wise: "House/Deep" must sort directly
    // after "House" even when a sibling like "House Party" exists (' ' < '/').
    out.sort_by(|a, b| a.path.split('/').cmp(b.path.split('/')));
    Ok(out)
}

/// Files sitting DIRECTLY in one crate folder ("" = loose in the library root),
/// with the filename and add stamp the manual order is reconciled against.
///
/// Missing files are INCLUDED (amends the original spec's "missing files hold
/// no slot"): `list_tracks` deliberately keeps a missing row visible and
/// dimmed, so excluding it here would renumber only the present tracks while
/// the missing row kept a stale `crate_pos` — duplicate numbers, and every row
/// after it at a different screen index than its index in the backend's order,
/// which makes drag-drop and ▲▼ act on the wrong slot. A missing file keeping
/// its place is also the better behaviour: the file may well come back.
///
/// Folder patterns are LIKE-escaped and bound, never interpolated: the folder
/// name is on-disk user data.
pub fn crate_track_files(
    conn: &Connection,
    library_id: i64,
    folder: &str,
) -> Result<Vec<crate::core::crate_order::CrateTrack>> {
    let (cond, patterns) = folder_direct_children(folder);
    let sql = format!(
        "SELECT t.id, t.rel_path, t.added_at FROM track t
         WHERE t.library_id = :lib AND ({cond})
         ORDER BY t.id"
    );
    let mut params: Vec<(&str, &dyn rusqlite::ToSql)> = vec![(":lib", &library_id)];
    if let Some((ref direct, ref deeper)) = patterns {
        params.push((":folder_direct", direct));
        params.push((":folder_deeper", deeper));
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), |row| {
        let rel: String = row.get(1)?;
        let file_name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
        Ok(crate::core::crate_order::CrateTrack {
            id: row.get(0)?,
            file_name,
            added_at: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Writes resolved positions in one transaction, so a crate is never half
/// renumbered. The `crate_pos <> ?2` guard matters: `list_tracks` refreshes a
/// crate's positions on every listing (every search debounce tick included),
/// and `track_fts_au` is a blanket AFTER UPDATE trigger — an unconditional
/// write would re-index every row of the crate in FTS for no change at all.
pub fn set_crate_positions(conn: &Connection, positions: &[(i64, i64)]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "UPDATE track SET crate_pos = ?2
             WHERE id = ?1 AND (crate_pos IS NULL OR crate_pos <> ?2)",
        )?;
        for (id, pos) in positions {
            stmt.execute(params![id, pos])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// A key SpinDex produced itself is a Camelot code ("8A", "12B"). Anything
/// else sitting in key_camelot came from the file's own tag — the scanner
/// stores `InitialKey` there for want of anywhere else, so a track can show
/// "Abm" or "G#m" without ever having been analyzed.
const IS_CAMELOT_KEY: &str = "(key_camelot GLOB '[1-9][AB]' OR key_camelot GLOB '1[0-2][AB]')";

/// When analysis is allowed to write the key. A 'manual' key is user ground
/// truth and is never touched. Otherwise analysis writes when re-analyzing,
/// when there is no key at all, or when the stored key is a tag's musical key
/// rather than a Camelot code — that last case is what a user means by
/// "analyze this track", and without it a tagged track could never be
/// analyzed at all.
fn key_writable_sql(overwrite_param: &str) -> String {
    format!("key_source IS NOT 'manual' AND ({overwrite_param} OR key_camelot IS NULL OR NOT {IS_CAMELOT_KEY})")
}

const TRACK_COLS: &str = "t.id, t.library_id, t.rel_path, t.title, t.artist, t.album, t.genre,
    t.label, t.year, t.bpm, t.key_camelot, t.rating, t.comment, t.duration_secs, t.bitrate_kbps,
    t.format, t.has_artwork, t.is_missing, t.added_at, t.modified_at, t.key_source,
    t.key_confidence, t.key_candidates, t.loudness_lufs, t.clip_ratio, t.cutoff_hz,
    t.quality_score, t.enhanced_at, t.crate_pos, t.tempo_drift_pct, t.serato_grid_source";

fn row_to_track(row: &rusqlite::Row) -> rusqlite::Result<TrackRow> {
    Ok(TrackRow {
        id: row.get(0)?,
        library_id: row.get(1)?,
        rel_path: row.get(2)?,
        title: row.get(3)?,
        artist: row.get(4)?,
        album: row.get(5)?,
        genre: row.get(6)?,
        label: row.get(7)?,
        year: row.get(8)?,
        bpm: row.get(9)?,
        key_camelot: row.get(10)?,
        rating: row.get(11)?,
        comment: row.get(12)?,
        duration_secs: row.get(13)?,
        bitrate_kbps: row.get(14)?,
        format: row.get(15)?,
        has_artwork: row.get::<_, i64>(16)? != 0,
        is_missing: row.get::<_, i64>(17)? != 0,
        added_at: row.get(18)?,
        modified_at: row.get(19)?,
        key_source: row.get(20)?,
        key_confidence: row.get(21)?,
        key_candidates: row.get(22)?,
        loudness_lufs: row.get(23)?,
        clip_ratio: row.get(24)?,
        cutoff_hz: row.get(25)?,
        quality_score: row.get(26)?,
        enhanced_at: row.get(27)?,
        crate_pos: row.get(28)?,
        tempo_drift_pct: row.get(29)?,
        serato_grid_source: row.get(30)?,
    })
}

#[derive(Debug, serde::Serialize)]
pub struct GenreCount {
    pub genre: String,
    pub count: i64,
}

/// Distinct genres across all libraries (present files only), most-used first.
/// Case-insensitive dedup: the shown spelling is the most frequent original
/// spelling within the group (tie: first byte-wise alphabetically).
pub fn list_genres(conn: &Connection) -> Result<Vec<GenreCount>> {
    let mut stmt = conn.prepare(
        "WITH spelled AS (
            SELECT TRIM(genre) AS g, LOWER(TRIM(genre)) AS lg, COUNT(*) AS n
            FROM track
            WHERE genre IS NOT NULL AND TRIM(genre) <> '' AND is_missing = 0
            GROUP BY TRIM(genre)
        )
        SELECT (SELECT s2.g FROM spelled s2 WHERE s2.lg = s.lg
                ORDER BY s2.n DESC, s2.g ASC LIMIT 1) AS genre,
               SUM(s.n) AS cnt
        FROM spelled s
        GROUP BY s.lg
        ORDER BY cnt DESC, genre COLLATE NOCASE ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(GenreCount {
            genre: r.get(0)?,
            count: r.get(1)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

#[derive(Debug, serde::Serialize)]
pub struct HealthCounts {
    pub duplicates: i64,
    pub missing_bpm: i64,
    pub missing_key: i64,
    pub missing_artwork: i64,
    pub missing_file: i64,
    pub analysis_error: i64,
    pub low_quality: i64,
    pub unsteady_tempo: i64,
}

/// `tempo_drift_pct >= UNSTEADY_PCT` as SQL, so the threshold has exactly one
/// home (core::tempo) and the count and the filter can never disagree.
fn unsteady_cond(col_prefix: &str) -> String {
    format!(
        "{col_prefix}is_missing = 0 AND {col_prefix}analyzed_at IS NOT NULL
         AND {col_prefix}tempo_drift_pct >= {}",
        crate::core::tempo::UNSTEADY_PCT
    )
}

/// Health counters for one library, or all libraries when None. The metadata
/// counters only consider present files; missing_file counts the absent ones.
pub fn library_health(conn: &Connection, library_id: Option<i64>) -> Result<HealthCounts> {
    let where_lib = match library_id {
        Some(_) => "WHERE library_id = :lib",
        None => "",
    };
    let sql = format!(
        "SELECT
            COALESCE(SUM(CASE WHEN is_missing = 0 AND bpm IS NULL THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN is_missing = 0 AND key_camelot IS NULL THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN is_missing = 0 AND has_artwork = 0 THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(is_missing), 0),
            COALESCE(SUM(CASE WHEN analysis_error IS NOT NULL THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN is_missing = 0 AND quality_score IS NOT NULL AND quality_score < 45 THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN {unsteady} THEN 1 ELSE 0 END), 0)
         FROM track {where_lib}",
        unsteady = unsteady_cond("")
    );
    let mut params_vec: Vec<(&str, &dyn rusqlite::ToSql)> = Vec::new();
    if let Some(ref lib) = library_id {
        params_vec.push((":lib", lib));
    }
    let (
        missing_bpm,
        missing_key,
        missing_artwork,
        missing_file,
        analysis_error,
        low_quality,
        unsteady_tempo,
    ) = conn.query_row(&sql, params_vec.as_slice(), |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
    })?;

    let mut duplicates = 0;
    let lib_ids: Vec<i64> = match library_id {
        Some(id) => vec![id],
        None => list_libraries(conn)?.into_iter().map(|l| l.id).collect(),
    };
    for id in lib_ids {
        duplicates += crate::core::dupes::duplicate_track_count(conn, id)?;
    }

    Ok(HealthCounts {
        duplicates,
        missing_bpm,
        missing_key,
        missing_artwork,
        missing_file,
        analysis_error,
        low_quality,
        unsteady_tempo,
    })
}

/// Whitelist mapping of frontend health-filter keys to WHERE expressions —
/// like sort_expr, filter input never reaches SQL as free text.
fn health_expr(key: &str) -> Option<String> {
    Some(match key {
        "missing_bpm" => "t.is_missing = 0 AND t.bpm IS NULL".to_string(),
        "missing_key" => "t.is_missing = 0 AND t.key_camelot IS NULL".to_string(),
        "missing_artwork" => "t.is_missing = 0 AND t.has_artwork = 0".to_string(),
        "missing_file" => "t.is_missing = 1".to_string(),
        "analysis_error" => "t.analysis_error IS NOT NULL".to_string(),
        "low_quality" => {
            "t.is_missing = 0 AND t.quality_score IS NOT NULL AND t.quality_score < 45".to_string()
        }
        "unsteady_tempo" => unsteady_cond("t."),
        _ => return None,
    })
}

/// Whitelist mapping of frontend sort keys to ORDER BY expressions —
/// sort input never reaches SQL as free text.
fn sort_expr(key: &str) -> Option<&'static str> {
    Some(match key {
        "title" => "t.title COLLATE NOCASE",
        "artist" => "t.artist COLLATE NOCASE",
        "album" => "t.album COLLATE NOCASE",
        "genre" => "t.genre COLLATE NOCASE",
        "year" => "t.year",
        "bpm" => "t.bpm",
        "rating" => "t.rating",
        // Camelot codes sort by wheel position (1A..12B), not as text —
        // zero-pad the number and append the letter: 8A -> "08A", 11A -> "11A".
        "key" => {
            "CASE WHEN t.key_camelot IS NULL THEN NULL
                  ELSE printf('%02d%s', CAST(t.key_camelot AS INTEGER),
                              UPPER(substr(t.key_camelot, -1))) END"
        }
        "time" => "t.duration_secs",
        "bitrate" => "t.bitrate_kbps",
        "format" => "t.format",
        "added" => "t.added_at",
        "updated" => "CAST(t.modified_at AS INTEGER)",
        "quality" => "t.quality_score",
        // manual crate order; NULLS LAST keeps unpositioned rows at the bottom
        "num" => "t.crate_pos",
        _ => return None,
    })
}

/// Turn free text into an FTS5 prefix query: each word becomes "word"*,
/// quotes stripped so user input can never break the MATCH syntax.
fn fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split_whitespace()
        .map(|t| t.replace('"', ""))
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

/// Escapes LIKE wildcards so a folder named "100% House" or "Deep_House"
/// matches literally under ESCAPE '\'.
fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// The one predicate matching files sitting DIRECTLY in a folder-crate
/// ("" = loose in the library root), plus the LIKE patterns to bind for it
/// (`None` for the root case, which needs no parameters).
///
/// `list_tracks` and `crate_track_files` MUST agree on this: the first decides
/// which rows the table shows, the second which rows get a position number.
/// They used to build the same SQL separately and drifted apart over the
/// `is_missing` filter — hence one shared helper.
fn folder_direct_children(folder: &str) -> (&'static str, Option<(String, String)>) {
    if folder.is_empty() {
        ("t.rel_path NOT LIKE '%/%'", None)
    } else {
        (
            "t.rel_path LIKE :folder_direct ESCAPE '\\' \
             AND t.rel_path NOT LIKE :folder_deeper ESCAPE '\\'",
            Some((
                format!("{}/%", like_escape(folder)),
                format!("{}/%/%", like_escape(folder)),
            )),
        )
    }
}

pub fn list_tracks(
    conn: &Connection,
    query: &str,
    library_id: Option<i64>,
    sort: Option<&str>,
    descending: bool,
    health: Option<&str>,
    folder: Option<&str>,
    ids: Option<&[i64]>,
    limit: i64,
    offset: i64,
) -> Result<TrackPage> {
    list_tracks_keyed(
        conn, query, library_id, sort, descending, health, folder, ids, None, limit, offset,
    )
}

/// The Camelot wheel position (1-12) and letter of every major/minor root,
/// with the enharmonic spellings taggers use. Drives `key_spellings`.
const KEY_ROOTS: [(&str, &[&str], &[&str]); 12] = [
    // (camelot number, minor roots (A), major roots (B))
    ("1", &["ab", "g#"], &["b", "cb"]),
    ("2", &["eb", "d#"], &["f#", "gb"]),
    ("3", &["bb", "a#"], &["db", "c#"]),
    ("4", &["f"], &["ab", "g#"]),
    ("5", &["c"], &["eb", "d#"]),
    ("6", &["g"], &["bb", "a#"]),
    ("7", &["d"], &["f"]),
    ("8", &["a"], &["c"]),
    ("9", &["e"], &["g"]),
    ("10", &["b"], &["d"]),
    ("11", &["f#", "gb"], &["a"]),
    ("12", &["db", "c#"], &["e"]),
];

/// Every stored spelling of a Camelot key, lower-cased with spaces removed:
/// the code itself ("8a", "08a") and the musical names a file tag may carry
/// ("am", "amin", "aminor"). `key_camelot` holds analysis codes AND tag keys
/// copied verbatim by the scanner, so a key filter has to match both. An
/// unknown code yields nothing.
pub fn key_spellings(code: &str) -> Vec<String> {
    let c = code.trim().to_lowercase();
    if c.is_empty() || !c.is_ascii() {
        return Vec::new();
    }
    let (num, letter) = c.split_at(c.len() - 1);
    let Some(&(_, minor, major)) = KEY_ROOTS.iter().find(|(n, _, _)| *n == num) else {
        return Vec::new();
    };
    let (roots, suffixes): (&[&str], &[&str]) = match letter {
        "a" => (minor, &["m", "min", "minor"]),
        "b" => (major, &["", "maj", "major"]),
        _ => return Vec::new(),
    };
    let mut out = vec![format!("{num}{letter}")];
    if num.len() == 1 {
        out.push(format!("0{num}{letter}"));
    }
    for root in roots {
        for suf in suffixes {
            out.push(format!("{root}{suf}"));
            // the proper sharp/flat signs some taggers write ("a♭m")
            if let Some(acc) = root.get(1..).filter(|a| !a.is_empty()) {
                let sign = if acc == "#" { "♯" } else { "♭" };
                out.push(format!("{}{sign}{suf}", &root[..1]));
            }
        }
    }
    out
}

/// `list_tracks` plus a key filter: `keys` are Camelot codes ("8A"), and a
/// track passes when its key is any spelling of one of them. An empty or
/// absent list means no key filter.
#[allow(clippy::too_many_arguments)]
pub fn list_tracks_keyed(
    conn: &Connection,
    query: &str,
    library_id: Option<i64>,
    sort: Option<&str>,
    descending: bool,
    health: Option<&str>,
    folder: Option<&str>,
    ids: Option<&[i64]>,
    keys: Option<&[String]>,
    limit: i64,
    offset: i64,
) -> Result<TrackPage> {
    let keys_json = keys.filter(|k| !k.is_empty()).map(|k| {
        let all: Vec<String> = k.iter().flat_map(|c| key_spellings(c)).collect();
        serde_json::to_string(&all).unwrap_or_else(|_| "[]".to_string())
    });
    const KEY_COND: &str =
        "REPLACE(LOWER(TRIM(t.key_camelot)), ' ', '') IN (SELECT value FROM json_each(:keys))";
    let lib_filter = |prefix: &str| match library_id {
        Some(_) => format!("{prefix} t.library_id = :lib"),
        None => String::new(),
    };
    let health_cond = health.and_then(health_expr);
    let health_filter = |prefix: &str| match &health_cond {
        Some(expr) => format!("{prefix} ({expr})"),
        None => String::new(),
    };
    // Folder-crate filter: direct children of `folder` only ("" = tracks
    // loose in the library root). Shared with crate_track_files so the listed
    // rows and the numbered rows can never diverge; patterns are LIKE-escaped
    // and bound, never interpolated — the folder name is on-disk user data.
    let folder_parts = folder.map(folder_direct_children);
    let folder_cond: Option<&str> = folder_parts.as_ref().map(|(c, _)| *c);
    let folder_filter = |prefix: &str| match folder_cond {
        Some(cond) => format!("{prefix} ({cond})"),
        None => String::new(),
    };
    let folder_patterns = folder_parts.as_ref().and_then(|(_, p)| p.as_ref());
    let folder_direct = folder_patterns.map(|(d, _)| d);
    let folder_deeper = folder_patterns.map(|(_, e)| e);
    // Explicit id-set filter (Serato crates): the set travels as one bound
    // JSON parameter through SQLite's json_each — no placeholder explosion.
    let ids_json = ids.map(|v| {
        serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string())
    });
    let ids_filter = |prefix: &str| match ids_json {
        Some(_) => {
            format!("{prefix} t.id IN (SELECT value FROM json_each(:ids))")
        }
        None => String::new(),
    };
    let dir = if descending { "DESC" } else { "ASC" };
    let order_override = sort
        .and_then(sort_expr)
        .map(|expr| format!("{expr} {dir} NULLS LAST"));
    // An explicit id set carries its own meaning: it is a Serato crate's
    // sequence, straight out of the .crate file. With no column sort chosen,
    // that sequence IS the order — json_each exposes the array index as `key`.
    let ids_order = "(SELECT je.key FROM json_each(:ids) je WHERE je.value = t.id)";

    match fts_query(query) {
        Some(match_expr) => {
            let where_lib = lib_filter(" AND");
            let where_health = health_filter(" AND");
            let where_folder = folder_filter(" AND");
            let where_ids = ids_filter(" AND");
            let where_keys = if keys_json.is_some() {
                format!(" AND {KEY_COND}")
            } else {
                String::new()
            };
            let order = order_override
                .as_deref()
                .unwrap_or(if ids_json.is_some() { ids_order } else { "rank" });
            let sql = format!(
                "SELECT {TRACK_COLS} FROM track t
                 JOIN track_fts f ON t.id = f.rowid
                 WHERE track_fts MATCH :q{where_lib}{where_health}{where_folder}{where_ids}{where_keys}
                 ORDER BY {order} LIMIT :limit OFFSET :offset"
            );
            let count_sql = format!(
                "SELECT COUNT(*) FROM track t
                 JOIN track_fts f ON t.id = f.rowid
                 WHERE track_fts MATCH :q{where_lib}{where_health}{where_folder}{where_ids}{where_keys}"
            );
            let mut params_vec: Vec<(&str, &dyn rusqlite::ToSql)> = vec![(":q", &match_expr)];
            if let Some(ref lib) = library_id {
                params_vec.push((":lib", lib));
            }
            if let (Some(direct), Some(deeper)) = (folder_direct, folder_deeper) {
                params_vec.push((":folder_direct", direct));
                params_vec.push((":folder_deeper", deeper));
            }
            if let Some(ref j) = ids_json {
                params_vec.push((":ids", j));
            }
            if let Some(ref k) = keys_json {
                params_vec.push((":keys", k));
            }
            let total = conn.query_row(&count_sql, params_vec.as_slice(), |row| row.get(0))?;
            let mut stmt = conn.prepare(&sql)?;
            params_vec.push((":limit", &limit));
            params_vec.push((":offset", &offset));
            let rows = stmt
                .query_map(params_vec.as_slice(), row_to_track)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(TrackPage { rows, total })
        }
        None => {
            let mut conds: Vec<String> = Vec::new();
            if library_id.is_some() {
                conds.push("t.library_id = :lib".to_string());
            }
            if let Some(expr) = health_cond {
                conds.push(format!("({expr})"));
            }
            if let Some(cond) = folder_cond {
                conds.push(format!("({cond})"));
            }
            if ids_json.is_some() {
                conds.push("t.id IN (SELECT value FROM json_each(:ids))".to_string());
            }
            if keys_json.is_some() {
                conds.push(KEY_COND.to_string());
            }
            let where_lib = if conds.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", conds.join(" AND "))
            };
            let order = order_override.as_deref().unwrap_or(if ids_json.is_some() {
                ids_order
            } else {
                "t.artist COLLATE NOCASE, t.title COLLATE NOCASE"
            });
            let sql = format!(
                "SELECT {TRACK_COLS} FROM track t {where_lib}
                 ORDER BY {order}
                 LIMIT :limit OFFSET :offset"
            );
            let count_sql = format!("SELECT COUNT(*) FROM track t {where_lib}");
            let mut params_vec: Vec<(&str, &dyn rusqlite::ToSql)> = Vec::new();
            if let Some(ref lib) = library_id {
                params_vec.push((":lib", lib));
            }
            if let (Some(direct), Some(deeper)) = (folder_direct, folder_deeper) {
                params_vec.push((":folder_direct", direct));
                params_vec.push((":folder_deeper", deeper));
            }
            if let Some(ref j) = ids_json {
                params_vec.push((":ids", j));
            }
            if let Some(ref k) = keys_json {
                params_vec.push((":keys", k));
            }
            let total = conn.query_row(&count_sql, params_vec.as_slice(), |row| row.get(0))?;
            let mut stmt = conn.prepare(&sql)?;
            params_vec.push((":limit", &limit));
            params_vec.push((":offset", &offset));
            let rows = stmt
                .query_map(params_vec.as_slice(), row_to_track)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(TrackPage { rows, total })
        }
    }
}

/// Stores an analysis result. bpm/key only fill NULL columns unless
/// `overwrite`; peaks and analyzed_at are always updated, the error cleared.
/// A key with key_source = 'manual' is user ground truth and is never
/// touched (bpm and waveform still update).
pub fn save_analysis(
    conn: &Connection,
    track_id: i64,
    bpm: Option<f64>,
    key: Option<&str>,
    key_confidence: Option<f64>,
    key_candidates: Option<&str>,
    peaks: &[u8],
    bands: Option<&[u8]>,
    beats: Option<&[f64]>,
    downbeats: Option<&[f64]>,
    overwrite: bool,
) -> Result<()> {
    // Beat grid as little-endian f32 seconds (half the size of f64, far
    // beyond the precision beat detection actually has), plus the drift
    // figure derived from it. A sidecar too old to emit beats clears both
    // rather than leaving a stale value that no longer matches the audio.
    let grid_blob: Option<Vec<u8>> = beats.map(|b| {
        let mut out = Vec::with_capacity(b.len() * 4);
        for t in b {
            out.extend_from_slice(&(*t as f32).to_le_bytes());
        }
        out
    });
    let drift = beats.and_then(crate::core::tempo::drift_pct);
    let down_blob: Option<Vec<u8>> = downbeats.map(|b| {
        let mut out = Vec::with_capacity(b.len() * 4);
        for t in b {
            out.extend_from_slice(&(*t as f32).to_le_bytes());
        }
        out
    });
    // Every key column keys off ONE predicate, so the value, its source and
    // its confidence can never disagree about whether analysis wrote them.
    let writable = key_writable_sql("?4");
    let sql = format!(
        "UPDATE track SET
            bpm = CASE WHEN ?4 OR bpm IS NULL THEN COALESCE(?2, bpm) ELSE bpm END,
            key_camelot = CASE WHEN {writable}
                               THEN COALESCE(?3, key_camelot) ELSE key_camelot END,
            key_source = CASE WHEN ({writable}) AND ?3 IS NOT NULL
                              THEN 'analysis' ELSE key_source END,
            key_confidence = CASE WHEN ({writable}) AND ?3 IS NOT NULL
                                  THEN ?6 ELSE key_confidence END,
            key_candidates = CASE WHEN ({writable}) AND ?3 IS NOT NULL
                                  THEN ?7 ELSE key_candidates END,
            waveform_peaks = ?5,
            waveform_bands = ?8,
            beat_grid = ?9,
            tempo_drift_pct = ?10,
            downbeat_grid = ?11,
            analyzed_at = datetime('now'),
            analysis_error = NULL
         WHERE id = ?1"
    );
    conn.execute(
        &sql,
        params![
            track_id,
            bpm,
            key,
            overwrite,
            peaks,
            key_confidence,
            key_candidates,
            bands,
            grid_blob,
            drift,
            down_blob
        ],
    )?;
    Ok(())
}

/// Stores quality metrics and derives quality_score from the row's own
/// format/bitrate (the score formula lives in core::quality). Quality is
/// always overwritten on re-analysis — nothing here is user-editable.
pub fn save_quality(
    conn: &Connection,
    track_id: i64,
    lufs: Option<f64>,
    clip_ratio: f64,
    cutoff_hz: Option<u32>,
    spectrogram: &[u8],
) -> Result<()> {
    let (format, bitrate): (Option<String>, Option<i64>) = conn.query_row(
        "SELECT format, bitrate_kbps FROM track WHERE id = ?1",
        params![track_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let score = crate::core::quality::quality_score(cutoff_hz, clip_ratio, format.as_deref(), bitrate);
    conn.execute(
        "UPDATE track SET loudness_lufs = ?2, clip_ratio = ?3, cutoff_hz = ?4,
            spectrogram = ?5, quality_score = ?6
         WHERE id = ?1",
        params![track_id, lufs, clip_ratio, cutoff_hz, spectrogram, score],
    )?;
    Ok(())
}

/// Marks a track as processed by the Enhance engine, so it isn't re-offered
/// for the same enhancement on a later pass.
pub fn mark_enhanced(conn: &Connection, track_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE track SET enhanced_at = datetime('now') WHERE id = ?1",
        params![track_id],
    )?;
    Ok(())
}

/// Clears a track's enhanced stamp (e.g. the enhancement was undone).
pub fn clear_enhanced(conn: &Connection, track_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE track SET enhanced_at = NULL WHERE id = ?1",
        params![track_id],
    )?;
    Ok(())
}

/// After an in-place file change (Flac-out replace, or any operation that
/// rewrites the file on disk): refresh the size/mtime stamps the scanner uses
/// for incremental rescans, and optionally move the row to a new rel_path and
/// format. `None` leaves the corresponding column unchanged. The FTS UPDATE
/// trigger re-indexes rel_path automatically.
pub fn update_track_file(
    conn: &Connection,
    track_id: i64,
    new_rel_path: Option<&str>,
    new_format: Option<&str>,
    file_size: i64,
    modified_at: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE track SET
            rel_path = COALESCE(?2, rel_path),
            format = COALESCE(?3, format),
            file_size = ?4,
            modified_at = ?5
         WHERE id = ?1",
        params![track_id, new_rel_path, new_format, file_size, modified_at],
    )?;
    Ok(())
}

#[derive(Debug, serde::Serialize)]
pub struct QualityDetail {
    pub loudness_lufs: Option<f64>,
    pub clip_ratio: Option<f64>,
    pub cutoff_hz: Option<i64>,
    pub quality_score: Option<i64>,
    pub spectrogram: Option<Vec<u8>>,
    pub sample_rate: Option<i64>,
    pub bitrate_kbps: Option<i64>,
    pub format: Option<String>,
}

pub fn quality_detail(conn: &Connection, track_id: i64) -> Result<QualityDetail> {
    conn.query_row(
        "SELECT loudness_lufs, clip_ratio, cutoff_hz, quality_score, spectrogram,
                sample_rate, bitrate_kbps, format
         FROM track WHERE id = ?1",
        params![track_id],
        |r| Ok(QualityDetail {
            loudness_lufs: r.get(0)?,
            clip_ratio: r.get(1)?,
            cutoff_hz: r.get(2)?,
            quality_score: r.get(3)?,
            spectrogram: r.get(4)?,
            sample_rate: r.get(5)?,
            bitrate_kbps: r.get(6)?,
            format: r.get(7)?,
        }),
    )
    .map_err(Into::into)
}

pub fn save_analysis_error(conn: &Connection, track_id: i64, msg: &str) -> Result<()> {
    conn.execute(
        "UPDATE track SET analysis_error = ?2, analyzed_at = datetime('now') WHERE id = ?1",
        params![track_id, msg],
    )?;
    Ok(())
}

pub fn get_waveform(conn: &Connection, track_id: i64) -> Result<Option<Vec<u8>>> {
    let peaks = conn.query_row(
        "SELECT waveform_peaks FROM track WHERE id = ?1",
        params![track_id],
        |row| row.get::<_, Option<Vec<u8>>>(0),
    )?;
    Ok(peaks)
}

pub fn waveform_bands(conn: &Connection, track_id: i64) -> Result<Option<Vec<u8>>> {
    let bands = conn.query_row(
        "SELECT waveform_bands FROM track WHERE id = ?1",
        params![track_id],
        |row| row.get::<_, Option<Vec<u8>>>(0),
    )?;
    Ok(bands)
}

/// Decodes the stored beat grid back to seconds. None when the track has not
/// been analyzed by a sidecar that emits beats. This is the SAMPLED grid —
/// good for drift and a suggested tempo, never for planning a warp.
pub fn beat_grid(conn: &Connection, track_id: i64) -> Result<Option<Vec<f64>>> {
    let blob = conn.query_row(
        "SELECT beat_grid FROM track WHERE id = ?1",
        params![track_id],
        |row| row.get::<_, Option<Vec<u8>>>(0),
    )?;
    Ok(blob.map(|b| {
        b.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64)
            .collect()
    }))
}

/// Stored (sampled) downbeat times in seconds; None until analyzed by a
/// sidecar that emits "downbeats".
pub fn downbeat_grid(conn: &Connection, track_id: i64) -> Result<Option<Vec<f64>>> {
    let blob = conn.query_row(
        "SELECT downbeat_grid FROM track WHERE id = ?1",
        params![track_id],
        |row| row.get::<_, Option<Vec<u8>>>(0),
    )?;
    Ok(blob.map(|b| {
        b.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64)
            .collect()
    }))
}

/// Records that a Serato BeatGrid frame was written to this track's file.
/// Records who wrote the file's Serato grid: 'auto' or 'manual'.
pub fn set_serato_grid_source(conn: &Connection, track_id: i64, source: &str) -> Result<()> {
    conn.execute(
        "UPDATE track SET serato_grid_source = ?2 WHERE id = ?1",
        params![track_id, source],
    )?;
    Ok(())
}

pub fn stamp_serato_grid_written(conn: &Connection, track_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE track SET serato_grid_written_at = datetime('now') WHERE id = ?1",
        params![track_id],
    )?;
    Ok(())
}

/// Of the given tracks, those that have a downbeat grid AND either were never
/// gridded by SpinDex or have been re-analyzed since the last grid write.
/// `rewrite_all` forces every track that has a downbeat grid. Order preserved
/// from `ids`; a row that can't be read is skipped.
pub fn tracks_needing_grid(conn: &Connection, ids: &[i64], rewrite_all: bool) -> Result<Vec<i64>> {
    let mut out = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT downbeat_grid IS NOT NULL, serato_grid_written_at, analyzed_at, serato_grid_source
         FROM track WHERE id = ?1 AND is_missing = 0",
    )?;
    for &id in ids {
        let row = stmt.query_row(params![id], |r| {
            Ok((
                r.get::<_, bool>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        });
        if let Ok((has_grid, written, analyzed, source)) = row {
            if !has_grid {
                continue;
            }
            // a hand-set grid is never replaced by a sweep, only on request
            if source.as_deref() == Some("manual") && !rewrite_all {
                continue;
            }
            let need = rewrite_all
                || written.is_none()
                || matches!((&written, &analyzed), (Some(w), Some(a)) if w < a);
            if need {
                out.push(id);
            }
        }
    }
    Ok(out)
}

/// Mirrors a successful tag write into the cache, including the new file
/// stamps so the next incremental rescan sees the file as unchanged.
pub fn apply_metadata_update(
    conn: &Connection,
    track_id: i64,
    update: &crate::core::metadata::TagUpdate,
    file_size: i64,
    modified_at: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE track SET
            title = ?2, artist = ?3, album = ?4, genre = ?5,
            year = ?6, bpm = ?7,
            key_source = CASE
                WHEN ?8 IS NULL THEN NULL
                WHEN key_camelot IS NOT ?8 THEN 'manual'
                ELSE key_source END,
            key_confidence = CASE
                WHEN ?8 IS NULL OR key_camelot IS NOT ?8 THEN NULL
                ELSE key_confidence END,
            key_camelot = ?8, comment = ?9,
            label = ?12, rating = ?13,
            file_size = ?10, modified_at = ?11
         WHERE id = ?1",
        params![
            track_id,
            update.title,
            update.artist,
            update.album,
            update.genre,
            update.year,
            update.bpm,
            update.key,
            update.comment,
            file_size,
            modified_at,
            update.label,
            update.rating,
        ],
    )?;
    Ok(())
}

/// Updates ONLY the columns named in `apply` (field→column whitelist), plus
/// the file stamps. Untouched columns are never written. When `key` is
/// applied, the manual-key rule from `apply_metadata_update` fires; when it is
/// not applied, key_camelot/key_source/key_confidence are left alone.
pub fn apply_partial_metadata(
    conn: &Connection,
    id: i64,
    update: &crate::core::metadata::TagUpdate,
    apply: &[String],
    file_size: i64,
    modified_at: &str,
) -> Result<()> {
    let has = |f: &str| apply.iter().any(|a| a == f);
    let mut set: Vec<String> = Vec::new();
    let mut params: Vec<(&str, &dyn rusqlite::ToSql)> = Vec::new();

    if has("title") { set.push("title = :title".into()); params.push((":title", &update.title)); }
    if has("artist") { set.push("artist = :artist".into()); params.push((":artist", &update.artist)); }
    if has("album") { set.push("album = :album".into()); params.push((":album", &update.album)); }
    if has("genre") { set.push("genre = :genre".into()); params.push((":genre", &update.genre)); }
    if has("label") { set.push("label = :label".into()); params.push((":label", &update.label)); }
    if has("year") { set.push("year = :year".into()); params.push((":year", &update.year)); }
    if has("bpm") { set.push("bpm = :bpm".into()); params.push((":bpm", &update.bpm)); }
    if has("comment") { set.push("comment = :comment".into()); params.push((":comment", &update.comment)); }
    if has("rating") { set.push("rating = :rating".into()); params.push((":rating", &update.rating)); }
    if has("key") {
        set.push(
            "key_source = CASE WHEN :key IS NULL THEN NULL \
                WHEN key_camelot IS NOT :key THEN 'manual' ELSE key_source END".into(),
        );
        set.push(
            "key_confidence = CASE WHEN :key IS NULL OR key_camelot IS NOT :key \
                THEN NULL ELSE key_confidence END".into(),
        );
        set.push("key_camelot = :key".into());
        params.push((":key", &update.key));
    }

    if set.is_empty() {
        return Ok(()); // nothing ticked for this row
    }

    set.push("file_size = :size".into());
    set.push("modified_at = :mtime".into());
    params.push((":size", &file_size));
    params.push((":mtime", &modified_at));
    params.push((":id", &id));

    let sql = format!("UPDATE track SET {} WHERE id = :id", set.join(", "));
    conn.execute(&sql, &params[..])?;
    Ok(())
}

/// Deletes track rows by id (the caller has already verified the files are
/// gone from disk). FTS stays in sync via the existing triggers. Returns the
/// number of rows deleted.
pub fn delete_track_rows(conn: &Connection, ids: &[i64]) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let json = serde_json::to_string(ids)?;
    let n = conn.execute(
        "DELETE FROM track WHERE id IN (SELECT value FROM json_each(?1))",
        params![json],
    )?;
    Ok(n)
}

pub fn set_library_removable(
    conn: &Connection,
    library_id: i64,
    volume_serial: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE library SET is_removable = 1, volume_serial = ?2 WHERE id = ?1",
        params![library_id, volume_serial],
    )?;
    Ok(())
}

/// Removable libraries belonging to the physical stick with this volume
/// serial — the drive letter is deliberately not part of the identity.
/// Legacy rows with a NULL serial never match.
pub fn removable_libraries_for_serial(conn: &Connection, serial: &str) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM library WHERE is_removable = 1 AND volume_serial = ?1",
    )?;
    let rows = stmt.query_map(params![serial], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Drops every library of a removable stick (by volume serial) from the
/// index — tracks go with them via ON DELETE CASCADE. Audio files are never
/// touched; the stick simply presents as fresh/unindexed next time. Returns
/// the number of libraries removed.
pub fn forget_device(conn: &Connection, serial: &str) -> Result<usize> {
    let n = conn.execute(
        "DELETE FROM library WHERE is_removable = 1 AND volume_serial = ?1",
        params![serial],
    )?;
    Ok(n)
}

/// Unlinks one library from the index (tracks cascade). Never touches the
/// folder or any file on disk — deleting content is Explorer's job.
pub fn remove_library(conn: &Connection, library_id: i64) -> Result<()> {
    conn.execute("DELETE FROM library WHERE id = ?1", params![library_id])?;
    Ok(())
}

/// After a crate folder was renamed on disk: rewrites the rel_path prefix of
/// every track under it (nested included). Exact segment match — a sibling
/// like "House2" is never caught by renaming "House". Returns rows changed.
pub fn rename_crate_paths(
    conn: &Connection,
    library_id: i64,
    old_path: &str,
    new_path: &str,
) -> Result<usize> {
    let n = conn.execute(
        "UPDATE track
         SET rel_path = ?1 || substr(rel_path, length(?2) + 1)
         WHERE library_id = ?3
           AND substr(rel_path, 1, length(?2) + 1) = ?2 || '/'",
        params![new_path, old_path, library_id],
    )?;
    Ok(n)
}

/// Track id for an exact (library, rel_path) pair — how Serato crate entries
/// are matched against the index. None = not indexed (yet).
pub fn track_id_by_rel(
    conn: &Connection,
    library_id: i64,
    rel_path: &str,
) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM track WHERE library_id = ?1 AND rel_path = ?2",
            params![library_id, rel_path],
            |r| r.get(0),
        )
        .optional()?)
}

/// After a file was moved between folders of its library: point its row at
/// the new rel_path (size/mtime/tags are unchanged by a move).
pub fn set_track_rel_path(conn: &Connection, track_id: i64, rel_path: &str) -> Result<()> {
    conn.execute(
        "UPDATE track SET rel_path = ?1 WHERE id = ?2",
        params![rel_path, track_id],
    )?;
    Ok(())
}

/// A different stick took over a drive letter whose root a disconnected
/// removable library still owns (root_path is UNIQUE, so the newcomer would
/// otherwise inherit that row and its tracks). Parks the old row on a
/// placeholder root ('missing://<id><folder-suffix>', e.g. a root of
/// "D:\MP3\" parks to "missing://7\MP3\") — its real root is restored by
/// `devices::swap_drive_prefix` (folder suffix included) when its own stick
/// reconnects under a new letter. Rows whose serial matches the connecting
/// stick are left alone.
pub fn park_conflicting_removable(
    conn: &Connection,
    root: &str,
    serial: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE library SET root_path = 'missing://' || id || substr(root_path, 3)
         WHERE is_removable = 1 AND root_path = ?1
           AND (volume_serial IS NULL OR volume_serial <> ?2)",
        params![root, serial.unwrap_or("")],
    )?;
    Ok(())
}

/// Moves a library to a new root (a known stick reappearing under a
/// different drive letter). Track rel_paths are relative and survive as-is.
pub fn update_library_root(conn: &Connection, library_id: i64, new_root: &str) -> Result<()> {
    conn.execute(
        "UPDATE library SET root_path = ?2 WHERE id = ?1",
        params![library_id, new_root],
    )?;
    Ok(())
}

/// Marks every track of a library missing (drive unplugged). Cache is kept.
pub fn mark_library_tracks_missing(conn: &Connection, library_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE track SET is_missing = 1 WHERE library_id = ?1",
        params![library_id],
    )?;
    Ok(())
}

/// Reverse of mark_library_tracks_missing, used when a known drive returns.
/// (The incremental scanner skips unchanged files entirely, so it would
/// never reset the flag itself.)
pub fn clear_library_missing(conn: &Connection, library_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE track SET is_missing = 0 WHERE library_id = ?1",
        params![library_id],
    )?;
    Ok(())
}

/// Flags the given rel_paths of a library as missing — the scanner's deletion
/// detection for files that vanished from disk between scans. Rows are KEPT
/// (the cache stays browsable and the health panel counts them under "broken"),
/// only the flag changes; a returning file clears it again via `upsert_track`.
/// The `is_missing = 0` guard means already-flagged rows are not re-counted,
/// so the returned number is genuinely "newly gone since the last scan".
pub fn mark_missing_paths(conn: &Connection, library_id: i64, rel_paths: &[String]) -> Result<u64> {
    if rel_paths.is_empty() {
        return Ok(0);
    }
    let mut stmt = conn.prepare(
        "UPDATE track SET is_missing = 1
         WHERE library_id = ?1 AND rel_path = ?2 AND is_missing = 0",
    )?;
    let mut flagged = 0u64;
    for rel_path in rel_paths {
        flagged += stmt.execute(params![library_id, rel_path])? as u64;
    }
    Ok(flagged)
}

/// Reverse of `mark_missing_paths`: a track whose file has reappeared
/// (restored, moved back) is unflagged. Same scoping and the mirror-image
/// guard (`is_missing = 1`, so an already-present row is never re-counted).
pub fn clear_missing_paths(conn: &Connection, library_id: i64, rel_paths: &[String]) -> Result<u64> {
    if rel_paths.is_empty() {
        return Ok(0);
    }
    let mut stmt = conn.prepare(
        "UPDATE track SET is_missing = 0
         WHERE library_id = ?1 AND rel_path = ?2 AND is_missing = 1",
    )?;
    let mut cleared = 0u64;
    for rel_path in rel_paths {
        cleared += stmt.execute(params![library_id, rel_path])? as u64;
    }
    Ok(cleared)
}

/// Mirrors an artwork change into the cache with refreshed file stamps.
pub fn update_artwork_flag(
    conn: &Connection,
    track_id: i64,
    has_artwork: bool,
    file_size: i64,
    modified_at: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE track SET has_artwork = ?2, file_size = ?3, modified_at = ?4 WHERE id = ?1",
        params![track_id, has_artwork as i64, file_size, modified_at],
    )?;
    Ok(())
}

pub fn mark_track_missing(conn: &Connection, track_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE track SET is_missing = 1 WHERE id = ?1",
        params![track_id],
    )?;
    Ok(())
}

/// Absolute path of a track (library root + rel_path).
/// The metadata SpinDex holds for one track, shaped for a tag write.
///
/// This is the export direction: BPM and `key_camelot` in particular live only
/// in the database — analysis never writes them back to the file — so this is
/// how an analyzed BPM or Camelot key ever reaches Serato.
pub fn track_tag_update(
    conn: &Connection,
    track_id: i64,
) -> Result<crate::core::metadata::TagUpdate> {
    let u = conn.query_row(
        "SELECT title, artist, album, genre, year, bpm, key_camelot, comment, label, rating
         FROM track WHERE id = ?1",
        params![track_id],
        |r| {
            Ok(crate::core::metadata::TagUpdate {
                title: r.get(0)?,
                artist: r.get(1)?,
                album: r.get(2)?,
                genre: r.get(3)?,
                year: r.get(4)?,
                bpm: r.get(5)?,
                key: r.get(6)?,
                comment: r.get(7)?,
                label: r.get(8)?,
                rating: r.get(9)?,
            })
        },
    )?;
    Ok(u)
}

pub fn track_abs_path(conn: &Connection, track_id: i64) -> Result<std::path::PathBuf> {
    let (root, abs) = track_root_and_abs_path(conn, track_id)?;
    let _ = root;
    Ok(abs)
}

/// A track's library root alongside its absolute path. The root lets a caller
/// tell "this file is confirmed gone" (root reachable, file absent) apart from
/// "this library isn't currently reachable" (e.g. an unplugged USB stick) —
/// `track_abs_path` alone can't make that distinction.
pub fn track_root_and_abs_path(
    conn: &Connection,
    track_id: i64,
) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    let (root, rel): (String, String) = conn.query_row(
        "SELECT l.root_path, t.rel_path FROM track t
         JOIN library l ON l.id = t.library_id WHERE t.id = ?1",
        params![track_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let root = std::path::PathBuf::from(root);
    let abs = root.join(rel);
    Ok((root, abs))
}

/// One indexed, present track on a stick, as the DJ sync reads it.
#[derive(Debug, Clone)]
pub struct StickTrack {
    pub id: i64,
    pub abs_path: std::path::PathBuf,
    pub stick_rel_path: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub label: Option<String>,
    pub year: Option<i64>,
    pub comment: Option<String>,
    pub duration_secs: Option<f64>,
    pub bitrate_kbps: Option<i64>,
    pub sample_rate: Option<i64>,
    pub file_size: i64,
    pub format: Option<String>,
    pub bpm: Option<f64>,
    pub key_camelot: Option<String>,
}

/// Every present track in the libraries on drive `letter` ("D:"), sorted by
/// stick-relative path (case-insensitive) so exports are deterministic.
pub fn stick_tracks(conn: &Connection, letter: &str) -> Result<Vec<StickTrack>> {
    let prefix = format!("{letter}\\");
    let mut stmt = conn.prepare(
        "SELECT t.id, l.root_path, t.rel_path, t.title, t.artist, t.album, t.genre, t.label,
                t.year, t.comment, t.duration_secs, t.bitrate_kbps, t.sample_rate, t.file_size,
                t.format, t.bpm, t.key_camelot
         FROM track t JOIN library l ON l.id = t.library_id
         WHERE t.is_missing = 0 AND substr(l.root_path, 1, length(?1)) = ?1",
    )?;
    let rows = stmt.query_map(params![prefix], |r| {
        let root: String = r.get(1)?;
        let rel: String = r.get(2)?;
        Ok((
            root,
            rel,
            StickTrack {
                id: r.get(0)?,
                abs_path: std::path::PathBuf::new(),
                stick_rel_path: String::new(),
                title: r.get(3)?,
                artist: r.get(4)?,
                album: r.get(5)?,
                genre: r.get(6)?,
                label: r.get(7)?,
                year: r.get(8)?,
                comment: r.get(9)?,
                duration_secs: r.get(10)?,
                bitrate_kbps: r.get(11)?,
                sample_rate: r.get(12)?,
                file_size: r.get(13)?,
                format: r.get(14)?,
                bpm: r.get(15)?,
                key_camelot: r.get(16)?,
            },
        ))
    })?;
    let mut out: Vec<(usize, StickTrack)> = Vec::new();
    for row in rows {
        let (root, rel, mut t) = row?;
        let abs = std::path::PathBuf::from(&root).join(&rel);
        let full = abs.to_string_lossy().to_string();
        t.stick_rel_path = full.strip_prefix(&prefix).unwrap_or(&full).replace('\\', "/");
        t.abs_path = abs;
        out.push((root.len(), t));
    }
    // Overlapping libraries (a recursive one at the drive root plus a nested
    // one) index the same file twice. Keep one row per file: the longest
    // root, which is the library stick_path::resolve_with picks for crates.
    out.sort_by(|a, b| {
        a.1.stick_rel_path
            .to_lowercase()
            .cmp(&b.1.stick_rel_path.to_lowercase())
            .then(b.0.cmp(&a.0))
    });
    out.dedup_by(|b, a| a.1.stick_rel_path.eq_ignore_ascii_case(&b.1.stick_rel_path));
    Ok(out.into_iter().map(|(_, t)| t).collect())
}

/// Tracks to analyze as (id, absolute path). Filters: explicit ids, a single
/// library, and/or only tracks still missing bpm or key. Missing files are
/// excluded.
pub fn tracks_for_analysis(
    conn: &Connection,
    track_ids: Option<&[i64]>,
    library_id: Option<i64>,
    only_missing: bool,
) -> Result<Vec<(i64, std::path::PathBuf)>> {
    let mut sql = String::from(
        "SELECT t.id, l.root_path, t.rel_path FROM track t
         JOIN library l ON l.id = t.library_id
         WHERE t.is_missing = 0",
    );
    if only_missing {
        // A tag's musical key is not a key SpinDex can use, so a track
        // carrying one still counts as needing analysis; a manual key does
        // not, since analysis would refuse to touch it anyway.
        sql.push_str(&format!(
            " AND (t.bpm IS NULL OR t.key_camelot IS NULL
                   OR (t.key_source IS NOT 'manual' AND NOT {}))",
            IS_CAMELOT_KEY.replace("key_camelot", "t.key_camelot")
        ));
    }
    if library_id.is_some() {
        sql.push_str(" AND t.library_id = :lib");
    }
    if let Some(ids) = track_ids {
        let list = ids
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND t.id IN ({list})"));
    }
    sql.push_str(" ORDER BY t.id");

    let mut params_vec: Vec<(&str, &dyn rusqlite::ToSql)> = Vec::new();
    if let Some(ref lib) = library_id {
        params_vec.push((":lib", lib));
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_vec.as_slice(), |row| {
        let root: String = row.get(1)?;
        let rel: String = row.get(2)?;
        Ok((row.get::<_, i64>(0)?, std::path::Path::new(&root).join(rel)))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Everything the enhance worker needs to plan and run one track: library
/// id/root (for path joins, backups and Serato repointing) plus the stored
/// rel_path/format/sample_rate. Bit depth isn't a DB column — the worker
/// reads it from the file itself via `metadata::bit_depth`. Missing files
/// are excluded; unknown ids are silently dropped (matches `tracks_for_copy`).
pub struct EnhanceTarget {
    pub id: i64,
    pub library_id: i64,
    pub root_path: std::path::PathBuf,
    pub rel_path: String,
    pub format: String,
    pub sample_rate: Option<i64>,
}

pub fn tracks_for_enhance(conn: &Connection, track_ids: &[i64]) -> Result<Vec<EnhanceTarget>> {
    if track_ids.is_empty() {
        return Ok(Vec::new());
    }
    let list = track_ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT t.id, t.library_id, l.root_path, t.rel_path, COALESCE(t.format, ''), t.sample_rate
         FROM track t JOIN library l ON l.id = t.library_id
         WHERE t.is_missing = 0 AND t.id IN ({list})
         ORDER BY t.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(EnhanceTarget {
            id: row.get(0)?,
            library_id: row.get(1)?,
            root_path: std::path::PathBuf::from(row.get::<_, String>(2)?),
            rel_path: row.get(3)?,
            format: row.get(4)?,
            sample_rate: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Unhashed, present tracks as (id, absolute path), optionally limited to
/// specific libraries. Feeds the SHA-256 hashing queue.
pub fn tracks_for_hashing(
    conn: &Connection,
    library_ids: Option<&[i64]>,
) -> Result<Vec<(i64, std::path::PathBuf)>> {
    let mut sql = String::from(
        "SELECT t.id, l.root_path, t.rel_path FROM track t
         JOIN library l ON l.id = t.library_id
         WHERE t.is_missing = 0 AND t.sha256 IS NULL",
    );
    if let Some(ids) = library_ids {
        let list = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        sql.push_str(&format!(" AND t.library_id IN ({list})"));
    }
    sql.push_str(" ORDER BY t.id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        let root: String = row.get(1)?;
        let rel: String = row.get(2)?;
        Ok((row.get::<_, i64>(0)?, std::path::Path::new(&root).join(rel)))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Tracks for a copy operation as (source library id, absolute path),
/// in the order of the given ids. Unknown ids are silently dropped.
pub fn tracks_for_copy(
    conn: &Connection,
    track_ids: &[i64],
) -> Result<Vec<(i64, std::path::PathBuf)>> {
    let mut stmt = conn.prepare(
        "SELECT t.library_id, l.root_path, t.rel_path FROM track t
         JOIN library l ON l.id = t.library_id WHERE t.id = ?1",
    )?;
    let mut out = Vec::with_capacity(track_ids.len());
    for &id in track_ids {
        let row = stmt
            .query_row(params![id], |row| {
                let lib: i64 = row.get(0)?;
                let root: String = row.get(1)?;
                let rel: String = row.get(2)?;
                Ok((lib, std::path::Path::new(&root).join(rel)))
            })
            .optional()?;
        if let Some(item) = row {
            out.push(item);
        }
    }
    Ok(out)
}

/// All non-missing tracks under a crate folder (direct AND nested children),
/// as (absolute source path, source rel_path). Sibling folders sharing the
/// prefix ("House Party" vs "House") never match: the LIKE pattern requires
/// a '/' after the folder, with %/_ escaped so odd folder names match
/// literally.
pub fn tracks_in_subtree(
    conn: &Connection,
    library_id: i64,
    folder: &str,
) -> Result<Vec<(std::path::PathBuf, String)>> {
    let pattern = format!("{}/%", like_escape(folder));
    let mut stmt = conn.prepare(
        "SELECT l.root_path, t.rel_path FROM track t
         JOIN library l ON l.id = t.library_id
         WHERE t.library_id = :lib AND t.is_missing = 0
           AND t.rel_path LIKE :pat ESCAPE '\\'",
    )?;
    let rows = stmt.query_map(
        rusqlite::named_params! { ":lib": library_id, ":pat": pattern },
        |r| {
            let root: String = r.get(0)?;
            let rel: String = r.get(1)?;
            Ok((std::path::Path::new(&root).join(&rel), rel))
        },
    )?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn save_sha256(conn: &Connection, track_id: i64, sha256: &str) -> Result<()> {
    conn.execute(
        "UPDATE track SET sha256 = ?2 WHERE id = ?1",
        params![track_id, sha256],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db(name: &str) -> (std::path::PathBuf, Connection) {
        let dir = std::env::temp_dir().join(format!("spindex-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        let conn = open(&path).unwrap();
        (path, conn)
    }

    fn insert_track(conn: &Connection, rel: &str, bpm: Option<f64>, key: Option<&str>) -> i64 {
        conn.execute(
            "INSERT INTO track (library_id, rel_path, file_size, modified_at, bpm, key_camelot)
             VALUES (1, ?1, 1, 'now', ?2, ?3)",
            params![rel, bpm, key],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn meta_with(bpm: Option<f64>, key: Option<&str>) -> crate::core::metadata::TrackMeta {
        let mut m = crate::core::metadata::TrackMeta::default();
        m.bpm = bpm;
        m.key = key.map(|s| s.to_string());
        m
    }

    fn key_bpm_of(conn: &Connection, rel: &str) -> (Option<String>, Option<f64>) {
        conn.query_row(
            "SELECT key_camelot, bpm FROM track WHERE rel_path = ?1",
            params![rel],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    /// Analysis results live only in the database — the Camelot key and the
    /// detected BPM are never written to the file. A rescan re-reads the file
    /// and used to overwrite both from its tags, so any change to the file
    /// (a tag edit, an enhance, a stabilize) silently threw the analysis away:
    /// a file with no key tag had its analyzed key wiped to NULL entirely.
    #[test]
    fn a_rescan_does_not_throw_away_analysis_results() {
        let (path, conn) = test_db("keep-analysis.db");
        let lib = ensure_library(&conn, "lib", "C:/music").unwrap();

        // no key or bpm in the file's tags at all
        upsert_track(&conn, lib, "a.mp3", 1, "1", &meta_with(None, None)).unwrap();
        let id: i64 = conn
            .query_row("SELECT id FROM track WHERE rel_path = 'a.mp3'", [], |r| r.get(0))
            .unwrap();
        save_analysis(&conn, id, Some(126.0), Some("8A"), Some(0.9), None, &[1], None, None, None, false)
            .unwrap();
        assert_eq!(key_bpm_of(&conn, "a.mp3"), (Some("8A".into()), Some(126.0)));

        // the file changes on disk and the scanner re-upserts it
        upsert_track(&conn, lib, "a.mp3", 2, "2", &meta_with(None, None)).unwrap();
        assert_eq!(
            key_bpm_of(&conn, "a.mp3"),
            (Some("8A".into()), Some(126.0)),
            "a rescan wiped the analyzed key/bpm"
        );

        // a musical key in the tags must not beat the analyzed Camelot code
        upsert_track(&conn, lib, "b.mp3", 1, "1", &meta_with(Some(120.0), Some("Abm"))).unwrap();
        let id_b: i64 = conn
            .query_row("SELECT id FROM track WHERE rel_path = 'b.mp3'", [], |r| r.get(0))
            .unwrap();
        save_analysis(&conn, id_b, Some(126.0), Some("2A"), Some(0.9), None, &[1], None, None, None, true)
            .unwrap();
        upsert_track(&conn, lib, "b.mp3", 2, "2", &meta_with(Some(120.0), Some("Abm"))).unwrap();
        assert_eq!(
            key_bpm_of(&conn, "b.mp3"),
            (Some("2A".into()), Some(126.0)),
            "the file tag overwrote an analyzed key/bpm"
        );

        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    /// The other half of the rule: tags still FILL what SpinDex doesn't have,
    /// and a track SpinDex has never analyzed keeps tracking its file.
    #[test]
    fn a_rescan_still_takes_tags_the_app_has_no_value_for() {
        let (path, conn) = test_db("tags-fill.db");
        let lib = ensure_library(&conn, "lib", "C:/music").unwrap();

        // never analyzed: the tag is the only source, and it keeps tracking
        upsert_track(&conn, lib, "a.mp3", 1, "1", &meta_with(Some(120.0), Some("Abm"))).unwrap();
        assert_eq!(key_bpm_of(&conn, "a.mp3"), (Some("Abm".into()), Some(120.0)));
        upsert_track(&conn, lib, "a.mp3", 2, "2", &meta_with(Some(124.0), Some("Fm"))).unwrap();
        assert_eq!(
            key_bpm_of(&conn, "a.mp3"),
            (Some("Fm".into()), Some(124.0)),
            "an unanalyzed track must follow its file's tags"
        );

        // analyzed, but the analyzer found no key: the tag fills that gap
        let id: i64 = conn
            .query_row("SELECT id FROM track WHERE rel_path = 'a.mp3'", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "UPDATE track SET key_camelot = NULL, bpm = NULL, key_source = NULL,
                 analyzed_at = datetime('now') WHERE id = ?1",
            params![id],
        )
        .unwrap();
        upsert_track(&conn, lib, "a.mp3", 3, "3", &meta_with(Some(124.0), Some("Fm"))).unwrap();
        assert_eq!(
            key_bpm_of(&conn, "a.mp3"),
            (Some("Fm".into()), Some(124.0)),
            "tags must still fill a value the app does not have"
        );

        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tracks_needing_grid_never_touches_a_manual_grid_unless_forced() {
        let (path, conn) = test_db("needing-grid-manual.db");
        let _lib = ensure_library(&conn, "L", "C:/l").unwrap();
        let a = insert_track(&conn, "a.mp3", None, None);
        let downs = [0.0f64; 10];
        save_analysis(
            &conn, a, Some(120.0), None, None, None, &[1], None, None, Some(&downs), true,
        )
        .unwrap();
        set_serato_grid_source(&conn, a, "manual").unwrap();
        // never gridded by the sweep, yet manual: the sweep must leave it alone
        assert!(tracks_needing_grid(&conn, &[a], false).unwrap().is_empty());
        // an explicit per-track overwrite still can
        assert_eq!(tracks_needing_grid(&conn, &[a], true).unwrap(), vec![a]);
        let src: Option<String> = conn
            .query_row("SELECT serato_grid_source FROM track WHERE id = ?1", [a], |r| r.get(0))
            .unwrap();
        assert_eq!(src.as_deref(), Some("manual"));
        drop(conn);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn pre_v14_database_migrates_and_lists_tracks() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        migrate(&conn).unwrap();
        conn.execute_batch(
            "ALTER TABLE track DROP COLUMN serato_grid_source;
             PRAGMA user_version = 13;",
        )
        .unwrap();
        migrate(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 14);
        conn.execute("INSERT INTO library (id, name, root_path) VALUES (1, 'L', 'C:/L')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO track (library_id, rel_path, file_size, modified_at)
             VALUES (1, 'a.mp3', 1, '0')",
            [],
        )
        .unwrap();
        let page = list_tracks(&conn, "", Some(1), None, false, None, None, None, 10, 0).unwrap();
        assert_eq!(page.rows.len(), 1);
        assert!(page.rows[0].serato_grid_source.is_none());
    }

    #[test]
    fn tracks_needing_grid_skips_freshly_gridded_but_not_reanalyzed() {
        let (path, conn) = test_db("needing-grid.db");
        let _lib = ensure_library(&conn, "L", "C:/l").unwrap();
        let a = insert_track(&conn, "a.mp3", None, None);
        let b = insert_track(&conn, "b.mp3", None, None);
        let downs = [0.0f64; 10];
        for id in [a, b] {
            save_analysis(
                &conn,
                id,
                Some(120.0),
                None,
                None,
                None,
                &[1],
                None,
                None,
                Some(&downs),
                true,
            )
            .unwrap();
        }
        // a is gridded and has NOT been re-analyzed since
        stamp_serato_grid_written(&conn, a).unwrap();

        let need = tracks_needing_grid(&conn, &[a, b], false).unwrap();
        assert_eq!(need, vec![b]);

        let all = tracks_needing_grid(&conn, &[a, b], true).unwrap();
        assert_eq!(all, vec![a, b]);

        // re-analyze a after its grid write: analyzed_at now newer than the
        // grid stamp, so a needs a fresh grid again. datetime('now') has
        // 1s resolution, so force the ordering rather than race it.
        save_analysis(
            &conn, a, Some(120.0), None, None, None, &[1], None, None, Some(&downs), true,
        )
        .unwrap();
        conn.execute(
            "UPDATE track SET analyzed_at = datetime('now','+1 second') WHERE id = ?1",
            params![a],
        )
        .unwrap();
        let reanalyzed = tracks_needing_grid(&conn, &[a, b], false).unwrap();
        assert_eq!(reanalyzed, vec![a, b]);

        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    /// A file tag can carry a musical key ("Abm", "G#m", "Fmin"), which the
    /// scanner stores in key_camelot for want of anywhere else. It is NOT a
    /// Camelot code, so the user still needs analysis to produce one — but a
    /// non-NULL key made "Analyze → missing" skip the track entirely and made
    /// save_analysis refuse to replace the value. The track was therefore
    /// unanalyzable forever, which reads as "analysis stopped working".
    #[test]
    fn a_musical_key_from_a_tag_does_not_block_analysis() {
        let (path, conn) = test_db("tag-key-block.db");
        let lib = ensure_library(&conn, "lib", "C:/music").unwrap();
        let tagged = insert_track(&conn, "tagged.mp3", Some(120.0), Some("Abm"));
        let analyzed = insert_track(&conn, "analyzed.mp3", Some(120.0), Some("8A"));
        let manual = insert_track(&conn, "manual.mp3", Some(120.0), Some("Gm"));
        conn.execute(
            "UPDATE track SET key_source = 'analysis' WHERE id = ?1",
            params![analyzed],
        )
        .unwrap();
        conn.execute("UPDATE track SET key_source = 'manual' WHERE id = ?1", params![manual])
            .unwrap();

        let queued: Vec<i64> = tracks_for_analysis(&conn, None, Some(lib), true)
            .unwrap()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert!(queued.contains(&tagged), "a tag's musical key must not hide the track");
        assert!(!queued.contains(&analyzed), "a real Camelot key is not missing");
        assert!(!queued.contains(&manual), "a manual key is user ground truth");

        // …and the analysis result must actually land, without overwrite.
        save_analysis(&conn, tagged, None, Some("1A"), Some(0.9), None, &[1], None, None, None, false)
            .unwrap();
        let (k, src): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT key_camelot, key_source FROM track WHERE id = ?1",
                params![tagged],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(k.as_deref(), Some("1A"), "analysis must replace the tag's key");
        assert_eq!(src.as_deref(), Some("analysis"));

        // a manual musical key stays locked even when analysis runs on it
        save_analysis(&conn, manual, None, Some("6A"), Some(0.9), None, &[1], None, None, None, false)
            .unwrap();
        let mk: Option<String> = conn
            .query_row("SELECT key_camelot FROM track WHERE id = ?1", params![manual], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(mk.as_deref(), Some("Gm"), "manual keys are never touched");

        // an analysis-produced Camelot key still needs re-analyze to change
        save_analysis(&conn, analyzed, None, Some("3A"), Some(0.9), None, &[1], None, None, None, false)
            .unwrap();
        let ak: Option<String> = conn
            .query_row("SELECT key_camelot FROM track WHERE id = ?1", params![analyzed], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(ak.as_deref(), Some("8A"), "fills NULLs only, unless re-analyze");

        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    /// A dropped-in or untagged file has NULL tags, so it used to be
    /// invisible to search — FTS indexed tag text only. rel_path (filename
    /// and crate folders) must be searchable like any tag field.
    #[test]
    fn search_matches_filename_and_folder() {
        let (path, conn) = test_db("fts-relpath.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let id = insert_track(&conn, "Imports/XyzUnique999.mp3", None, None); // all tags NULL

        let hits = |q: &str| {
            list_tracks(&conn, q, None, None, false, None, None, None, 50, 0)
                .unwrap()
                .rows
                .len()
        };
        assert_eq!(hits("XyzUnique999"), 1, "filename matches");
        assert_eq!(hits("XyzUn"), 1, "filename prefix matches");
        assert_eq!(hits("Imports"), 1, "crate folder name matches");
        assert_eq!(hits("Nothing404"), 0);

        // a move/rename keeps the index in sync via the UPDATE trigger
        conn.execute(
            "UPDATE track SET rel_path = 'Other/Renamed888.mp3' WHERE id = ?1",
            params![id],
        )
        .unwrap();
        assert_eq!(hits("Renamed888"), 1, "new name found");
        assert_eq!(hits("XyzUnique999"), 0, "old name gone");

        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    /// A database created with the old six-column FTS shape must come out of
    /// `open()` with rel_path searchable — the v7 migration drops, recreates
    /// and rebuilds the index from existing rows.
    #[test]
    fn migration_v7_rebuilds_fts_with_rel_path() {
        let dir = std::env::temp_dir().join(format!("spindex-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fts-migrate.db");
        let _ = std::fs::remove_file(&path);

        {
            // hand-build a pre-v7 database: old FTS shape, one indexed track
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE library (id INTEGER PRIMARY KEY, name TEXT NOT NULL,
                     root_path TEXT NOT NULL UNIQUE, is_removable INTEGER NOT NULL DEFAULT 0,
                     last_scanned_at TEXT, volume_serial TEXT);
                 CREATE TABLE track (id INTEGER PRIMARY KEY,
                     library_id INTEGER NOT NULL REFERENCES library(id) ON DELETE CASCADE,
                     rel_path TEXT NOT NULL, file_size INTEGER NOT NULL, modified_at TEXT NOT NULL,
                     sha256 TEXT, title TEXT, artist TEXT, album TEXT, genre TEXT, label TEXT,
                     year INTEGER, bpm REAL, key_camelot TEXT, rating INTEGER, comment TEXT,
                     duration_secs REAL, bitrate_kbps INTEGER, sample_rate INTEGER, format TEXT,
                     has_artwork INTEGER NOT NULL DEFAULT 0, is_missing INTEGER NOT NULL DEFAULT 0,
                     waveform_peaks BLOB, analyzed_at TEXT, analysis_error TEXT,
                     added_at INTEGER, key_source TEXT, key_confidence REAL, key_candidates TEXT,
                     UNIQUE(library_id, rel_path));
                 CREATE VIRTUAL TABLE track_fts USING fts5(
                     title, artist, album, genre, label, comment,
                     content='track', content_rowid='id');
                 INSERT INTO library (name, root_path) VALUES ('lib', 'C:/old');
                 INSERT INTO track (library_id, rel_path, file_size, modified_at, title)
                     VALUES (1, 'Crate/OldFile777.mp3', 1, 'now', 'Old Title');
                 INSERT INTO track_fts(rowid, title) VALUES (1, 'Old Title');
                 PRAGMA user_version = 6;",
            )
            .unwrap();
        }

        let conn = open(&path).unwrap();
        let hits = |q: &str| {
            list_tracks(&conn, q, None, None, false, None, None, None, 50, 0)
                .unwrap()
                .rows
                .len()
        };
        assert_eq!(hits("OldFile777"), 1, "filename searchable after migration");
        assert_eq!(hits("Old Title"), 1, "tag text still searchable");

        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn crate_folders_derive_from_rel_paths() {
        let (path, conn) = test_db("crates.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        insert_track(&conn, "loose.mp3", None, None); // root file: no crate
        insert_track(&conn, "House/a.mp3", None, None);
        insert_track(&conn, "House/b.mp3", None, None);
        insert_track(&conn, "House/Deep House/c.mp3", None, None);
        // intermediate folder with no direct tracks, only a nested child
        insert_track(&conn, "Techno/Hard/d.mp3", None, None);
        // sibling that byte-sorts between "House" and "House/…" (' ' < '/')
        insert_track(&conn, "House Party/e.mp3", None, None);
        // missing tracks don't count
        let gone = insert_track(&conn, "House/gone.mp3", None, None);
        conn.execute("UPDATE track SET is_missing = 1 WHERE id = ?1", params![gone])
            .unwrap();

        let crates = crate_folders(&conn, 1).unwrap();
        let flat: Vec<(String, usize, i64)> = crates
            .iter()
            .map(|c| (c.path.clone(), c.depth, c.track_count))
            .collect();
        assert_eq!(
            flat,
            vec![
                ("House".into(), 0, 2),
                ("House/Deep House".into(), 1, 1),
                ("House Party".into(), 0, 1),
                ("Techno".into(), 0, 0),
                ("Techno/Hard".into(), 1, 1),
            ],
            "hierarchical order, direct counts only, intermediates at 0, missing excluded"
        );
        assert_eq!(crates[1].name, "Deep House");

        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rename_crate_paths_rewrites_exact_prefix_only() {
        let (path, conn) = test_db("renamecrate.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let a = insert_track(&conn, "House/a.mp3", None, None);
        let b = insert_track(&conn, "House/Deep/b.mp3", None, None);
        let c = insert_track(&conn, "House2/c.mp3", None, None); // sibling, must not change
        let d = insert_track(&conn, "loose.mp3", None, None);

        let n = rename_crate_paths(&conn, 1, "House", "Club").unwrap();
        assert_eq!(n, 2, "direct + nested rows rewritten");
        let rel = |id: i64| -> String {
            conn.query_row("SELECT rel_path FROM track WHERE id = ?1", params![id], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(rel(a), "Club/a.mp3");
        assert_eq!(rel(b), "Club/Deep/b.mp3");
        assert_eq!(rel(c), "House2/c.mp3", "sibling prefix untouched");
        assert_eq!(rel(d), "loose.mp3");

        // nested crate rename works the same way
        rename_crate_paths(&conn, 1, "Club/Deep", "Club/Deeper").unwrap();
        assert_eq!(rel(b), "Club/Deeper/b.mp3");

        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn crate_folders_union_disk_folders() {
        let dir = std::env::temp_dir().join(format!("spindex-crateunion-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Empty Crate")).unwrap();
        std::fs::create_dir_all(dir.join("House").join("Deep")).unwrap();

        let (db_path, conn) = test_db("crateunion.db");
        // forward slashes: library roots are stored clean, but join() output
        // on Windows mixes separators — use the dir as-is like add_library does
        ensure_library(&conn, "lib", dir.to_str().unwrap()).unwrap();
        insert_track(&conn, "House/a.mp3", None, None);

        let crates = crate_folders(&conn, 1).unwrap();
        let flat: Vec<(&str, i64)> =
            crates.iter().map(|c| (c.path.as_str(), c.track_count)).collect();
        assert_eq!(
            flat,
            vec![("Empty Crate", 0), ("House", 1), ("House/Deep", 0)],
            "disk-only folders (empty crates) join the track-derived tree"
        );

        drop(conn);
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_spellings_cover_camelot_and_musical_names() {
        let s = key_spellings("1A");
        for want in ["1a", "01a", "abm", "g#m", "abmin", "g#minor", "a♭m"] {
            assert!(s.contains(&want.to_string()), "1A misses {want}: {s:?}");
        }
        let s = key_spellings("8B");
        for want in ["8b", "c", "cmaj", "cmajor"] {
            assert!(s.contains(&want.to_string()), "8B misses {want}: {s:?}");
        }
        assert!(!s.contains(&"cm".to_string()), "a major key never matches a minor name");
        assert!(key_spellings("13A").is_empty());
        assert!(key_spellings("é").is_empty());
        assert!(key_spellings("").is_empty());
    }

    #[test]
    fn list_tracks_key_filter_matches_codes_and_tag_spellings() {
        let (path, conn) = test_db("keyfilter.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let a = insert_track(&conn, "a.mp3", None, Some("8A"));
        let b = insert_track(&conn, "b.mp3", None, Some("A minor"));
        let c = insert_track(&conn, "c.mp3", None, Some("12B"));
        insert_track(&conn, "d.mp3", None, Some("8B"));
        insert_track(&conn, "e.mp3", None, None);

        let ids = |keys: &[&str]| -> Vec<i64> {
            let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
            let mut v: Vec<i64> = list_tracks_keyed(
                &conn, "", None, None, false, None, None, None, Some(&keys), 50, 0,
            )
            .unwrap()
            .rows
            .iter()
            .map(|t| t.id)
            .collect();
            v.sort();
            v
        };
        assert_eq!(ids(&["8A"]), vec![a, b]);
        assert_eq!(ids(&["8A", "12B"]), vec![a, b, c]);
        let all = list_tracks_keyed(&conn, "", None, None, false, None, None, None, Some(&[]), 50, 0)
            .unwrap();
        assert_eq!(all.total, 5, "an empty key list is no filter");
        drop(conn);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn list_tracks_folder_filter_direct_children_only() {
        let (path, conn) = test_db("folderfilter.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        insert_track(&conn, "loose.mp3", None, None);
        let a = insert_track(&conn, "House/a.mp3", None, None);
        let b = insert_track(&conn, "House/b.mp3", None, None);
        insert_track(&conn, "House/Deep House/c.mp3", None, None);
        // LIKE wildcards in the folder name must match literally
        let w = insert_track(&conn, "100% House/w.mp3", None, None);
        insert_track(&conn, "100X House/x.mp3", None, None);
        conn.execute("UPDATE track SET title = 'Deep Cut' WHERE id = ?1", params![b])
            .unwrap();

        let ids = |page: TrackPage| -> Vec<i64> { page.rows.iter().map(|t| t.id).collect() };

        let page =
            list_tracks(&conn, "", Some(1), None, false, None, Some("House"), None, 100, 0).unwrap();
        assert_eq!(ids(page), vec![a, b], "direct children only, no nested, no other folders");

        let page = list_tracks(&conn, "", Some(1), None, false, None, Some("100% House"), None, 100, 0)
            .unwrap();
        assert_eq!(ids(page), vec![w], "% in folder name matches literally, not as wildcard");

        let page = list_tracks(&conn, "", Some(1), None, false, None, Some(""), None, 100, 0).unwrap();
        assert_eq!(page.rows[0].rel_path, "loose.mp3", "empty folder = library root");
        assert_eq!(page.total, 1);

        let page = list_tracks(&conn, "deep", Some(1), None, false, None, Some("House"), None, 100, 0)
            .unwrap();
        assert_eq!(ids(page), vec![b], "folder filter composes with FTS search");

        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_analysis_fills_nulls_and_respects_overwrite() {
        let (path, conn) = test_db("analysis.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let id = insert_track(&conn, "a.mp3", Some(128.0), None);

        save_analysis(&conn, id, Some(90.0), Some("8A"), Some(0.9), None, &[1, 2, 3], None, None, None, false).unwrap();
        let (bpm, key): (f64, String) = conn
            .query_row(
                "SELECT bpm, key_camelot FROM track WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(bpm, 128.0, "existing bpm kept without overwrite");
        assert_eq!(key, "8A", "null key filled");

        save_analysis(&conn, id, Some(90.0), Some("9B"), Some(0.8), None, &[1], None, None, None, true).unwrap();
        let (bpm, key): (f64, String) = conn
            .query_row(
                "SELECT bpm, key_camelot FROM track WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(bpm, 90.0, "overwrite replaces bpm");
        assert_eq!(key, "9B");

        assert_eq!(get_waveform(&conn, id).unwrap(), Some(vec![1]));
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn waveform_bands_roundtrip_and_v10() {
        let (path, conn) = test_db("waveform_bands.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let track_id = insert_track(&conn, "a.mp3", None, None);
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert!(version >= 10);

        let bands: Vec<u8> = (0..1200).map(|i| (i % 256) as u8).collect();
        save_analysis(&conn, track_id, Some(120.0), Some("8A"), Some(0.9), None, &[1, 2, 3], Some(&bands), None, None, false)
            .unwrap();
        assert_eq!(waveform_bands(&conn, track_id).unwrap().as_deref(), Some(&bands[..]));

        // sidecar without bands -> NULL, not an error
        save_analysis(&conn, track_id, Some(120.0), Some("8A"), Some(0.9), None, &[1, 2, 3], None, None, None, false)
            .unwrap();
        assert_eq!(waveform_bands(&conn, track_id).unwrap(), None);

        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn beat_grid_roundtrip_and_drift_derivation_v12() {
        let (path, conn) = test_db("beat_grid.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let track_id = insert_track(&conn, "a.mp3", None, None);

        // A rock-steady 120 BPM grid: stored verbatim, drift ~0.
        let steady: Vec<f64> = (0..64).map(|i| i as f64 * 0.5).collect();
        save_analysis(
            &conn, track_id, Some(120.0), None, None, None, &[1, 2], None, Some(&steady), None, true,
        )
        .unwrap();

        let back = beat_grid(&conn, track_id).unwrap().expect("grid stored");
        assert_eq!(back.len(), steady.len());
        assert!((back[10] - steady[10]).abs() < 1e-3, "beat times survive the blob");

        let drift: Option<f64> = conn
            .query_row("SELECT tempo_drift_pct FROM track WHERE id = ?1", params![track_id], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(drift.expect("drift derived") < 0.05, "steady grid drifts ~0");

        // No grid at all -> NULL drift, no stale value left behind.
        save_analysis(&conn, track_id, Some(120.0), None, None, None, &[1, 2], None, None, None, true)
            .unwrap();
        let cleared: Option<f64> = conn
            .query_row("SELECT tempo_drift_pct FROM track WHERE id = ?1", params![track_id], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(cleared.is_none(), "an old sidecar clears drift rather than lying");

        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn downbeat_grid_roundtrip_and_v13() {
        let (path, conn) = test_db("downbeat_grid.db");
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 14);
        let _lib = ensure_library(&conn, "L", "C:/l").unwrap();
        let track_id = insert_track(&conn, "a.mp3", None, None);
        let downs = [0.51f64, 2.53, 4.55, 6.57];
        save_analysis(
            &conn,
            track_id,
            Some(120.0),
            None,
            None,
            None,
            &[1u8],
            None,
            None,
            Some(&downs),
            true,
        )
        .unwrap();
        let got = downbeat_grid(&conn, track_id).unwrap().unwrap();
        assert_eq!(got.len(), 4);
        assert!((got[1] - 2.53).abs() < 1e-3);
        // an old sidecar (no downbeats) clears the column
        save_analysis(
            &conn,
            track_id,
            Some(120.0),
            None,
            None,
            None,
            &[1u8],
            None,
            None,
            None,
            true,
        )
        .unwrap();
        assert_eq!(downbeat_grid(&conn, track_id).unwrap(), None);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn pre_v13_database_migrates_and_lists_tracks() {
        // Build a v12 DB by dropping the v13 columns (bundled SQLite accepts DROP COLUMN),
        // then reopen through migrate() and run a folder-crate list.
        let dir = std::env::temp_dir().join("spindex-test-v13-migrate");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("v12.db");
        let _ = std::fs::remove_file(&path);
        {
            let conn = open(&path).unwrap();
            conn.execute_batch(
                "ALTER TABLE track DROP COLUMN downbeat_grid;
                 ALTER TABLE track DROP COLUMN serato_grid_source;
             ALTER TABLE track DROP COLUMN serato_grid_written_at;
                 PRAGMA user_version = 12;",
            )
            .unwrap();
        }
        let conn = open(&path).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 14);
        let lib = ensure_library(&conn, "L", "C:/l").unwrap();
        let _ = list_tracks(&conn, "", Some(lib), None, false, None, Some(""), None, 10, 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unsteady_tempo_health_counts_and_filters() {
        let (path, conn) = test_db("unsteady.db");
        let lib = ensure_library(&conn, "lib", "C:/music").unwrap();
        insert_track(&conn, "steady.mp3", None, None);
        insert_track(&conn, "wobbly.mp3", None, None);
        conn.execute(
            "UPDATE track SET analyzed_at = datetime('now'),
                 tempo_drift_pct = CASE WHEN rel_path = 'wobbly.mp3' THEN 4.0 ELSE 0.2 END",
            [],
        )
        .unwrap();

        let health = library_health(&conn, Some(lib)).unwrap();
        assert_eq!(health.unsteady_tempo, 1, "only the wobbly track counts");

        let page = list_tracks(
            &conn,
            "",
            Some(lib),
            None,
            false,
            Some("unsteady_tempo"),
            None,
            None,
            50,
            0,
        )
        .unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].rel_path, "wobbly.mp3");

        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tracks_for_analysis_filters_missing_metadata() {
        let (path, conn) = test_db("queue.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let a = insert_track(&conn, "a.mp3", None, None);
        let b = insert_track(&conn, "b.mp3", Some(120.0), Some("8A"));
        let c = insert_track(&conn, "c.mp3", Some(120.0), None);

        let missing = tracks_for_analysis(&conn, None, None, true).unwrap();
        let ids: Vec<i64> = missing.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![a, c], "complete tracks are skipped");
        assert!(missing[0].1.ends_with("a.mp3"));
        assert!(missing[0].1.starts_with("C:/music"));

        let all = tracks_for_analysis(&conn, None, None, false).unwrap();
        assert_eq!(all.len(), 3);

        let picked = tracks_for_analysis(&conn, Some(&[b]), None, false).unwrap();
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].0, b);

        save_analysis_error(&conn, a, "boom").unwrap();
        let err: String = conn
            .query_row(
                "SELECT analysis_error FROM track WHERE id = ?1",
                params![a],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(err, "boom");
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn migration_adds_analysis_columns() {
        let dir = std::env::temp_dir().join(format!("spindex-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mig.db");
        let _ = std::fs::remove_file(&path);
        {
            let conn = open(&path).unwrap();
            conn.execute(
                "UPDATE track SET waveform_peaks = x'00', analyzed_at = '2026', analysis_error = NULL,
                                  key_source = 'analysis', key_confidence = 0.5",
                [],
            )
            .unwrap();
            conn.execute("UPDATE track SET key_candidates = '[]'", []).unwrap();
            conn.execute("UPDATE library SET volume_serial = '1A2B3C4D'", [])
                .unwrap();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, 14, "migrate() now runs through v14 in one pass");
        }
        // reopening an already-migrated db must not fail
        open(&path).unwrap();
        let _ = std::fs::remove_file(&path);
    }

    /// A removable library is the physical stick (volume serial), not the
    /// drive letter: a different stick on the same letter must not match, a
    /// known stick on a new letter must.
    #[test]
    fn removable_identity_by_volume_serial() {
        let dir = std::env::temp_dir().join(format!("spindex-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("serial.db");
        let _ = std::fs::remove_file(&path);
        let conn = open(&path).unwrap();

        let id = ensure_library(&conn, "CK MUSE (D:)", "D:\\").unwrap();
        set_library_removable(&conn, id, Some("1A2B3C4D")).unwrap();

        assert_eq!(
            removable_libraries_for_serial(&conn, "1A2B3C4D").unwrap(),
            vec![id],
            "known stick matches by serial"
        );
        assert!(
            removable_libraries_for_serial(&conn, "FFFF0000")
                .unwrap()
                .is_empty(),
            "different stick on the same letter is unknown"
        );

        let second = ensure_library(&conn, "MP3 (CK MUSE)", "D:\\MP3\\").unwrap();
        set_library_removable(&conn, second, Some("1A2B3C4D")).unwrap();
        let mut both = removable_libraries_for_serial(&conn, "1A2B3C4D").unwrap();
        both.sort();
        assert_eq!(both, vec![id, second], "one stick, many libraries");

        // Known stick reappears under a new letter: root moves, ids survive.
        update_library_root(&conn, id, "E:\\").unwrap();
        let root: String = conn
            .query_row("SELECT root_path FROM library WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(root, "E:\\");

        // A new stick claiming a letter whose previous (different) stick
        // still owns the root: the old row is parked so the new stick gets
        // a fresh library instead of inheriting the old one's row.
        let old = ensure_library(&conn, "GONE (G:)", "G:\\").unwrap();
        set_library_removable(&conn, old, None).unwrap();
        park_conflicting_removable(&conn, "G:\\", Some("1A2B3C4D")).unwrap();
        let fresh = ensure_library(&conn, "NEW STICK (G:)", "G:\\").unwrap();
        assert_ne!(fresh, old, "new stick must not inherit the old library row");
        let parked_root: String = conn
            .query_row("SELECT root_path FROM library WHERE id = ?1", [old], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(parked_root.starts_with("missing://"), "old row parked: {parked_root}");

        // a parked folder root un-parks with its suffix when its stick returns
        let folder_lib = ensure_library(&conn, "MP3 (GONE)", "H:\\MP3\\").unwrap();
        set_library_removable(&conn, folder_lib, Some("DEAD0001")).unwrap();
        park_conflicting_removable(&conn, "H:\\MP3\\", Some("OTHER111")).unwrap();
        let parked: String = conn
            .query_row("SELECT root_path FROM library WHERE id = ?1", [folder_lib], |r| r.get(0))
            .unwrap();
        assert_eq!(parked, format!("missing://{folder_lib}\\MP3\\"));
        let restored = crate::core::devices::swap_drive_prefix(&parked, "K:");
        assert_eq!(restored, "K:\\MP3\\");
        update_library_root(&conn, folder_lib, &restored).unwrap();

        // parking the same stick (matching serial) must NOT happen
        set_library_removable(&conn, fresh, Some("1A2B3C4D")).unwrap();
        park_conflicting_removable(&conn, "G:\\", Some("1A2B3C4D")).unwrap();
        let same_root: String = conn
            .query_row("SELECT root_path FROM library WHERE id = ?1", [fresh], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(same_root, "G:\\", "matching serial keeps its root");

        // Legacy rows without a serial never claim a connecting stick.
        let legacy = ensure_library(&conn, "OLD STICK (F:)", "F:\\").unwrap();
        set_library_removable(&conn, legacy, None).unwrap();
        assert!(
            removable_libraries_for_serial(&conn, "ABCD9999")
                .unwrap()
                .is_empty()
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sort_whitelist_and_ordering() {
        assert_eq!(sort_expr("bpm"), Some("t.bpm"));
        assert_eq!(sort_expr("added"), Some("t.added_at"));
        assert_eq!(sort_expr("id; DROP TABLE track"), None, "injection-proof");

        let (path, conn) = test_db("sort.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        insert_track(&conn, "slow.mp3", Some(90.0), None);
        insert_track(&conn, "fast.mp3", Some(174.0), None);
        insert_track(&conn, "untagged.mp3", None, None);

        let page = list_tracks(&conn, "", None, Some("bpm"), false, None, None, None, 100, 0).unwrap();
        let bpms: Vec<Option<f64>> = page.rows.iter().map(|t| t.bpm).collect();
        assert_eq!(bpms, vec![Some(90.0), Some(174.0), None], "asc, nulls last");

        let page = list_tracks(&conn, "", None, Some("bpm"), true, None, None, None, 100, 0).unwrap();
        let bpms: Vec<Option<f64>> = page.rows.iter().map(|t| t.bpm).collect();
        assert_eq!(bpms, vec![Some(174.0), Some(90.0), None], "desc, nulls last");

        // Camelot keys sort by wheel position, not lexicographically
        insert_track(&conn, "k1.mp3", None, Some("11A"));
        insert_track(&conn, "k2.mp3", None, Some("1A"));
        insert_track(&conn, "k3.mp3", None, Some("8A"));
        insert_track(&conn, "k4.mp3", None, Some("8B"));
        insert_track(&conn, "k5.mp3", None, Some("2B"));
        let page = list_tracks(&conn, "", None, Some("key"), false, None, None, None, 100, 0).unwrap();
        let keys: Vec<Option<String>> = page
            .rows
            .iter()
            .map(|t| t.key_camelot.clone())
            .filter(|k| k.is_some())
            .collect();
        assert_eq!(
            keys,
            vec![
                Some("1A".into()),
                Some("2B".into()),
                Some("8A".into()),
                Some("8B".into()),
                Some("11A".into()),
            ],
            "camelot wheel order, letter after number"
        );

        // the real scanner path (upsert_track) stamps added_at
        let meta = crate::core::metadata::TrackMeta::default();
        upsert_track(&conn, 1, "up.mp3", 1, "1234", &meta).unwrap();
        let added: Option<i64> = conn
            .query_row(
                "SELECT added_at FROM track WHERE rel_path = 'up.mp3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(added.is_some(), "upsert sets added_at");
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn manual_key_survives_analysis_overwrite() {
        let (path, conn) = test_db("manual.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let id = insert_track(&conn, "a.mp3", None, Some("10A"));

        // user corrects the key -> manual
        let update = crate::core::metadata::TagUpdate {
            title: None,
            artist: None,
            album: None,
            genre: None,
            year: None,
            bpm: None,
            key: Some("2A".into()),
            comment: None,
            label: None,
            rating: None,
        };
        apply_metadata_update(&conn, id, &update, 1, "now").unwrap();
        let (key, source): (String, String) = conn
            .query_row(
                "SELECT key_camelot, key_source FROM track WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((key.as_str(), source.as_str()), ("2A", "manual"));

        // overwrite analysis must not touch it, but must update bpm
        save_analysis(&conn, id, Some(120.0), Some("10A"), Some(0.9), Some("[{\"key\":\"10A\",\"p\":0.9}]"), &[1], None, None, None, true).unwrap();
        let (key, source, bpm): (String, String, f64) = conn
            .query_row(
                "SELECT key_camelot, key_source, bpm FROM track WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((key.as_str(), source.as_str()), ("2A", "manual"));
        assert_eq!(bpm, 120.0);

        // saving unrelated fields with the same key keeps the lock
        let same = crate::core::metadata::TagUpdate {
            genre: Some("House".into()),
            key: Some("2A".into()),
            ..update
        };
        apply_metadata_update(&conn, id, &same, 1, "now").unwrap();
        let source: String = conn
            .query_row("SELECT key_source FROM track WHERE id = ?1", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(source, "manual");

        // clearing the key unlocks; the next analysis fills it again
        let clear = crate::core::metadata::TagUpdate { key: None, ..same };
        apply_metadata_update(&conn, id, &clear, 1, "now").unwrap();
        let source: Option<String> = conn
            .query_row("SELECT key_source FROM track WHERE id = ?1", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(source, None);
        save_analysis(&conn, id, None, Some("10A"), Some(0.8), Some("[{\"key\":\"10A\",\"p\":0.8}]"), &[1], None, None, None, false).unwrap();
        let (key, source, conf): (String, String, f64) = conn
            .query_row(
                "SELECT key_camelot, key_source, key_confidence FROM track WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((key.as_str(), source.as_str()), ("10A", "analysis"));
        assert_eq!(conf, 0.8);
        let cands: String = conn
            .query_row("SELECT key_candidates FROM track WHERE id = ?1", params![id], |r| r.get(0))
            .unwrap();
        assert!(cands.contains("10A"), "candidates stored on unlocked write");
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hashing_queries() {
        let (path, conn) = test_db("hash.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let a = insert_track(&conn, "a.mp3", None, None);
        let b = insert_track(&conn, "b.mp3", None, None);
        conn.execute("UPDATE track SET is_missing = 1 WHERE id = ?1", params![b])
            .unwrap();

        let todo = tracks_for_hashing(&conn, Some(&[1])).unwrap();
        assert_eq!(todo.len(), 1, "missing tracks excluded");
        assert_eq!(todo[0].0, a);
        assert!(todo[0].1.starts_with("C:/music"));

        save_sha256(&conn, a, "abc123").unwrap();
        assert!(tracks_for_hashing(&conn, None).unwrap().is_empty(), "hashed tracks skipped");
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn apply_partial_updates_only_listed_columns() {
        let (path, conn) = test_db("partialmeta.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let id = insert_track(&conn, "a.mp3", Some(120.0), Some("8A"));
        conn.execute(
            "UPDATE track SET title='T', artist='A', genre='Old', key_source='manual' WHERE id=?1",
            params![id],
        )
        .unwrap();

        let update = crate::core::metadata::TagUpdate {
            genre: Some("House".into()),
            title: Some("IGNORED".into()),
            artist: None, album: None, year: None, bpm: None, key: Some("2A".into()), comment: None, label: None,
            rating: None,
        };
        // apply ONLY genre — title/artist/key columns must be untouched
        apply_partial_metadata(&conn, id, &update, &["genre".to_string()], 42, "now").unwrap();

        let (title, artist, genre, key, ksrc): (String, String, String, String, String) = conn
            .query_row(
                "SELECT title, artist, genre, key_camelot, key_source FROM track WHERE id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(genre, "House", "genre updated");
        assert_eq!(title, "T", "title untouched");
        assert_eq!(artist, "A", "artist untouched");
        assert_eq!(key, "8A", "key untouched (not applied)");
        assert_eq!(ksrc, "manual", "key_source untouched (not applied)");

        // now apply the key — manual-key rule fires (key changes → source manual)
        apply_partial_metadata(&conn, id, &update, &["key".to_string()], 42, "now").unwrap();
        let (key, ksrc): (String, String) = conn
            .query_row("SELECT key_camelot, key_source FROM track WHERE id=?1", params![id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((key.as_str(), ksrc.as_str()), ("2A", "manual"));
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn delete_track_rows_removes_only_given_ids_and_fts() {
        let (path, conn) = test_db("delete.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let a = insert_track(&conn, "a.mp3", None, None);
        let b = insert_track(&conn, "b.mp3", None, None);
        conn.execute(
            "UPDATE track SET title = 'FindMeUnique' WHERE id = ?1",
            params![a],
        )
        .unwrap();

        let n = delete_track_rows(&conn, &[a]).unwrap();
        assert_eq!(n, 1);

        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM track", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 1, "only the given id is deleted");
        let b_still: i64 = conn
            .query_row("SELECT COUNT(*) FROM track WHERE id = ?1", params![b], |r| r.get(0))
            .unwrap();
        assert_eq!(b_still, 1);

        // FTS trigger cleaned the index row too
        let fts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM track_fts WHERE track_fts MATCH '\"FindMeUnique\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts, 0, "FTS entry gone with the row");

        // empty id list is a no-op
        assert_eq!(delete_track_rows(&conn, &[]).unwrap(), 0);
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    /// Reverse of mark_missing_paths — a track whose file has reappeared
    /// (restored, moved back) clears the flag. Scoped to library_id + exact
    /// rel_path, and guarded by `is_missing = 1` so an already-present row is
    /// untouched and the count only reflects rows that actually flipped.
    #[test]
    fn clear_missing_paths_unflags_only_the_given_already_missing_rows() {
        let (path, conn) = test_db("clear_missing.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let a = insert_track(&conn, "a.mp3", None, None);
        let b = insert_track(&conn, "b.mp3", None, None);
        conn.execute("UPDATE track SET is_missing = 1 WHERE id IN (?1, ?2)", params![a, b])
            .unwrap();

        let n = clear_missing_paths(&conn, 1, &["a.mp3".to_string()]).unwrap();
        assert_eq!(n, 1);

        let a_missing: i64 = conn
            .query_row("SELECT is_missing FROM track WHERE id = ?1", params![a], |r| r.get(0))
            .unwrap();
        assert_eq!(a_missing, 0, "a.mp3 is unflagged");
        let b_missing: i64 = conn
            .query_row("SELECT is_missing FROM track WHERE id = ?1", params![b], |r| r.get(0))
            .unwrap();
        assert_eq!(b_missing, 1, "b.mp3 was not named, so it stays flagged");

        // already-present rows are not re-counted
        assert_eq!(clear_missing_paths(&conn, 1, &["a.mp3".to_string()]).unwrap(), 0);
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn health_counts_and_filter() {
        let (path, conn) = test_db("health.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let complete = insert_track(&conn, "ok.mp3", Some(120.0), Some("8A"));
        conn.execute(
            "UPDATE track SET has_artwork = 1 WHERE id = ?1",
            params![complete],
        )
        .unwrap();
        insert_track(&conn, "nobpm.mp3", None, Some("9B"));
        let gone = insert_track(&conn, "gone.mp3", Some(100.0), Some("1A"));
        mark_track_missing(&conn, gone).unwrap();
        let broken = insert_track(&conn, "broken.mp3", None, None);
        save_analysis_error(&conn, broken, "decode failed").unwrap();

        let h = library_health(&conn, Some(1)).unwrap();
        assert_eq!(h.missing_bpm, 2, "nobpm + broken, missing file not counted");
        assert_eq!(h.missing_key, 1, "broken only");
        assert_eq!(h.missing_artwork, 2);
        assert_eq!(h.missing_file, 1);
        assert_eq!(h.analysis_error, 1);
        assert_eq!(h.duplicates, 0);

        let page = list_tracks(&conn, "", None, None, false, Some("missing_bpm"), None, None, 100, 0).unwrap();
        assert_eq!(page.total, 2);
        let page = list_tracks(&conn, "", None, None, false, Some("missing_file"), None, None, 100, 0).unwrap();
        assert_eq!(page.total, 1);
        assert!(page.rows[0].rel_path.ends_with("gone.mp3"));
        // unknown filter keys are ignored, never interpolated
        let page =
            list_tracks(&conn, "", None, None, false, Some("1=1; DROP TABLE track"), None, None, 100, 0)
            .unwrap();
        assert_eq!(page.total, 4);
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ensure_library_keeps_existing_name() {
        let (path, conn) = test_db("libname.db");
        let id = ensure_library(&conn, "KINGSTON (E:)", "E:\\").unwrap();
        let id2 = ensure_library(&conn, "E:\\", "E:\\").unwrap();
        assert_eq!(id, id2);
        let name: String = conn
            .query_row("SELECT name FROM library WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "KINGSTON (E:)", "rescan must not clobber the label");
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fts_query_builds_prefix_terms() {
        assert_eq!(fts_query("daft punk"), Some("\"daft\"* \"punk\"*".into()));
        assert_eq!(fts_query("  "), None);
        assert_eq!(fts_query("a\"b"), Some("\"ab\"*".into()));
    }

    #[test]
    fn migrates_to_v6_with_volume_serial_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(FTS_SCHEMA).unwrap();
        migrate(&conn).unwrap();

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 14, "migrate() now runs through v14 in one pass");

        // Column exists and defaults to NULL.
        conn.execute(
            "INSERT INTO library (name, root_path) VALUES ('S', 'E:\\')",
            [],
        )
        .unwrap();
        let serial: Option<String> = conn
            .query_row("SELECT volume_serial FROM library WHERE root_path='E:\\'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(serial, None);
    }

    #[test]
    fn migrates_to_v8_with_quality_columns() {
        let (path, conn) = test_db("qual_migrate.db");
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert!(v >= 9);
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let id = insert_track(&conn, "a.mp3", None, None);
        conn.execute(
            "UPDATE track SET loudness_lufs = -9.5, clip_ratio = 0.0,
             cutoff_hz = 21000, quality_score = 100 WHERE id = ?1",
            params![id],
        )
        .unwrap();
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    /// The live-bug scenario: a parallel branch shipped the FTS rebuild as v7,
    /// so real DBs exist at user_version 7 WITHOUT quality columns. Opening
    /// such a DB must still run the v8 quality migration instead of assuming
    /// "7 means quality columns exist" and failing every query.
    #[test]
    fn v7_fts_database_gains_quality_columns_via_v8() {
        let dir = std::env::temp_dir().join(format!("spindex-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v7_no_quality.db");
        let _ = std::fs::remove_file(&path);
        {
            // hand-build the foreign v7 state: full schema + FTS, version
            // pinned to 7, quality columns absent
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute_batch(FTS_SCHEMA).unwrap();
            conn.execute_batch(
                "ALTER TABLE track ADD COLUMN waveform_peaks BLOB;
                 ALTER TABLE track ADD COLUMN analyzed_at TEXT;
                 ALTER TABLE track ADD COLUMN analysis_error TEXT;
                 ALTER TABLE track ADD COLUMN added_at INTEGER;
                 ALTER TABLE track ADD COLUMN key_source TEXT;
                 ALTER TABLE track ADD COLUMN key_confidence REAL;
                 ALTER TABLE track ADD COLUMN key_candidates TEXT;
                 ALTER TABLE library ADD COLUMN volume_serial TEXT;
                 PRAGMA user_version = 7;",
            )
            .unwrap();
        }
        let conn = open(&path).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 14);
        // the startup-killing queries must work now
        conn.execute("UPDATE track SET quality_score = 50", []).unwrap();
        library_health(&conn, None).unwrap();
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn migrates_to_v9_with_enhanced_at() {
        let (path, conn) = test_db("enh_migrate.db");
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert!(v >= 9);
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let id = insert_track(&conn, "a.mp3", None, None);
        mark_enhanced(&conn, id).unwrap();
        let at: Option<String> = conn
            .query_row("SELECT enhanced_at FROM track WHERE id = ?1", params![id], |r| r.get(0))
            .unwrap();
        assert!(at.is_some());
        clear_enhanced(&conn, id).unwrap();
        let at: Option<String> = conn
            .query_row("SELECT enhanced_at FROM track WHERE id = ?1", params![id], |r| r.get(0))
            .unwrap();
        assert!(at.is_none());
        drop(conn);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn update_track_file_moves_rel_path_and_stamps() {
        let (path, conn) = test_db("enh_move.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let id = insert_track(&conn, "House/t.mp3", None, None);
        update_track_file(&conn, id, Some("House/t.flac"), Some("flac"), 999, "1234567").unwrap();
        let (rp, fmt, size): (String, String, i64) = conn
            .query_row("SELECT rel_path, format, file_size FROM track WHERE id = ?1",
                params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert_eq!(rp, "House/t.flac");
        assert_eq!(fmt, "flac");
        assert_eq!(size, 999);
        // stamps-only variant leaves rel_path/format alone
        update_track_file(&conn, id, None, None, 1000, "1234568").unwrap();
        let rp: String = conn
            .query_row("SELECT rel_path FROM track WHERE id = ?1", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(rp, "House/t.flac");
        drop(conn);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn save_quality_derives_score_from_row_format() {
        let (path, conn) = test_db("qual_save.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let id = insert_track(&conn, "a.mp3", None, None);
        // make the row a 128 kbps mp3
        conn.execute(
            "UPDATE track SET format = 'mp3', bitrate_kbps = 128 WHERE id = ?1",
            params![id],
        )
        .unwrap();
        save_quality(&conn, id, Some(-12.0), 0.0, Some(15_800), &[7u8; 4]).unwrap();
        let (lufs, score, blob): (f64, i64, Vec<u8>) = conn
            .query_row(
                "SELECT loudness_lufs, quality_score, spectrogram FROM track WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(lufs, -12.0);
        assert_eq!(score, 30, "cutoff 40-penalty + 128 kbps cap");
        assert_eq!(blob, vec![7u8; 4]);
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn quality_detail_returns_metrics_and_blob() {
        let (path, conn) = test_db("qual_detail.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let id = insert_track(&conn, "a.mp3", None, None);
        save_quality(&conn, id, Some(-8.0), 0.002, Some(20_500), &[1, 2, 3]).unwrap();
        let d = quality_detail(&conn, id).unwrap();
        assert_eq!(d.loudness_lufs, Some(-8.0));
        assert_eq!(d.cutoff_hz, Some(20_500));
        assert_eq!(d.spectrogram.as_deref(), Some(&[1u8, 2, 3][..]));
        assert!(d.quality_score.is_some());
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn low_quality_health_count_and_filter_and_sort() {
        let (path, conn) = test_db("qual_health.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let a = insert_track(&conn, "a.mp3", None, None);
        let b = insert_track(&conn, "b.mp3", None, None);
        conn.execute("UPDATE track SET quality_score = 30 WHERE id = ?1", params![a])
            .unwrap();
        conn.execute("UPDATE track SET quality_score = 90 WHERE id = ?1", params![b])
            .unwrap();
        let h = library_health(&conn, None).unwrap();
        assert_eq!(h.low_quality, 1, "only scores < 45, NULLs excluded");
        let page = list_tracks(&conn, "", None, None, false, Some("low_quality"), None, None, 100, 0)
            .unwrap();
        assert!(page.rows.iter().all(|t| t.quality_score.map_or(false, |s| s < 45)));
        assert_eq!(page.total, 1);
        // sort key accepted and orders by score
        let page = list_tracks(&conn, "", None, Some("quality"), false, None, None, None, 100, 0)
            .unwrap();
        assert!(page.total >= 2);
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    fn insert_genre_track(conn: &Connection, rel: &str, genre: Option<&str>, missing: bool) {
        conn.execute(
            "INSERT INTO track (library_id, rel_path, file_size, modified_at, genre, is_missing)
             VALUES (1, ?1, 1, 'now', ?2, ?3)",
            params![rel, genre, missing as i64],
        )
        .unwrap();
    }

    #[test]
    fn list_genres_dedupes_case_orders_by_count() {
        let (_path, conn) = test_db("genres.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        insert_genre_track(&conn, "a.mp3", Some("House"), false);
        insert_genre_track(&conn, "b.mp3", Some("House"), false);
        insert_genre_track(&conn, "c.mp3", Some("HOUSE"), false); // case-merged, "House" spelling wins
        insert_genre_track(&conn, "d.mp3", Some("Techno"), false);
        insert_genre_track(&conn, "e.mp3", Some(" Techno "), false); // trim-merged
        insert_genre_track(&conn, "f.mp3", Some("Disco"), true); // missing file: excluded
        insert_genre_track(&conn, "g.mp3", Some("   "), false); // blank: excluded
        insert_genre_track(&conn, "h.mp3", None, false);

        let genres = list_genres(&conn).unwrap();
        let pairs: Vec<(&str, i64)> = genres.iter().map(|g| (g.genre.as_str(), g.count)).collect();
        assert_eq!(pairs, vec![("House", 3), ("Techno", 2)]);
    }

    #[test]
    fn list_genres_spelling_tie_breaks_alphabetically() {
        let (_path, conn) = test_db("genres-tie.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        insert_genre_track(&conn, "a.mp3", Some("dnb"), false);
        insert_genre_track(&conn, "b.mp3", Some("DnB"), false); // 1-vs-1 tie: "DnB" < "dnb" byte-wise
        let genres = list_genres(&conn).unwrap();
        assert_eq!(genres.len(), 1);
        assert_eq!(genres[0].genre, "DnB");
        assert_eq!(genres[0].count, 2);
    }

    #[test]
    fn list_genres_empty_library() {
        let (_path, conn) = test_db("genres-empty.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        assert!(list_genres(&conn).unwrap().is_empty());
    }

    #[test]
    fn tracks_in_subtree_direct_nested_not_siblings() {
        let (path, conn) = test_db("subtree.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let _direct = insert_track(&conn, "House/Deep/a.mp3", None, None);
        let _nested = insert_track(&conn, "House/Deep/Vocal/b.mp3", None, None);
        let _outside = insert_track(&conn, "House/c.mp3", None, None);
        let _sibling = insert_track(&conn, "House Party/d.mp3", None, None);
        let gone = insert_track(&conn, "House/Deep/gone.mp3", None, None);
        mark_track_missing(&conn, gone).unwrap();

        let mut rels: Vec<String> = tracks_in_subtree(&conn, 1, "House/Deep")
            .unwrap()
            .into_iter()
            .map(|(_, rel)| rel)
            .collect();
        rels.sort();
        let mut expected = vec![
            "House/Deep/Vocal/b.mp3".to_string(),
            "House/Deep/a.mp3".to_string(),
        ];
        expected.sort();
        assert_eq!(rels, expected);

        // absolute source path joins the library root
        let (abs, _) = &tracks_in_subtree(&conn, 1, "House/Deep").unwrap()[0];
        assert!(abs.starts_with("C:/music"), "{}", abs.display());

        // % / _ in a folder name match literally
        insert_track(&conn, "100% House/e.mp3", None, None);
        let hits = tracks_in_subtree(&conn, 1, "100% House").unwrap();
        assert_eq!(hits.len(), 1);
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn num_sorts_by_crate_pos() {
        assert_eq!(sort_expr("num"), Some("t.crate_pos"));
    }

    #[test]
    fn crate_pos_defaults_to_null_and_round_trips() {
        let (path, conn) = test_db("crate-pos.db");
        ensure_library(&conn, "lib", "C:/music").unwrap();
        let id = insert_track(&conn, "House/a.mp3", None, None);

        let pos: Option<i64> = conn
            .query_row("SELECT crate_pos FROM track WHERE id = ?1", params![id], |r| r.get(0))
            .unwrap();
        assert!(pos.is_none(), "new rows start unpositioned");

        conn.execute("UPDATE track SET crate_pos = 7 WHERE id = ?1", params![id]).unwrap();
        let page =
            list_tracks(&conn, "", Some(1), Some("num"), false, None, None, None, 10, 0).unwrap();
        assert_eq!(page.rows[0].crate_pos, Some(7));
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    fn seed_crate_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        migrate(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO library (id, name, root_path) VALUES (1, 'L', 'C:/L'),
                                                              (2, 'M', 'C:/M');
             INSERT INTO track (id, library_id, rel_path, file_size, modified_at, added_at)
             VALUES (1, 1, 'House/a.mp3', 1, '0', 10),
                    (2, 1, 'House/b.mp3', 1, '0', 20),
                    (3, 1, 'House/Deep/c.mp3', 1, '0', 30),
                    (4, 1, 'loose.mp3', 1, '0', 40),
                    (5, 2, 'House/x.mp3', 1, '0', 50);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn an_id_set_lists_in_the_order_it_was_given() {
        let conn = seed_crate_db();
        // ids deliberately not in id order and not in artist/title order
        let page = list_tracks(&conn, "", None, None, false, None, None, Some(&[3, 1, 2]), 10, 0)
            .unwrap();
        assert_eq!(
            page.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![3, 1, 2],
            "an explicit id set is the crate's own order and must be preserved"
        );
    }

    #[test]
    fn an_explicit_sort_still_overrides_the_id_order() {
        let conn = seed_crate_db();
        // seed_crate_db leaves title NULL for every row, which would make a
        // title sort accidentally match [3, 1, 2] regardless of whether the
        // override actually took effect — give the rows distinguishable
        // titles so the assertion is meaningful.
        conn.execute_batch(
            "UPDATE track SET title = 'C' WHERE id = 1;
             UPDATE track SET title = 'B' WHERE id = 2;
             UPDATE track SET title = 'A' WHERE id = 3;",
        )
        .unwrap();
        let page =
            list_tracks(&conn, "", None, Some("title"), false, None, None, Some(&[3, 1, 2]), 10, 0)
                .unwrap();
        let ids: Vec<i64> = page.rows.iter().map(|r| r.id).collect();
        assert_ne!(ids, vec![3, 1, 2], "sorting by a column must win over the id order");
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn crate_track_files_returns_direct_children_only() {
        let conn = seed_crate_db();
        let rows = crate_track_files(&conn, 1, "House").unwrap();
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![1, 2], "nested, loose and other-library rows excluded");
        assert_eq!(rows[0].file_name, "a.mp3");
        assert_eq!(rows[0].added_at, Some(10));
    }

    #[test]
    fn crate_track_files_empty_folder_means_library_root() {
        let conn = seed_crate_db();
        let rows = crate_track_files(&conn, 1, "").unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![4]);
    }

    /// Missing files are INCLUDED, on purpose. The old assertion here encoded
    /// the original spec's "missing files hold no slot", which turned out to
    /// desynchronise the table from the numbering: list_tracks keeps a missing
    /// row visible, so skipping it here left it with a stale crate_pos and put
    /// every following row at a different screen index than its backend index.
    #[test]
    fn crate_track_files_includes_missing_files() {
        let conn = seed_crate_db();
        conn.execute("UPDATE track SET is_missing = 1 WHERE id = 1", []).unwrap();
        let rows = crate_track_files(&conn, 1, "House").unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![1, 2]);
    }

    /// The shared direct-children predicate must select exactly the rows the
    /// table lists, for the same folder — that equality is what keeps a row's
    /// screen index equal to its index in the manual order.
    #[test]
    fn crate_track_files_matches_the_list_tracks_folder_filter() {
        let conn = seed_crate_db();
        conn.execute("UPDATE track SET is_missing = 1 WHERE id = 1", []).unwrap();
        for folder in ["House", ""] {
            let numbered: Vec<i64> =
                crate_track_files(&conn, 1, folder).unwrap().iter().map(|r| r.id).collect();
            let mut listed: Vec<i64> = list_tracks(
                &conn, "", Some(1), None, false, None, Some(folder), None, 100, 0,
            )
            .unwrap()
            .rows
            .iter()
            .map(|r| r.id)
            .collect();
            listed.sort_unstable();
            assert_eq!(numbered, listed, "folder {folder:?}");
        }
    }

    #[test]
    fn crate_track_files_escapes_like_wildcards() {
        let conn = seed_crate_db();
        conn.execute(
            "INSERT INTO track (id, library_id, rel_path, file_size, modified_at)
             VALUES (6, 1, '100% House/d.mp3', 1, '0'), (7, 1, '100X House/e.mp3', 1, '0')",
            [],
        )
        .unwrap();
        let rows = crate_track_files(&conn, 1, "100% House").unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![6]);
    }

    #[test]
    fn stick_tracks_lists_only_that_sticks_present_tracks() {
        let dir = std::env::temp_dir().join(format!("spindex-stick-tracks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let _ = std::fs::remove_file(&db);
        let conn = open(&db).unwrap();
        conn.execute("INSERT INTO library (id, name, root_path) VALUES (1, 'MP3', 'D:\\MP3')", []).unwrap();
        conn.execute("INSERT INTO library (id, name, root_path) VALUES (2, 'Other', 'E:\\Music')", []).unwrap();
        for (id, lib, rel, missing) in [(1, 1, "b.mp3", 0), (2, 1, "A.mp3", 0), (3, 1, "gone.mp3", 1), (4, 2, "x.mp3", 0)] {
            conn.execute(
                "INSERT INTO track (id, library_id, rel_path, file_size, modified_at, is_missing, title)
                 VALUES (?1, ?2, ?3, 10, '0', ?4, 'T')",
                rusqlite::params![id, lib, rel, missing],
            )
            .unwrap();
        }
        let t = stick_tracks(&conn, "D:").unwrap();
        let rels: Vec<&str> = t.iter().map(|t| t.stick_rel_path.as_str()).collect();
        assert_eq!(rels, vec!["MP3/A.mp3", "MP3/b.mp3"], "sorted, missing and other drives excluded");
        assert_eq!(t[0].abs_path, std::path::PathBuf::from("D:\\MP3\\A.mp3"));
        assert_eq!(t[0].title.as_deref(), Some("T"));
    }

    #[test]
    fn stick_tracks_lists_a_file_indexed_by_two_libraries_once() {
        // A recursive library at the drive root plus a nested one index the
        // same file twice; the export must list it once, owned by the
        // longest root (the library `stick_path::resolve_with` picks).
        let dir = std::env::temp_dir().join(format!("spindex-stick-dupes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let _ = std::fs::remove_file(&db);
        let conn = open(&db).unwrap();
        conn.execute("INSERT INTO library (id, name, root_path) VALUES (1, 'root', 'D:\\')", []).unwrap();
        conn.execute("INSERT INTO library (id, name, root_path) VALUES (2, 'MP3', 'D:\\MP3')", []).unwrap();
        for (id, lib, rel) in [(1, 1, "MP3\\a.mp3"), (2, 2, "a.mp3")] {
            conn.execute(
                "INSERT INTO track (id, library_id, rel_path, file_size, modified_at) VALUES (?1, ?2, ?3, 10, '0')",
                rusqlite::params![id, lib, rel],
            )
            .unwrap();
        }
        let t = stick_tracks(&conn, "D:").unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].id, 2, "the nested library's row wins");
        assert_eq!(t[0].stick_rel_path, "MP3/a.mp3");
    }

    #[test]
    fn set_crate_positions_writes_all_rows() {
        let conn = seed_crate_db();
        set_crate_positions(&conn, &[(1, 2), (2, 1)]).unwrap();
        let a: i64 = conn
            .query_row("SELECT crate_pos FROM track WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        let b: i64 = conn
            .query_row("SELECT crate_pos FROM track WHERE id = 2", [], |r| r.get(0))
            .unwrap();
        assert_eq!((a, b), (2, 1));
    }

    /// Guards against the two-branches-claim-the-same-version-number failure
    /// mode (see the v7/v8 quality-columns note above): a database written by
    /// a build that stopped at v10 must still open, migrate to v11, and serve
    /// a folder-crate listing afterwards.
    #[test]
    fn pre_v11_database_migrates_and_lists_a_crate() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        migrate(&conn).unwrap();
        // simulate a database written by a build that stopped at v10
        conn.execute_batch(
            "ALTER TABLE track DROP COLUMN crate_pos;
             ALTER TABLE track DROP COLUMN beat_grid;
             ALTER TABLE track DROP COLUMN tempo_drift_pct;
             ALTER TABLE track DROP COLUMN downbeat_grid;
             ALTER TABLE track DROP COLUMN serato_grid_source;
             ALTER TABLE track DROP COLUMN serato_grid_written_at;
             PRAGMA user_version = 10;",
        )
        .unwrap();
        migrate(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 14);
        conn.execute(
            "INSERT INTO library (id, name, root_path) VALUES (1, 'L', 'C:/L')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO track (library_id, rel_path, file_size, modified_at)
             VALUES (1, 'House/a.mp3', 1, '0')",
            [],
        )
        .unwrap();
        let page =
            list_tracks(&conn, "", Some(1), Some("num"), false, None, Some("House"), None, 10, 0)
                .unwrap();
        assert_eq!(page.rows.len(), 1);
    }

    /// Same guard as the v11 test above: a database written by a build that
    /// stopped at v11 must migrate and still serve a listing. Two branches
    /// once both claimed v7 and a DB migrated by one broke startup on the
    /// other — this class of bug is worth a test per version.
    #[test]
    fn pre_v12_database_migrates_and_lists_tracks() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        migrate(&conn).unwrap();
        conn.execute_batch(
            "ALTER TABLE track DROP COLUMN beat_grid;
             ALTER TABLE track DROP COLUMN tempo_drift_pct;
             ALTER TABLE track DROP COLUMN downbeat_grid;
             ALTER TABLE track DROP COLUMN serato_grid_source;
             ALTER TABLE track DROP COLUMN serato_grid_written_at;
             PRAGMA user_version = 11;",
        )
        .unwrap();
        migrate(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 14);
        conn.execute("INSERT INTO library (id, name, root_path) VALUES (1, 'L', 'C:/L')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO track (library_id, rel_path, file_size, modified_at)
             VALUES (1, 'a.mp3', 1, '0')",
            [],
        )
        .unwrap();
        let page = list_tracks(&conn, "", Some(1), None, false, None, None, None, 10, 0).unwrap();
        assert_eq!(page.rows.len(), 1);
        assert!(page.rows[0].tempo_drift_pct.is_none());
    }
}
