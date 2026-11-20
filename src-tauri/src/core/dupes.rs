//! Duplicate / similarity matching between libraries (milestone 3 core,
//! pulled forward for the library comparison feature).
//!
//! Levels, strongest first: identical content (sha256), same normalized
//! artist+title tags, similar (duration ±2s + fuzzy title ≥ 0.85).

use std::collections::HashMap;

pub const SIMILARITY_THRESHOLD: f64 = 0.85;
pub const DURATION_WINDOW_SECS: f64 = 2.0;

/// Streams a file through SHA-256, returning lowercase hex.
pub fn hash_file(path: &std::path::Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("opening {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Lowercase, replace all non-alphanumerics with spaces, collapse whitespace.
pub fn normalize_tag(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Normalized Levenshtein similarity in [0, 1] over already-normalized titles.
pub fn title_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    let max = ac.len().max(bc.len());
    1.0 - levenshtein(&ac, &bc) as f64 / max as f64
}

/// A track reduced to the fields matching needs.
#[derive(Debug, Clone)]
pub struct CmpTrack {
    pub id: i64,
    pub sha256: Option<String>,
    /// "normalized_artist|normalized_title", None when both parts are empty.
    pub artist_title: Option<String>,
    pub norm_title: String,
    pub duration: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchLevel {
    /// Weakest: identical filename is common for unrelated rips ("01-track.mp3").
    SameFilename,
    Similar,
    SameTags,
    Identical,
}

impl MatchLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            MatchLevel::Identical => "identical",
            MatchLevel::SameTags => "same_tags",
            MatchLevel::Similar => "similar",
            MatchLevel::SameFilename => "same_filename",
        }
    }
}

/// For each source track with at least one match in `target`, the strongest
/// match level and one matching target track id.
pub fn classify_matches(
    source: &[CmpTrack],
    target: &[CmpTrack],
) -> HashMap<i64, (MatchLevel, i64)> {
    let mut sha_map: HashMap<&str, i64> = HashMap::new();
    let mut tag_map: HashMap<&str, i64> = HashMap::new();
    // duration bucketed to whole seconds; ±2s lookup scans buckets b-2..=b+2
    let mut dur_buckets: HashMap<i64, Vec<&CmpTrack>> = HashMap::new();
    for t in target {
        if let Some(sha) = t.sha256.as_deref() {
            sha_map.entry(sha).or_insert(t.id);
        }
        if let Some(at) = t.artist_title.as_deref() {
            tag_map.entry(at).or_insert(t.id);
        }
        if let Some(d) = t.duration {
            if !t.norm_title.is_empty() {
                dur_buckets.entry(d.round() as i64).or_default().push(t);
            }
        }
    }

    let mut out = HashMap::new();
    for s in source {
        if let Some(&tid) = s.sha256.as_deref().and_then(|sha| sha_map.get(sha)) {
            out.insert(s.id, (MatchLevel::Identical, tid));
            continue;
        }
        if let Some(&tid) = s.artist_title.as_deref().and_then(|at| tag_map.get(at)) {
            out.insert(s.id, (MatchLevel::SameTags, tid));
            continue;
        }
        let (Some(d), false) = (s.duration, s.norm_title.is_empty()) else {
            continue;
        };
        let bucket = d.round() as i64;
        'buckets: for b in bucket - 2..=bucket + 2 {
            for t in dur_buckets.get(&b).map(Vec::as_slice).unwrap_or(&[]) {
                let dt = t.duration.unwrap_or(f64::MAX);
                if (dt - d).abs() <= DURATION_WINDOW_SECS
                    && title_similarity(&s.norm_title, &t.norm_title) >= SIMILARITY_THRESHOLD
                {
                    out.insert(s.id, (MatchLevel::Similar, t.id));
                    break 'buckets;
                }
            }
        }
    }
    out
}

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct LibraryComparison {
    pub library_id: i64,
    pub name: String,
    pub source_total: i64,
    pub shared: i64,
    pub pct_of_source: f64,
    pub identical: i64,
    pub same_tags: i64,
    pub similar: i64,
    pub unhashed_source: i64,
    pub unhashed_target: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PairTrack {
    pub id: i64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub rel_path: String,
    pub bitrate_kbps: Option<i64>,
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchPair {
    pub source: PairTrack,
    pub target: PairTrack,
    pub level: String,
}

#[derive(Debug, Serialize)]
pub struct MatchPage {
    pub rows: Vec<MatchPair>,
    pub total: i64,
}

/// Loads a library's present tracks reduced to matching fields. A missing
/// title falls back to the filename stem so untagged files can still match
/// at the "similar" level.
fn load_cmp_tracks(conn: &Connection, scope: Scope) -> Result<Vec<CmpTrack>> {
    let (clause, value) = scope.filter();
    let mut stmt = conn.prepare(&format!(
        "SELECT id, sha256, artist, title, rel_path, duration_secs
         FROM track WHERE {clause} AND is_missing = 0"
    ))?;
    let rows = stmt.query_map(params![value], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<f64>>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, sha256, artist, title, rel_path, duration) = row?;
        let stem = rel_path
            .rsplit('/')
            .next()
            .unwrap_or(&rel_path)
            .rsplit_once('.')
            .map(|(s, _)| s.to_string())
            .unwrap_or_else(|| rel_path.clone());
        let norm_title = normalize_tag(title.as_deref().unwrap_or(&stem));
        let norm_artist = normalize_tag(artist.as_deref().unwrap_or(""));
        let norm_tag_title = normalize_tag(title.as_deref().unwrap_or(""));
        let artist_title = if norm_artist.is_empty() && norm_tag_title.is_empty() {
            None
        } else {
            Some(format!("{norm_artist}|{norm_tag_title}"))
        };
        out.push(CmpTrack { id, sha256, artist_title, norm_title, duration });
    }
    Ok(out)
}

fn unhashed_count(conn: &Connection, scope: Scope) -> Result<i64> {
    let (clause, value) = scope.filter();
    Ok(conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM track
             WHERE {clause} AND is_missing = 0 AND sha256 IS NULL"
        ),
        params![value],
        |r| r.get(0),
    )?)
}

/// Which tracks a comparison or duplicate search looks at.
#[derive(Debug, Clone, Copy)]
pub enum Scope<'a> {
    Library(i64),
    /// Explicit track ids (a crate or playlist), resolved by the caller.
    Ids(&'a [i64]),
}

impl Scope<'_> {
    /// WHERE fragment and its single bound value. Id sets go through one JSON
    /// parameter (`json_each`), like `list_tracks`, so there is no placeholder
    /// explosion for large crates.
    fn filter(&self) -> (&'static str, rusqlite::types::Value) {
        match self {
            Scope::Library(id) => ("library_id = ?1", rusqlite::types::Value::Integer(*id)),
            Scope::Ids(ids) => (
                "id IN (SELECT value FROM json_each(?1))",
                rusqlite::types::Value::Text(
                    serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_string()),
                ),
            ),
        }
    }
}

/// Overlap between two track sets, counted by match level.
#[derive(Debug, Clone, Serialize)]
pub struct SetCounts {
    pub source_total: i64,
    pub shared: i64,
    pub pct_of_source: f64,
    pub identical: i64,
    pub same_tags: i64,
    pub similar: i64,
    pub unhashed_source: i64,
    pub unhashed_target: i64,
}

pub fn compare_scopes(conn: &Connection, source: Scope, target: Scope) -> Result<SetCounts> {
    let s = load_cmp_tracks(conn, source)?;
    let t = load_cmp_tracks(conn, target)?;
    Ok(counts_between(&s, &t))
}

/// The set's tracks reduced to matching fields, loaded once so a caller
/// comparing one source against many targets does not reload it each time.
pub fn load_scope(conn: &Connection, scope: Scope) -> Result<Vec<CmpTrack>> {
    load_cmp_tracks(conn, scope)
}

/// Overlap counts between two already-loaded sets. Unhashed counts come from
/// the loaded rows, so no extra queries run per candidate.
pub fn counts_between(s: &[CmpTrack], t: &[CmpTrack]) -> SetCounts {
    let matches = classify_matches(s, t);
    let count = |lvl: MatchLevel| matches.values().filter(|(l, _)| *l == lvl).count() as i64;
    let source_total = s.len() as i64;
    let shared = matches.len() as i64;
    SetCounts {
        source_total,
        shared,
        pct_of_source: if source_total > 0 {
            shared as f64 * 100.0 / source_total as f64
        } else {
            0.0
        },
        identical: count(MatchLevel::Identical),
        same_tags: count(MatchLevel::SameTags),
        similar: count(MatchLevel::Similar),
        unhashed_source: s.iter().filter(|c| c.sha256.is_none()).count() as i64,
        unhashed_target: t.iter().filter(|c| c.sha256.is_none()).count() as i64,
    }
}

/// Tracks with no match at any level, on each side.
#[derive(Debug, Serialize)]
pub struct Unmatched {
    pub only_source: Vec<PairTrack>,
    pub only_target: Vec<PairTrack>,
    pub only_source_total: i64,
    pub only_target_total: i64,
}

/// First `limit` unmatched tracks of each side, plus the full counts.
pub fn unmatched_scoped(conn: &Connection, source: Scope, target: Scope, limit: usize) -> Result<Unmatched> {
    let s = load_cmp_tracks(conn, source)?;
    let t = load_cmp_tracks(conn, target)?;
    // Each side against the other: `classify_matches` keeps one target per
    // source, so a second target copy of a matched song would otherwise look
    // unmatched. Every level is symmetric, so the reverse call is exact.
    let forward = classify_matches(&s, &t);
    let backward = classify_matches(&t, &s);
    let only_s: Vec<i64> = s.iter().map(|c| c.id).filter(|id| !forward.contains_key(id)).collect();
    let only_t: Vec<i64> = t.iter().map(|c| c.id).filter(|id| !backward.contains_key(id)).collect();
    let fetch = |ids: &[i64]| -> Result<Vec<PairTrack>> {
        ids.iter().take(limit).map(|id| pair_track(conn, *id)).collect()
    };
    Ok(Unmatched {
        only_source: fetch(&only_s)?,
        only_target: fetch(&only_t)?,
        only_source_total: only_s.len() as i64,
        only_target_total: only_t.len() as i64,
    })
}

/// One row per other library, ranked by percentage of the source library
/// that also exists there, descending.
pub fn compare_library(conn: &Connection, source_id: i64) -> Result<Vec<LibraryComparison>> {
    let mut out = Vec::new();
    for lib in crate::core::db::list_libraries(conn)? {
        if lib.id == source_id {
            continue;
        }
        let c = compare_scopes(conn, Scope::Library(source_id), Scope::Library(lib.id))?;
        out.push(LibraryComparison {
            library_id: lib.id,
            name: lib.name,
            source_total: c.source_total,
            shared: c.shared,
            pct_of_source: c.pct_of_source,
            identical: c.identical,
            same_tags: c.same_tags,
            similar: c.similar,
            unhashed_source: c.unhashed_source,
            unhashed_target: c.unhashed_target,
        });
    }
    out.sort_by(|a, b| {
        b.pct_of_source
            .partial_cmp(&a.pct_of_source)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.name.cmp(&b.name))
    });
    Ok(out)
}

fn pair_track(conn: &Connection, id: i64) -> Result<PairTrack> {
    Ok(conn.query_row(
        "SELECT id, title, artist, rel_path, bitrate_kbps, format
         FROM track WHERE id = ?1",
        params![id],
        |row| {
            Ok(PairTrack {
                id: row.get(0)?,
                title: row.get(1)?,
                artist: row.get(2)?,
                rel_path: row.get(3)?,
                bitrate_kbps: row.get(4)?,
                format: row.get(5)?,
            })
        },
    )?)
}

/// Matched pairs between two libraries for the drill-down view, strongest
/// level first, optionally filtered to one level. The match set is computed
/// in memory and sliced — same cost as compare_library, fine at 10k tracks.
pub fn compare_details(
    conn: &Connection,
    source_id: i64,
    target_id: i64,
    level: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<MatchPage> {
    compare_details_scoped(conn, Scope::Library(source_id), Scope::Library(target_id), level, limit, offset)
}

/// `compare_details` for any two track sets.
pub fn compare_details_scoped(
    conn: &Connection,
    source_scope: Scope,
    target_scope: Scope,
    level: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<MatchPage> {
    let source = load_cmp_tracks(conn, source_scope)?;
    let target = load_cmp_tracks(conn, target_scope)?;
    let matches = classify_matches(&source, &target);

    let mut items: Vec<(i64, MatchLevel, i64)> = matches
        .into_iter()
        .map(|(sid, (lvl, tid))| (sid, lvl, tid))
        .filter(|(_, lvl, _)| level.is_none_or(|l| lvl.as_str() == l))
        .collect();
    // strongest level first (MatchLevel ords Similar < SameTags < Identical),
    // then source id for a stable order across pages
    items.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let total = items.len() as i64;
    let mut rows = Vec::new();
    let start = offset.max(0) as usize;
    let end = (start + limit.max(0) as usize).min(items.len());
    for &(sid, lvl, tid) in items.get(start..end).unwrap_or(&[]) {
        rows.push(MatchPair {
            source: pair_track(conn, sid)?,
            target: pair_track(conn, tid)?,
            level: lvl.as_str().to_string(),
        });
    }
    Ok(MatchPage { rows, total })
}

// ---------- Within-library duplicate groups (milestone 3) ----------

#[derive(Debug, Clone, Serialize)]
pub struct DupeTrackInfo {
    pub id: i64,
    pub library_id: i64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub rel_path: String,
    pub bitrate_kbps: Option<i64>,
    pub format: Option<String>,
    pub file_size: i64,
    pub duration_secs: Option<f64>,
    pub quality_score: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct DupeGroup {
    pub level: String,
    pub tracks: Vec<DupeTrackInfo>,
    /// The copy SpinDex recommends keeping (see `rank_group`). Always Some for
    /// a real group; None only if a group were empty, which cannot happen.
    pub suggested_keeper_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct DupeReport {
    pub groups: Vec<DupeGroup>,
    /// Tracks without sha256 — the identical level is incomplete until hashed.
    pub unhashed: i64,
}

/// Union-find with per-component strongest match level.
struct UnionFind {
    parent: Vec<usize>,
    level: Vec<Option<MatchLevel>>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind {
            parent: (0..n).collect(),
            level: vec![None; n],
        }
    }
    fn find(&mut self, i: usize) -> usize {
        if self.parent[i] != i {
            let root = self.find(self.parent[i]);
            self.parent[i] = root;
        }
        self.parent[i]
    }
    fn union(&mut self, a: usize, b: usize, level: MatchLevel) {
        let (ra, rb) = (self.find(a), self.find(b));
        let strongest = self.level[ra].max(self.level[rb]).max(Some(level));
        if ra != rb {
            self.parent[rb] = ra;
        }
        self.level[ra] = strongest;
    }
}

struct DupeCandidate {
    cmp: CmpTrack,
    filename: String,
    info: DupeTrackInfo,
}

fn load_dupe_candidates(conn: &Connection, scope: Scope) -> Result<Vec<DupeCandidate>> {
    let (clause, value) = scope.filter();
    let mut stmt = conn.prepare(&format!(
        "SELECT id, sha256, artist, title, rel_path, duration_secs,
                bitrate_kbps, format, file_size, quality_score, library_id
         FROM track WHERE {clause} AND is_missing = 0"
    ))?;
    let rows = stmt.query_map(params![value], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<f64>>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, Option<i64>>(9)?,
            row.get::<_, i64>(10)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, sha256, artist, title, rel_path, duration, bitrate_kbps, format, file_size, quality_score, library_id) =
            row?;
        let filename = rel_path
            .rsplit('/')
            .next()
            .unwrap_or(&rel_path)
            .to_lowercase();
        let stem = filename
            .rsplit_once('.')
            .map(|(s, _)| s.to_string())
            .unwrap_or_else(|| filename.clone());
        let norm_title = normalize_tag(title.as_deref().unwrap_or(&stem));
        let norm_artist = normalize_tag(artist.as_deref().unwrap_or(""));
        let norm_tag_title = normalize_tag(title.as_deref().unwrap_or(""));
        let artist_title = if norm_artist.is_empty() && norm_tag_title.is_empty() {
            None
        } else {
            Some(format!("{norm_artist}|{norm_tag_title}"))
        };
        out.push(DupeCandidate {
            cmp: CmpTrack {
                id,
                sha256,
                artist_title,
                norm_title,
                duration,
            },
            filename,
            info: DupeTrackInfo {
                id,
                library_id,
                title,
                artist,
                rel_path,
                bitrate_kbps,
                format,
                file_size,
                duration_secs: duration,
                quality_score,
            },
        });
    }
    Ok(out)
}

/// Duplicate groups within one library. Union-find over all four levels;
/// each group carries the strongest level that connected any of its edges.
/// Uses whatever sha256 values exist — hashing stays user-triggered.
pub fn find_duplicates(conn: &Connection, library_id: i64) -> Result<DupeReport> {
    find_duplicates_scoped(conn, Scope::Library(library_id))
}

/// `find_duplicates` for any track set (a crate or playlist).
pub fn find_duplicates_scoped(conn: &Connection, scope: Scope) -> Result<DupeReport> {
    let cands = load_dupe_candidates(conn, scope)?;
    let n = cands.len();
    let mut uf = UnionFind::new(n);

    let mut by_key: HashMap<&str, usize> = HashMap::new();
    for (i, c) in cands.iter().enumerate() {
        if let Some(sha) = c.cmp.sha256.as_deref() {
            match by_key.entry(sha) {
                std::collections::hash_map::Entry::Occupied(e) => {
                    uf.union(*e.get(), i, MatchLevel::Identical)
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(i);
                }
            }
        }
    }
    by_key.clear();
    for (i, c) in cands.iter().enumerate() {
        if let Some(at) = c.cmp.artist_title.as_deref() {
            match by_key.entry(at) {
                std::collections::hash_map::Entry::Occupied(e) => {
                    uf.union(*e.get(), i, MatchLevel::SameTags)
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(i);
                }
            }
        }
    }
    by_key.clear();
    for (i, c) in cands.iter().enumerate() {
        match by_key.entry(c.filename.as_str()) {
            std::collections::hash_map::Entry::Occupied(e) => {
                uf.union(*e.get(), i, MatchLevel::SameFilename)
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(i);
            }
        }
    }

    // similar: duration-bucketed pairwise check, same scheme as classify_matches
    let mut dur_buckets: HashMap<i64, Vec<usize>> = HashMap::new();
    for (i, c) in cands.iter().enumerate() {
        if let Some(d) = c.cmp.duration {
            if !c.cmp.norm_title.is_empty() {
                dur_buckets.entry(d.round() as i64).or_default().push(i);
            }
        }
    }
    for (i, c) in cands.iter().enumerate() {
        let (Some(d), false) = (c.cmp.duration, c.cmp.norm_title.is_empty()) else {
            continue;
        };
        let bucket = d.round() as i64;
        for b in bucket - 2..=bucket + 2 {
            for &j in dur_buckets.get(&b).map(Vec::as_slice).unwrap_or(&[]) {
                if j <= i {
                    continue; // each pair once
                }
                let other = &cands[j];
                let dj = other.cmp.duration.unwrap_or(f64::MAX);
                if (dj - d).abs() <= DURATION_WINDOW_SECS
                    && title_similarity(&c.cmp.norm_title, &other.cmp.norm_title)
                        >= SIMILARITY_THRESHOLD
                {
                    uf.union(i, j, MatchLevel::Similar);
                }
            }
        }
    }

    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let root = uf.find(i);
        members.entry(root).or_default().push(i);
    }
    let mut groups: Vec<(MatchLevel, DupeGroup)> = Vec::new();
    for (root, idxs) in members {
        if idxs.len() < 2 {
            continue;
        }
        let Some(level) = uf.level[root] else { continue };
        let mut tracks: Vec<DupeTrackInfo> =
            idxs.iter().map(|&i| cands[i].info.clone()).collect();
        tracks.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        let suggested_keeper_id = rank_group(&tracks);
        groups.push((
            level,
            DupeGroup {
                level: level.as_str().to_string(),
                tracks,
                suggested_keeper_id,
            },
        ));
    }
    groups.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.tracks.len().cmp(&a.1.tracks.len()))
            .then(a.1.tracks[0].rel_path.cmp(&b.1.tracks[0].rel_path))
    });

    Ok(DupeReport {
        groups: groups.into_iter().map(|(_, g)| g).collect(),
        unhashed: unhashed_count(conn, scope)?,
    })
}

const LOSSLESS: [&str; 4] = ["flac", "wav", "aiff", "aif"];

/// The copy worth keeping in a duplicate group. Quality score decides only
/// when BOTH copies have been analyzed; if either side is unanalyzed the
/// score is skipped entirely (an unanalyzed lossless master must never lose
/// its keeper spot to an analyzed lossy copy just because it lacks a score
/// yet). Otherwise, first difference wins: lossless-ness, then bitrate, then
/// file size, then the lowest id so the choice is stable across runs.
pub fn rank_group(tracks: &[DupeTrackInfo]) -> Option<i64> {
    tracks
        .iter()
        .max_by(|a, b| {
            let lossless = |t: &DupeTrackInfo| {
                t.format
                    .as_deref()
                    .map(|f| LOSSLESS.contains(&f.to_lowercase().as_str()))
                    .unwrap_or(false)
            };
            let score_cmp = match (a.quality_score, b.quality_score) {
                (Some(x), Some(y)) => x.cmp(&y),
                _ => std::cmp::Ordering::Equal,
            };
            score_cmp
                .then(lossless(a).cmp(&lossless(b)))
                .then(a.bitrate_kbps.cmp(&b.bitrate_kbps))
                .then(a.file_size.cmp(&b.file_size))
                // lowest id wins the tie, so reverse it against max_by
                .then(b.id.cmp(&a.id))
        })
        .map(|t| t.id)
}

/// Number of tracks that sit in at least one duplicate group (health count).
pub fn duplicate_track_count(conn: &Connection, library_id: i64) -> Result<i64> {
    Ok(find_duplicates(conn, library_id)?
        .groups
        .iter()
        .map(|g| g.tracks.len() as i64)
        .sum())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: i64, sha: Option<&str>, artist: Option<&str>, title: &str, dur: Option<f64>) -> CmpTrack {
        CmpTrack {
            id,
            sha256: sha.map(String::from),
            artist_title: artist
                .map(|a| format!("{}|{}", normalize_tag(a), normalize_tag(title)))
                .filter(|s| s != "|"),
            norm_title: normalize_tag(title),
            duration: dur,
        }
    }

    #[test]
    fn normalize_strips_case_punctuation_whitespace() {
        assert_eq!(normalize_tag("  Daft  Punk "), "daft punk");
        assert_eq!(normalize_tag("One More Time (Radio Edit)"), "one more time radio edit");
        assert_eq!(normalize_tag("AC/DC"), "ac dc");
        assert_eq!(normalize_tag("!!!"), "");
    }

    #[test]
    fn similarity_threshold_edges() {
        assert_eq!(title_similarity("one more time", "one more time"), 1.0);
        assert!(title_similarity("one more time", "one more tim") >= 0.85);
        assert!(title_similarity("one more time", "something else") < 0.85);
        assert_eq!(title_similarity("", "anything"), 0.0);
    }

    #[test]
    fn classify_precedence_identical_beats_tags_beats_similar() {
        // target has a content twin, a tag twin, and a fuzzy twin of source track 1
        let source = vec![t(1, Some("aaa"), Some("Daft Punk"), "One More Time", Some(320.0))];
        let target = vec![
            t(10, Some("bbb"), Some("Daft Punk"), "One More Time", Some(320.0)),
            t(11, Some("aaa"), None, "renamed garbage", Some(999.0)),
        ];
        let m = classify_matches(&source, &target);
        assert_eq!(m[&1], (MatchLevel::Identical, 11), "sha match wins even with bad tags");

        let target_tags_only = vec![t(10, Some("bbb"), Some("daft punk"), "ONE MORE TIME", Some(320.0))];
        let m = classify_matches(&source, &target_tags_only);
        assert_eq!(m[&1], (MatchLevel::SameTags, 10));

        let target_fuzzy = vec![t(12, None, Some("Daft Pnuk"), "One More Tme", Some(321.5))];
        let m = classify_matches(&source, &target_fuzzy);
        assert_eq!(m[&1].0, MatchLevel::Similar, "duration ±2s + fuzzy title");
    }

    #[test]
    fn classify_similar_needs_duration_window() {
        let source = vec![t(1, None, Some("A"), "Track Name Here", Some(200.0))];
        let far = vec![t(2, None, Some("B"), "Track Name Here!", Some(210.0))];
        assert!(classify_matches(&source, &far).is_empty(), "10s apart: no match");
        let near = vec![t(2, None, Some("B"), "Track Name Here!", Some(201.9))];
        assert_eq!(classify_matches(&source, &near)[&1].0, MatchLevel::Similar);
    }

    #[test]
    fn empty_tags_never_match_at_tag_level() {
        let source = vec![t(1, None, None, "", Some(100.0))];
        let target = vec![t(2, None, None, "", Some(100.0))];
        assert!(classify_matches(&source, &target).is_empty());
    }

    fn cmp_test_db(name: &str) -> (std::path::PathBuf, rusqlite::Connection) {
        let dir = std::env::temp_dir().join(format!("spindex-dupes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        let conn = crate::core::db::open(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO library (id, name, root_path) VALUES
                (1, 'Main', 'C:/main'), (2, 'USB', 'D:/usb'), (3, 'Empty', 'E:/x');",
        )
        .unwrap();
        (path, conn)
    }

    fn ins(conn: &rusqlite::Connection, lib: i64, rel: &str, sha: Option<&str>,
           artist: Option<&str>, title: Option<&str>, dur: Option<f64>) -> i64 {
        conn.execute(
            "INSERT INTO track (library_id, rel_path, file_size, modified_at,
                                sha256, artist, title, duration_secs)
             VALUES (?1, ?2, 1, 'now', ?3, ?4, ?5, ?6)",
            rusqlite::params![lib, rel, sha, artist, title, dur],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// Two libraries sharing tracks at every level, plus a duplicate pair
    /// inside library 1 (same tags) so the duplicate path has a group.
    fn seed_two_libraries_with_matches(name: &str) -> (std::path::PathBuf, rusqlite::Connection) {
        let (path, conn) = cmp_test_db(name);
        ins(&conn, 1, "a.mp3", Some("s1"), Some("Artist A"), Some("Song One"), Some(180.0));
        ins(&conn, 1, "b.mp3", None, Some("Artist B"), Some("Song Two"), Some(200.0));
        ins(&conn, 1, "c.mp3", None, Some("Artist C"), Some("Song Three"), Some(220.0));
        ins(&conn, 1, "d.mp3", None, Some("Artist D"), Some("Song Four"), Some(240.0));
        ins(&conn, 1, "x/d.mp3", None, Some("artist d"), Some("SONG FOUR"), Some(240.5));
        ins(&conn, 2, "renamed.mp3", Some("s1"), None, Some("whatever"), Some(999.0));
        ins(&conn, 2, "b2.mp3", None, Some("artist b"), Some("SONG TWO"), Some(200.0));
        ins(&conn, 2, "c2.mp3", None, Some("Someone Else"), Some("Song Threee"), Some(221.0));
        ins(&conn, 2, "z.mp3", None, Some("Nobody"), Some("Unrelated"), Some(90.0));
        (path, conn)
    }

    fn ids_of(conn: &rusqlite::Connection, lib: i64) -> Vec<i64> {
        conn.prepare("SELECT id FROM track WHERE library_id = ?1 ORDER BY id")
            .unwrap()
            .query_map([lib], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect()
    }

    #[test]
    fn library_scope_matches_the_old_compare() {
        let (_p, conn) = seed_two_libraries_with_matches("scope-cmp.db");
        let old = compare_library(&conn, 1).unwrap();
        let row = old.iter().find(|r| r.library_id == 2).unwrap();
        let new = compare_scopes(&conn, Scope::Library(1), Scope::Library(2)).unwrap();
        assert_eq!(
            (new.shared, new.identical, new.same_tags, new.similar, new.source_total),
            (row.shared, row.identical, row.same_tags, row.similar, row.source_total)
        );
        let ids1 = ids_of(&conn, 1);
        let ids2 = ids_of(&conn, 2);
        let by_ids = compare_scopes(&conn, Scope::Ids(&ids1), Scope::Ids(&ids2)).unwrap();
        assert_eq!((by_ids.shared, by_ids.source_total), (new.shared, new.source_total));
    }

    #[test]
    fn library_scope_matches_the_old_duplicates() {
        let (_p, conn) = seed_two_libraries_with_matches("scope-dupes.db");
        let old = find_duplicates(&conn, 1).unwrap();
        let ids1 = ids_of(&conn, 1);
        let new = find_duplicates_scoped(&conn, Scope::Ids(&ids1)).unwrap();
        assert!(!old.groups.is_empty());
        assert_eq!(old.groups.len(), new.groups.len());
        assert_eq!(old.unhashed, new.unhashed);
    }

    #[test]
    fn id_scope_only_contains_its_own_tracks() {
        let (_p, conn) = seed_two_libraries_with_matches("scope-own.db");
        let ids1 = ids_of(&conn, 1);
        let c = compare_scopes(&conn, Scope::Ids(&ids1[..1]), Scope::Library(2)).unwrap();
        assert_eq!(c.source_total, 1);
        assert_eq!(c.shared, 1, "a.mp3 has an identical twin in library 2");
    }

    #[test]
    fn empty_set_compares_to_zero_percent() {
        let (_p, conn) = seed_two_libraries_with_matches("scope-empty.db");
        let c = compare_scopes(&conn, Scope::Ids(&[]), Scope::Library(2)).unwrap();
        assert_eq!((c.source_total, c.shared, c.pct_of_source), (0, 0, 0.0));
        assert!(find_duplicates_scoped(&conn, Scope::Ids(&[])).unwrap().groups.is_empty());
    }

    #[test]
    fn unmatched_lists_are_exact() {
        let (_p, conn) = seed_two_libraries_with_matches("scope-unmatched.db");
        let pairs = compare_details_scoped(&conn, Scope::Library(1), Scope::Library(2), None, 10_000, 0).unwrap();
        let counts = compare_scopes(&conn, Scope::Library(1), Scope::Library(2)).unwrap();
        let u = unmatched_scoped(&conn, Scope::Library(1), Scope::Library(2), 10_000).unwrap();
        assert_eq!(u.only_source_total + pairs.total, counts.source_total);
        let matched: std::collections::HashSet<i64> = pairs.rows.iter().map(|p| p.source.id).collect();
        assert!(u.only_source.iter().all(|t| !matched.contains(&t.id)));
        let names: Vec<&str> = u.only_target.iter().map(|t| t.rel_path.as_str()).collect();
        assert_eq!(names, vec!["z.mp3"], "the one target track nothing in library 1 matches");
    }

    #[test]
    fn a_second_matching_copy_in_the_target_is_not_listed_as_missing() {
        // The target holds two copies of one song; the source holds one.
        // Neither target copy is "only in target".
        let (_p, conn) = cmp_test_db("scope-twin-target.db");
        ins(&conn, 1, "song.mp3", None, Some("Artist"), Some("Song"), Some(200.0));
        ins(&conn, 2, "song.mp3", None, Some("Artist"), Some("Song"), Some(200.0));
        ins(&conn, 2, "song.wav", None, Some("artist"), Some("SONG"), Some(200.4));
        let u = unmatched_scoped(&conn, Scope::Library(1), Scope::Library(2), 100).unwrap();
        assert_eq!(u.only_target_total, 0, "{:?}", u.only_target.iter().map(|t| &t.rel_path).collect::<Vec<_>>());
        assert_eq!(u.only_source_total, 0);
    }

    #[test]
    fn compare_library_counts_and_ranking() {
        let (path, conn) = cmp_test_db("cmp.db");
        // Main: 4 tracks. USB shares 3 of them at three different levels.
        ins(&conn, 1, "a.mp3", Some("s1"), Some("Artist A"), Some("Song One"), Some(180.0));
        ins(&conn, 1, "b.mp3", None, Some("Artist B"), Some("Song Two"), Some(200.0));
        ins(&conn, 1, "c.mp3", None, Some("Artist C"), Some("Song Three"), Some(220.0));
        ins(&conn, 1, "d.mp3", None, Some("Artist D"), Some("Song Four"), Some(240.0));

        ins(&conn, 2, "renamed.mp3", Some("s1"), None, Some("whatever"), Some(999.0));
        ins(&conn, 2, "b2.mp3", None, Some("artist b"), Some("SONG TWO"), Some(200.0));
        ins(&conn, 2, "c2.mp3", None, Some("Someone Else"), Some("Song Threee"), Some(221.0));
        ins(&conn, 2, "z.mp3", None, Some("Nobody"), Some("Unrelated"), Some(90.0));

        let rows = compare_library(&conn, 1).unwrap();
        assert_eq!(rows.len(), 2, "one row per other library");
        let usb = &rows[0];
        assert_eq!(usb.library_id, 2, "ranked first (75% > 0%)");
        assert_eq!(usb.source_total, 4);
        assert_eq!(usb.shared, 3);
        assert_eq!(usb.identical, 1);
        assert_eq!(usb.same_tags, 1);
        assert_eq!(usb.similar, 1);
        assert!((usb.pct_of_source - 75.0).abs() < 0.01);
        assert_eq!(usb.unhashed_source, 3);
        assert_eq!(usb.unhashed_target, 3);
        let empty = &rows[1];
        assert_eq!(empty.shared, 0);
        assert_eq!(empty.pct_of_source, 0.0);

        let page = compare_details(&conn, 1, 2, None, 100, 0).unwrap();
        assert_eq!(page.total, 3);
        assert_eq!(page.rows[0].level, "identical", "ordered strongest first");
        assert_eq!(page.rows[0].target.rel_path, "renamed.mp3");

        let only_tags = compare_details(&conn, 1, 2, Some("same_tags"), 100, 0).unwrap();
        assert_eq!(only_tags.total, 1);
        assert_eq!(only_tags.rows[0].source.rel_path, "b.mp3");

        let paged = compare_details(&conn, 1, 2, None, 1, 1).unwrap();
        assert_eq!(paged.total, 3);
        assert_eq!(paged.rows.len(), 1);
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn find_duplicates_groups_transitively_with_strongest_level() {
        let (path, conn) = cmp_test_db("groups.db");
        // a and b share tags; b and c share content -> one group, level identical
        ins(&conn, 1, "x/a.mp3", None, Some("Artist"), Some("Song"), Some(180.0));
        ins(&conn, 1, "y/b.mp3", Some("h1"), Some("artist"), Some("SONG"), Some(180.5));
        ins(&conn, 1, "z/c.mp3", Some("h1"), Some("Other"), Some("Renamed"), Some(999.0));
        // d and e only share a filename (different folders, different tags)
        ins(&conn, 1, "cd1/01-track.mp3", None, Some("A"), Some("First Song"), Some(100.0));
        ins(&conn, 1, "cd2/01-Track.mp3", None, Some("B"), Some("Completely Different"), Some(300.0));
        // f is unique
        ins(&conn, 1, "solo.mp3", None, Some("C"), Some("Lonely"), Some(200.0));
        // g/h similar: close duration + fuzzy title
        ins(&conn, 1, "g.mp3", None, Some("D"), Some("Night Drive Extended"), Some(250.0));
        ins(&conn, 1, "h.mp3", None, Some("E"), Some("Night Drive Extendet"), Some(251.0));

        let report = find_duplicates(&conn, 1).unwrap();
        assert_eq!(report.groups.len(), 3);
        assert_eq!(report.groups[0].level, "identical", "strongest first");
        assert_eq!(report.groups[0].tracks.len(), 3, "transitive a-b-c merge");
        assert_eq!(report.groups[1].level, "similar");
        assert_eq!(report.groups[2].level, "same_filename");
        assert_eq!(report.unhashed, 6, "everything except h1 twins");

        assert_eq!(duplicate_track_count(&conn, 1).unwrap(), 7);

        for g in &report.groups {
            let keeper = g.suggested_keeper_id.expect("every real group has a keeper");
            assert!(
                g.tracks.iter().any(|t| t.id == keeper),
                "keeper must be a member of its own group",
            );
        }

        // missing tracks are excluded from matching
        conn.execute("UPDATE track SET is_missing = 1 WHERE rel_path = 'z/c.mp3'", [])
            .unwrap();
        let report = find_duplicates(&conn, 1).unwrap();
        assert_eq!(report.groups[0].level, "same_tags", "sha twin gone, tags remain");
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn level_ordering_puts_filename_weakest() {
        assert!(MatchLevel::Identical > MatchLevel::SameTags);
        assert!(MatchLevel::SameTags > MatchLevel::Similar);
        assert!(MatchLevel::Similar > MatchLevel::SameFilename);
    }

    #[test]
    fn hash_file_produces_known_sha256() {
        let dir = std::env::temp_dir().join(format!("spindex-dupes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("hashme.bin");
        std::fs::write(&f, b"abc").unwrap();
        assert_eq!(
            hash_file(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(hash_file(&dir.join("nope.bin")).is_err());
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn compare_empty_source_returns_zero_pct() {
        let (path, conn) = cmp_test_db("empty.db");
        ins(&conn, 2, "x.mp3", None, Some("A"), Some("B"), Some(100.0));
        let rows = compare_library(&conn, 3).unwrap();
        assert!(rows.iter().all(|r| r.pct_of_source == 0.0 && r.shared == 0));
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    fn info(
        id: i64,
        format: &str,
        bitrate: Option<i64>,
        size: i64,
        quality: Option<i64>,
    ) -> DupeTrackInfo {
        DupeTrackInfo {
            id,
            library_id: 1,
            title: None,
            artist: None,
            rel_path: format!("f/{id}.{format}"),
            bitrate_kbps: bitrate,
            format: Some(format.to_string()),
            file_size: size,
            duration_secs: Some(180.0),
            quality_score: quality,
        }
    }

    #[test]
    fn rank_group_prefers_higher_quality_score() {
        let g = vec![
            info(1, "mp3", Some(320), 9_000_000, Some(60)),
            info(2, "mp3", Some(192), 5_000_000, Some(88)),
        ];
        assert_eq!(rank_group(&g), Some(2), "quality score outranks bitrate");
    }

    #[test]
    fn rank_group_unanalyzed_lossless_beats_analyzed_lossy() {
        // Score only decides when BOTH copies have one; an unanalyzed FLAC
        // must not lose its keeper spot to an analyzed low-bitrate MP3.
        let g = vec![
            info(1, "flac", None, 30_000_000, None),
            info(2, "mp3", Some(128), 4_000_000, Some(40)),
        ];
        assert_eq!(rank_group(&g), Some(1), "unanalyzed lossless outranks an analyzed lossy copy");
    }

    #[test]
    fn rank_group_two_analyzed_copies_rank_by_score() {
        // Both copies analyzed: score still decides even when it picks the
        // lossy file (an upscaled "FLAC" scoring low loses to a genuine 320).
        let g = vec![
            info(1, "flac", Some(900), 30_000_000, Some(35)),
            info(2, "mp3", Some(320), 9_000_000, Some(90)),
        ];
        assert_eq!(rank_group(&g), Some(2), "both analyzed: score wins even over lossless");
    }

    #[test]
    fn rank_group_prefers_lossless_when_scores_tie() {
        let g = vec![
            info(1, "mp3", Some(320), 9_000_000, None),
            info(2, "flac", Some(900), 30_000_000, None),
        ];
        assert_eq!(rank_group(&g), Some(2));
    }

    #[test]
    fn rank_group_falls_back_to_bitrate_then_size() {
        let bitrate = vec![
            info(1, "mp3", Some(192), 9_000_000, None),
            info(2, "mp3", Some(320), 5_000_000, None),
        ];
        assert_eq!(rank_group(&bitrate), Some(2), "bitrate before size");
        let size = vec![
            info(1, "mp3", None, 5_000_000, None),
            info(2, "mp3", None, 9_000_000, None),
        ];
        assert_eq!(rank_group(&size), Some(2), "size when no bitrate");
    }

    #[test]
    fn rank_group_is_stable_and_handles_empty() {
        let same = vec![
            info(7, "mp3", Some(320), 9_000_000, Some(80)),
            info(3, "mp3", Some(320), 9_000_000, Some(80)),
        ];
        assert_eq!(rank_group(&same), Some(3), "identical inputs -> lowest id");
        assert_eq!(rank_group(&[]), None);
    }
}
