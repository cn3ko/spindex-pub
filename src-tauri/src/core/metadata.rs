use std::path::Path;

use anyhow::{bail, Context, Result};
use lofty::config::{ParseOptions, ParsingMode, WriteOptions};
use lofty::file::{AudioFile, TaggedFile, TaggedFileExt};
use lofty::picture::{Picture, PictureType};
// `write_artwork` takes a MimeType in its public signature, so re-export it
// (pub) — callers outside this crate and tests must be able to name it.
pub use lofty::picture::MimeType;
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::{ItemKey, Tag};
use lofty::TextEncoding;

/// Everything we read out of an audio file's tags and properties.
/// `key` is stored raw as found in the tag; Camelot normalization is a later milestone.
#[derive(Debug, Default, Clone)]
pub struct TrackMeta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub label: Option<String>,
    pub year: Option<i64>,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub rating: Option<i64>,
    pub comment: Option<String>,
    pub duration_secs: Option<f64>,
    pub bitrate_kbps: Option<i64>,
    pub sample_rate: Option<i64>,
    pub format: Option<String>,
    pub has_artwork: bool,
}

pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "wav", "aiff", "aif", "aac", "m4a"];

/// Which ID3v2 minor version to write, decided per container.
///
/// Measured against a real Serato library rather than assumed, because the two
/// containers genuinely differ:
/// * **MP3 → v2.3.** Serato's own guidance is v2.3, and a SpinDex-written v2.3
///   MP3 displays correctly (tags and cover) in the user's library.
/// * **WAV → v2.3.** Every one of the 169 lowercase `id3 ` chunks across the
///   two measured Serato libraries is v2.3, and the 41 of them carrying an
///   artist all have that artist in Serato's database. A v2.3 lowercase chunk
///   is therefore exactly the shape Serato itself writes and reads.
/// * **AIFF → v2.4.** All 78 AIFF/AIF files Serato reads are v2.4.
///
/// An earlier revision wrote WAV as v2.4 on the reasoning that "Serato reads no
/// artwork from v2.3 in a RIFF chunk". That comparison was confounded: both the
/// v2.3 and the v2.4 WAVs it weighed sat in an UPPERCASE `ID3 ` chunk, which
/// Serato ignores whatever the version — so the version was never the variable.
/// See `chunk_container`.
///
/// Formats that carry no ID3 tag at all (flac, m4a) are unaffected either way.
fn wants_id3v23(path: &Path) -> bool {
    !matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("aif" | "aiff")
    )
}

/// Which RIFF/AIFF chunk id this container's ID3 tag belongs in.
///
/// Chunk ids are case-sensitive four-character codes and the two containers
/// genuinely differ, so this is not cosmetic:
/// * **WAV (RIFF) → `id3 ` (lowercase)**, the ID3v2 recommendation for RIFF and
///   what Serato itself writes. Serato reads a WAV's tag from **nothing else**.
/// * **AIFF (FORM) → `ID3 ` (uppercase)**, which is that spec's own id and what
///   every AIFF Serato reads correctly carries.
///
/// lofty writes `ID3 ` into both, which is why WAV tags SpinDex saved were
/// invisible to Serato — see `normalize_id3_chunk_id`.
///
/// Returns `(container is big-endian, wanted chunk id)`; `None` for formats
/// that carry no chunked container at all (mp3, flac, m4a).
fn chunk_container(path: &Path) -> Option<(bool, [u8; 4])> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("wav") => Some((false, *b"id3 ")),
        Some("aif" | "aiff") => Some((true, *b"ID3 ")),
        _ => None,
    }
}

/// Puts the just-written ID3 chunk under the id its container calls for.
///
/// lofty always writes `ID3 `, so this runs after every save — best-effort like
/// the pre-write repairs, because the tag itself is already on disk and
/// readable; only its visibility to Serato depends on the id.
fn normalize_id3_chunk_id(path: &Path) {
    let Some((big_endian, want)) = chunk_container(path) else {
        return;
    };
    if let Err(e) = rewrite_id3_chunk_id(path, big_endian, &want) {
        tracing::warn!("normalizing ID3 chunk id in {}: {e}", path.display());
    }
}

/// Every save in this module must go through these options.
fn write_options(path: &Path) -> WriteOptions {
    WriteOptions::default().use_id3v23(wants_id3v23(path))
}

/// Saves the file's primary tag. ID3v2 tags are first rebuilt as a concrete
/// `Id3v2Tag`: the generic-`Tag` save path lowers every item to a plain text
/// frame, and the ID3v2.3 writer can only turn a real Timestamp TDRC frame
/// into TYER — a text TDRC is silently discarded, losing the year on every
/// save. The concrete conversion produces proper Timestamp frames.
fn save_tagged(tagged: &TaggedFile, path: &Path, what: &str) -> Result<()> {
    save_tagged_rating(tagged, path, what, None)
}

/// `save_tagged`, also setting the ID3 star rating when `rating` is `Some`
/// (`Some(None)` clears it). Non-ID3 ratings are already in the generic tag.
fn save_tagged_rating(
    tagged: &TaggedFile,
    path: &Path,
    what: &str,
    rating: Option<Option<i64>>,
) -> Result<()> {
    let err_ctx = || {
        format!(
            "writing {what} to {} (is the file read-only or in use?)",
            path.display()
        )
    };
    let tag = tagged
        .primary_tag()
        .context("no writable tag for this file type")?;
    if tag.tag_type() == lofty::tag::TagType::Id3v2 {
        let mut tag = tag.clone();
        sanitize_picture_descriptions(&mut tag);
        let mut id3: lofty::id3::v2::Id3v2Tag = tag.into();
        if let Some(r) = rating {
            set_id3_rating(&mut id3, r);
        }
        save_id3(id3, path, what)
    } else {
        save_via_copy(path, what, |tmp| {
            tag.save_to_path(tmp, write_options(path)).with_context(err_ctx)?;
            normalize_id3_chunk_id(tmp);
            Ok(())
        })
    }
}

/// Runs a tag save on a copy beside the original and only swaps a complete,
/// non-empty result into place.
///
/// lofty rewrites a tag by reading the whole file, truncating it to 0 bytes
/// and writing everything back. Anything that interrupts those seconds — a
/// pulled USB stick, a crash, a write error — leaves an empty or cut-off
/// file; a 45 MB AIFF on the acceptance stick was found at 0 bytes
/// (2026-09-24). Here that failure hits the copy, and `enhance::swap_in`
/// moves the original aside and restores it if the final rename fails.
///
/// A read-only file is refused with a clear error rather than replaced: the
/// rename would otherwise succeed where the old in-place write refused.
/// A file held open elsewhere (Serato) fails at the rename, original intact.
fn save_via_copy(path: &Path, what: &str, save: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let meta = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    if meta.permissions().readonly() {
        bail!("cannot write {what}: {} is read-only", path.display());
    }
    let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    // The audio extension stays last so lofty still recognises the format.
    let ext = path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = path.with_file_name(format!("{name}.spindex-tmp.tag.{ext}"));
    std::fs::copy(path, &tmp)
        .with_context(|| format!("copying {} before writing {what} (disk full?)", path.display()))?;
    let result = save(&tmp).and_then(|()| {
        let len = std::fs::metadata(&tmp)?.len();
        if len == 0 {
            bail!("writing {what} produced an empty file; {} left unchanged", path.display());
        }
        crate::core::enhance::swap_in(&tmp, path, None, true)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Finishes an ID3v2 save: the repairs every write must run, then the write
/// itself, then the chunk-id fix. Shared by `save_tagged` and
/// `write_serato_beatgrid` so neither can drift from the other.
///
/// Callers MUST run `sanitize_picture_descriptions` on the generic `Tag`
/// *before* converting it to `Id3v2Tag` — that step is not inside `save_id3`,
/// and skipping it lets a `None` picture description poison every APIC frame
/// (see `sanitize_picture_descriptions`).
///
/// * `dedupe_geob_frames` — runs on every ID3v2 save so files damaged by an
///   earlier chunk merge heal on their next tag write; a no-op otherwise.
/// * `force_latin1_picture_frames` — only v2.3 needs it: v2.3 forbids UTF-8, so
///   lofty's UTF-8 picture frame degrades to UTF-16, which Serato ignores. A
///   v2.4 save keeps lofty's UTF-8, the form of the untouched files Serato does
///   display.
fn save_id3(mut id3: lofty::id3::v2::Id3v2Tag, path: &Path, what: &str) -> Result<()> {
    let err_ctx = || {
        format!(
            "writing {what} to {} (is the file read-only or in use?)",
            path.display()
        )
    };
    dedupe_geob_frames(&mut id3);
    if wants_id3v23(path) {
        force_latin1_picture_frames(&mut id3);
    }
    save_via_copy(path, what, |tmp| {
        id3.save_to_path(tmp, write_options(path)).with_context(err_ctx)?;
        normalize_id3_chunk_id(tmp);
        Ok(())
    })
}

/// Parses GEOB frame bytes into its object, or `None` if they do not parse.
///
/// GEOB reaches us as `Frame::Binary` — lofty deliberately leaves rare frames
/// unparsed — so the descriptor Serato keys its blocks by is only reachable by
/// parsing the payload. Callers are responsible for only passing GEOB data;
/// `BinaryFrame` keeps its header private, so the frame id has to be checked
/// through the enclosing `Frame`.
fn parse_geob(data: &[u8]) -> Option<lofty::id3::v2::GeneralEncapsulatedObject<'static>> {
    lofty::id3::v2::GeneralEncapsulatedObject::parse(data, lofty::id3::v2::FrameFlags::default())
        .ok()
}

/// Collapses GEOB frames that share a descriptor, keeping the **last**.
///
/// Serato stores its beatgrid, cues and overview as GEOB frames keyed by
/// descriptor (`Serato BeatGrid`, `Serato Markers2`, …). Consolidating a file
/// that carried two ID3 chunks merges both chunks' frames, and because
/// `Id3v2Tag::insert` only replaces a frame it is byte-for-byte equal to, two
/// beatgrids with *different* payloads both survive — real files on the
/// acceptance stick ended up with 3x `Serato BeatGrid` and 2x
/// `Serato Markers2`. A tag holding several conflicting beatgrids is not a
/// state Serato can resolve.
///
/// Last wins, for the same reason the rest of a merged tag resolves that way:
/// lofty returns the value from the chunk it parsed last, so keeping the last
/// frame preserves what readers see today rather than reviving a stale block.
///
/// Deliberately conservative — a frame whose descriptor is absent or
/// unparseable is never dropped, and a descriptor that appears once is left
/// exactly where it was, so a healthy single-chunk file is untouched.
fn dedupe_geob_frames(id3: &mut lofty::id3::v2::Id3v2Tag) {
    use lofty::id3::v2::{Frame, FrameId};
    const GEOB: FrameId<'static> = FrameId::Valid(std::borrow::Cow::Borrowed("GEOB"));

    let frames: Vec<Frame<'static>> = id3.remove(&GEOB).collect();

    // Count first: only descriptors seen more than once are candidates, so the
    // common case rebuilds the tag exactly as it was.
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for frame in &frames {
        if let Frame::Binary(b) = frame {
            if let Some(descriptor) = parse_geob(&b.data).and_then(|g| g.descriptor) {
                *seen.entry(descriptor).or_default() += 1;
            }
        }
    }

    let mut remaining = seen.clone();
    for frame in frames {
        let descriptor = match &frame {
            Frame::Binary(b) => parse_geob(&b.data).and_then(|g| g.descriptor),
            _ => None,
        };
        match descriptor {
            Some(d) if seen.get(&d).copied().unwrap_or(0) > 1 => {
                let left = remaining.entry(d).or_default();
                *left -= 1;
                // Keep only the final occurrence.
                if *left == 0 {
                    id3.insert(frame);
                }
            }
            _ => {
                id3.insert(frame);
            }
        }
    }
}

/// Writes every APIC description as ISO-8859-1.
///
/// lofty builds picture frames with `TextEncoding::UTF8`, which ID3v2.3 does
/// not allow, so its writer substitutes UTF-16 — and an empty description then
/// encodes as a bare byte-order mark (`fffe`). That is legal ID3, but no common
/// tagger writes it, and **Serato silently ignores such a frame**: covers saved
/// by SpinDex never appeared, while every other file on the same stick (latin1
/// or UTF-8 descriptions) displayed fine. Latin1 keeps its own encoding through
/// the v2.3 downgrade, where an empty description is the conventional single
/// 0x00 — the exact form of the files Serato does read.
///
/// Descriptions are always empty here (see `sanitize_picture_descriptions`), so
/// nothing can be lost to latin1's narrower character range.
fn force_latin1_picture_frames(id3: &mut lofty::id3::v2::Id3v2Tag) {
    use lofty::id3::v2::{Frame, FrameId};
    const APIC: FrameId<'static> = FrameId::Valid(std::borrow::Cow::Borrowed("APIC"));
    let pictures: Vec<Frame<'static>> = id3.remove(&APIC).collect();
    for frame in pictures {
        match frame {
            Frame::Picture(mut picture) => {
                picture.encoding = TextEncoding::Latin1;
                id3.insert(Frame::Picture(picture));
            }
            other => {
                id3.insert(other);
            }
        }
    }
}

/// lofty writes a `None` picture description as a single 0x00 terminator
/// regardless of text encoding. Our ID3v2.3 saves upgrade picture frames to
/// UTF-16, whose reader demands a BOM — the mismatch makes the whole tag
/// unreadable ("UTF-16 string has an invalid byte order mark") in every
/// parse mode. An empty-string description encodes as BOM + terminator and
/// round-trips, so pictures are rebuilt with `Some("")` before every save.
fn sanitize_picture_descriptions(tag: &mut Tag) {
    if !tag.pictures().iter().any(|p| p.description().is_none()) {
        return;
    }
    let pics: Vec<Picture> = tag.pictures().to_vec();
    while !tag.pictures().is_empty() {
        tag.remove_picture(0);
    }
    for p in pics {
        let desc = Some(p.description().unwrap_or("").to_string());
        tag.push_picture(Picture::new_unchecked(
            p.pic_type(),
            p.mime_type().cloned(),
            desc,
            p.data().to_vec(),
        ));
    }
}

/// The field names an update actually carries a value for.
///
/// Exporting SpinDex's metadata into a file must never BLANK a tag the file
/// already has just because SpinDex holds nothing there — an unanalyzed track
/// has no BPM or key, and clearing the file's own would be destroying data
/// rather than exporting it. So only fields SpinDex knows are written; the
/// rest are left exactly as the file has them.
pub fn fields_with_values(update: &TagUpdate) -> Vec<String> {
    let text = |v: &Option<String>| v.as_deref().map(str::trim).is_some_and(|s| !s.is_empty());
    let mut out = Vec::new();
    if text(&update.title) {
        out.push("title".to_string());
    }
    if text(&update.artist) {
        out.push("artist".to_string());
    }
    if text(&update.album) {
        out.push("album".to_string());
    }
    if text(&update.genre) {
        out.push("genre".to_string());
    }
    if text(&update.label) {
        out.push("label".to_string());
    }
    if update.year.is_some() {
        out.push("year".to_string());
    }
    if update.bpm.is_some() {
        out.push("bpm".to_string());
    }
    if text(&update.key) {
        out.push("key".to_string());
    }
    if text(&update.comment) {
        out.push("comment".to_string());
    }
    if update.rating.is_some() {
        out.push("rating".to_string());
    }
    out
}

/// Whether writing `update`'s `apply` fields would leave the file unchanged.
///
/// The export sweep asks this so a crate whose tags already match is not
/// rewritten — and rewriting a WAV rewrites the whole multi-megabyte file, so
/// on a USB stick this is the difference between touching a few files and
/// moving gigabytes.
pub fn tags_already_match(meta: &TrackMeta, update: &TagUpdate, apply: &[String]) -> bool {
    let same_text = |a: &Option<String>, b: &Option<String>| {
        a.as_deref().unwrap_or("").trim() == b.as_deref().unwrap_or("").trim()
    };
    apply.iter().all(|f| match f.as_str() {
        "title" => same_text(&meta.title, &update.title),
        "artist" => same_text(&meta.artist, &update.artist),
        "album" => same_text(&meta.album, &update.album),
        "genre" => same_text(&meta.genre, &update.genre),
        "label" => same_text(&meta.label, &update.label),
        "key" => same_text(&meta.key, &update.key),
        "comment" => same_text(&meta.comment, &update.comment),
        "year" => meta.year == update.year,
        "rating" => meta.rating == update.rating,
        // BPM round-trips through the tag as text, so compare at the precision
        // a tag actually preserves rather than as exact floats.
        "bpm" => match (meta.bpm, update.bpm) {
            (Some(a), Some(b)) => (a - b).abs() < 0.05,
            (a, b) => a.is_none() && b.is_none(),
        },
        _ => true,
    })
}

pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Read-path probe. Real-world libraries are full of sloppy tags (non-digit
/// timestamps, odd-length UTF-16, bad year fields — 549 of 15119 files on the
/// acceptance USB stick). Relaxed mode discards the invalid field instead of
/// rejecting the whole file; write paths keep lofty's stricter default so a
/// save never persists a silently mangled tag.
fn probe_relaxed(path: &Path) -> Result<TaggedFile> {
    let relaxed = Probe::open(path)
        .with_context(|| format!("probing {}", path.display()))?
        .options(ParseOptions::new().parsing_mode(ParsingMode::Relaxed))
        .read();
    match relaxed {
        Ok(tagged) => Ok(tagged),
        Err(err) => {
            // Some tag corruption (e.g. undecodable text) is fatal to lofty in
            // every parsing mode. Retry without tags so the file is at least
            // indexed with its audio properties instead of disappearing.
            tracing::warn!(
                "tag unreadable in {}, indexing without tags: {err}",
                path.display()
            );
            Probe::open(path)
                .with_context(|| format!("probing {}", path.display()))?
                .options(ParseOptions::new().read_tags(false))
                .read()
                .with_context(|| format!("reading {}", path.display()))
        }
    }
}

pub fn read(path: &Path) -> Result<TrackMeta> {
    let tagged = probe_relaxed(path)?;

    let props = tagged.properties();
    let mut meta = TrackMeta {
        duration_secs: Some(props.duration().as_secs_f64()),
        bitrate_kbps: props.audio_bitrate().map(|b| b as i64),
        sample_rate: props.sample_rate().map(|s| s as i64),
        format: path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase()),
        ..TrackMeta::default()
    };

    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    if let Some(tag) = tag {
        meta.title = tag.title().map(|s| s.to_string());
        meta.artist = tag.artist().map(|s| s.to_string());
        meta.album = tag.album().map(|s| s.to_string());
        meta.genre = tag.genre().map(|s| s.to_string());
        meta.comment = tag.comment().map(|s| s.to_string());
        meta.year = tag.year().map(|y| y as i64);
        meta.label = tag
            .get_string(&ItemKey::Label)
            .or_else(|| tag.get_string(&ItemKey::Publisher))
            .map(|s| s.to_string());
        meta.key = tag.get_string(&ItemKey::InitialKey).map(|s| s.to_string());
        // lofty 0.22 maps TBPM to IntegerBpm; older/odd tags may still land
        // on Bpm, so check both.
        meta.bpm = tag
            .get_string(&ItemKey::IntegerBpm)
            .or_else(|| tag.get_string(&ItemKey::Bpm))
            .and_then(|s| s.trim().parse::<f64>().ok());
        if tag.tag_type() != lofty::tag::TagType::Id3v2 {
            meta.rating = tag.get_string(&ItemKey::Popularimeter).and_then(stars_from_text);
        }
        meta.has_artwork = !tag.pictures().is_empty();
    }
    // ID3 ratings live in the concrete tag. Moved out rather than cloned, so a
    // scan never copies every file's artwork just to read one byte.
    let id3_tag = tag.map(|t| t.tag_type()) == Some(lofty::tag::TagType::Id3v2);
    if id3_tag {
        let mut tagged = tagged;
        if let Some(t) = tagged.remove(lofty::tag::TagType::Id3v2) {
            let id3: lofty::id3::v2::Id3v2Tag = t.into();
            meta.rating = id3_rating(&id3);
        }
    }

    Ok(meta)
}

/// The source file's bit depth (lossless/PCM formats only; None for lossy
/// codecs or on any probe failure). Used by the enhance engine to pick the
/// matching PCM encoder (pcm_s16le vs pcm_s24le, etc.) when re-encoding.
pub fn bit_depth(path: &Path) -> Option<u8> {
    probe_relaxed(path).ok()?.properties().bit_depth()
}

/// Fields the metadata editor can change. `None`/empty string clears the tag.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct TagUpdate {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<i64>,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub comment: Option<String>,
    pub label: Option<String>,
    /// Star rating 1-5; None clears it.
    #[serde(default)]
    pub rating: Option<i64>,
}

fn clean(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl TagUpdate {
    /// Trims strings (empty -> None) and drops non-positive numbers, so the
    /// file tags and the database row always receive identical values.
    pub fn normalized(&self) -> TagUpdate {
        TagUpdate {
            title: clean(&self.title),
            artist: clean(&self.artist),
            album: clean(&self.album),
            genre: clean(&self.genre),
            year: self.year.filter(|y| *y > 0),
            bpm: self.bpm.filter(|b| *b > 0.0),
            key: clean(&self.key),
            comment: clean(&self.comment),
            label: clean(&self.label),
            rating: self.rating.filter(|r| (1..=5).contains(r)),
        }
    }
}

/// The POPM owner SpinDex writes its rating under. Windows Explorer and Windows
/// Media Player read this one, and most taggers write the same 1/64/128/196/255
/// scale, so it is the most widely understood choice.
const POPM_EMAIL: &str = "Windows Media Player 9 Series";

/// A POPM byte (0-255, 0 = unrated) as stars, using the common thresholds so
/// ratings written by other programs on slightly different scales still land
/// on the star they meant.
fn stars_from_popm(r: u8) -> Option<i64> {
    match r {
        0 => None,
        1..=31 => Some(1),
        32..=95 => Some(2),
        96..=159 => Some(3),
        160..=223 => Some(4),
        _ => Some(5),
    }
}

fn popm_from_stars(stars: i64) -> u8 {
    match stars {
        ..=1 => 1,
        2 => 64,
        3 => 128,
        4 => 196,
        _ => 255,
    }
}

/// A text rating (Vorbis `RATING`, MP4 `rate`) as stars. Taggers disagree on
/// the scale: 1-5 is taken as stars, anything above as 0-100.
fn stars_from_text(s: &str) -> Option<i64> {
    let v: f64 = s.trim().parse().ok()?;
    if !v.is_finite() || v <= 0.0 {
        return None;
    }
    let stars = if v <= 5.0 { v.round() } else { (v / 20.0).round() };
    Some((stars as i64).clamp(1, 5))
}

const POPM: lofty::id3::v2::FrameId<'static> =
    lofty::id3::v2::FrameId::Valid(std::borrow::Cow::Borrowed("POPM"));

/// The star rating in an ID3 tag. lofty keeps POPM frames out of its generic
/// tag view, so this reads the concrete tag. SpinDex's own frame wins, else
/// the first other program's rating.
fn id3_rating(id3: &lofty::id3::v2::Id3v2Tag) -> Option<i64> {
    let frames: Vec<&lofty::id3::v2::PopularimeterFrame<'_>> = id3
        .into_iter()
        .filter_map(|f| match f {
            lofty::id3::v2::Frame::Popularimeter(p) => Some(p),
            _ => None,
        })
        .collect();
    let ours = frames.iter().find(|f| f.email == POPM_EMAIL).and_then(|f| stars_from_popm(f.rating));
    ours.or_else(|| frames.iter().find_map(|f| stars_from_popm(f.rating)))
}

/// Sets (or clears) the star rating in an ID3 tag. The rating goes into EVERY
/// POPM frame, not just SpinDex's own: otherwise a cleared rating would come
/// back from another program's frame on the next read. Play counters are
/// kept, and a frame left with neither a rating nor a count is dropped.
fn set_id3_rating(id3: &mut lofty::id3::v2::Id3v2Tag, stars: Option<i64>) {
    let value = stars.map(popm_from_stars).unwrap_or(0);
    let mut frames: Vec<lofty::id3::v2::PopularimeterFrame<'static>> = id3
        .remove(&POPM)
        .filter_map(|f| match f {
            lofty::id3::v2::Frame::Popularimeter(p) => Some(p),
            _ => None,
        })
        .collect();
    for f in &mut frames {
        f.rating = value;
    }
    if value != 0 && !frames.iter().any(|f| f.email == POPM_EMAIL) {
        frames.push(lofty::id3::v2::PopularimeterFrame::new(POPM_EMAIL.to_string(), value, 0));
    }
    for f in frames.into_iter().filter(|f| f.rating != 0 || f.counter != 0) {
        id3.insert(lofty::id3::v2::Frame::Popularimeter(f));
    }
}

/// Sets (or clears) the rating in a non-ID3 tag (Vorbis `RATING`, MP4 `rate`)
/// on the 0-100 scale. ID3 ratings are set at save time instead, on the
/// concrete tag — see `set_id3_rating`.
fn set_text_rating(tag: &mut Tag, stars: Option<i64>) {
    if tag.tag_type() == lofty::tag::TagType::Id3v2 {
        return;
    }
    match stars {
        Some(s) => {
            tag.insert_text(ItemKey::Popularimeter, (s * 20).to_string());
        }
        None => tag.remove_key(&ItemKey::Popularimeter),
    }
}

/// Serato (and some store rips) omit the spec-mandated pad byte after a
/// trailing odd-sized chunk in AIFF/WAV files. lofty 0.22 assumes the pad
/// exists when replacing the ID3 chunk and slices one byte past EOF — a
/// panic, and a panic in a sync Tauri command aborts the whole app (it can't
/// unwind through the Win32 event-loop callback). Walk the chunk list and
/// append the missing pad byte before handing the file to lofty; one zero
/// byte after the final chunk, audio data untouched. 38/304 files on the
/// acceptance stick needed this.
/// Rebuilds a RIFF/AIFF container without any ID3 chunk. Returns None when the
/// file does not carry more than one — nothing to consolidate, don't touch it.
///
/// RIFF/AIFF chunk ids are case-sensitive four-character codes, and taggers
/// disagree about which case to use for ID3: Serato writes `id3 `, lofty
/// writes `ID3 `. A file can therefore end up with BOTH, holding different
/// values for the same field.
fn strip_id3_chunks(bytes: &[u8], big_endian: bool) -> Option<Vec<u8>> {
    let magic: &[u8] = if big_endian { b"FORM" } else { b"RIFF" };
    if bytes.len() < 12 || &bytes[0..4] != magic {
        return None;
    }
    let mut out = bytes[0..12].to_vec();
    let mut found = 0usize;
    let mut i = 12usize;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let raw = [bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]];
        let size = if big_endian {
            u32::from_be_bytes(raw)
        } else {
            u32::from_le_bytes(raw)
        } as usize;
        let total = 8usize.saturating_add(size).saturating_add(size % 2);
        let end = i.saturating_add(total).min(bytes.len());
        if id.eq_ignore_ascii_case(b"id3 ") {
            found += 1;
        } else {
            out.extend_from_slice(&bytes[i..end]);
        }
        if end <= i {
            return None; // zero-length step: malformed, leave the file alone
        }
        i = end;
    }
    if found < 2 {
        return None;
    }
    let container = (out.len() - 8) as u32;
    let size_bytes = if big_endian {
        container.to_be_bytes()
    } else {
        container.to_le_bytes()
    };
    out[4..8].copy_from_slice(&size_bytes);
    Some(out)
}

/// Renames the (single) ID3 chunk in `path` to `want`. Byte-for-byte identical
/// otherwise — only the four id bytes change, so no size field moves.
fn rewrite_id3_chunk_id(path: &Path, big_endian: bool, want: &[u8; 4]) -> std::io::Result<()> {
    let mut bytes = std::fs::read(path)?;
    let mut i = 12usize;
    while i + 8 <= bytes.len() {
        let raw = [bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]];
        let size = if big_endian {
            u32::from_be_bytes(raw)
        } else {
            u32::from_le_bytes(raw)
        } as usize;
        if bytes[i..i + 4].eq_ignore_ascii_case(b"id3 ") {
            if &bytes[i..i + 4] != want {
                bytes[i..i + 4].copy_from_slice(want);
                std::fs::write(path, &bytes)?;
            }
            return Ok(());
        }
        let step = 8usize.saturating_add(size).saturating_add(size % 2);
        if step == 0 {
            break;
        }
        i = i.saturating_add(step);
    }
    Ok(())
}

/// Merges duplicate ID3 chunks into one before any tag write.
///
/// lofty updates only ONE of a file's ID3 chunks and reports whichever it
/// parses last, so a file with two of them answers differently depending on
/// chunk order: on a real stick file a saved key read back as `1A` right after
/// the write and as `G#m` after the chunks were reordered. Every tag edit then
/// looks like it saved and silently reverted, and the chunk Serato reads never
/// gets the new value at all. Measured on the acceptance stick: 18 of 208
/// WAV/AIFF files carry duplicates, three of them carry three.
///
/// The merge is the tag lofty already produces across the chunks (Serato's
/// GEOB beatgrids/cues ride along in it), written back onto a container with
/// every ID3 chunk removed. Best-effort like the other pre-write repairs: any
/// failure leaves the original file exactly as it was.
/// Whether re-saving this file's tags would actually change anything.
///
/// The crate-wide repair sweep asks this first so it only rewrites files that
/// need it. That matters more than it sounds: rewriting a WAV rewrites the
/// whole file, so a blanket sweep of a 137-track WAV library moves gigabytes
/// across a USB stick to fix a handful of tags.
///
/// Three conditions, all of them defects we have shipped fixes for:
/// * more than one ID3 chunk in a RIFF/AIFF container — readers disagree about
///   which tag is the file's (see `repair_duplicate_id3_chunks`);
/// * the ID3 chunk sitting under the wrong four-character id for its container
///   — a WAV tag in `ID3 ` is invisible to Serato (see `chunk_container`);
/// * an `APIC` whose description is UTF-16 — Serato silently ignores such a
///   picture frame (see `force_latin1_picture_frames`).
///
/// Deliberately NOT a condition: the tag's ID3 minor version on its own. A
/// correctly-placed chunk is readable either way, and flagging on version would
/// rewrite an entire library to no visible purpose — rewriting a WAV rewrites
/// the whole file. Unreadable files answer `false`, so a sweep is never the
/// thing that decides to rewrite a broken file.
pub fn needs_tag_repair(path: &Path) -> bool {
    needs_tag_repair_inner(path).unwrap_or(false)
}

fn needs_tag_repair_inner(path: &Path) -> std::io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom};

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let mut f = std::fs::File::open(path)?;

    let tag_start: u64 = match ext.as_deref() {
        Some("mp3") => 0,
        Some("wav" | "aif" | "aiff") => {
            let big_endian = !matches!(ext.as_deref(), Some("wav"));
            let len = f.metadata()?.len();
            let mut hdr = [0u8; 12];
            if f.read_exact(&mut hdr).is_err() {
                return Ok(false);
            }
            if &hdr[..4] != if big_endian { b"FORM" } else { b"RIFF" } {
                return Ok(false);
            }
            let mut pos: u64 = 12;
            let mut first: Option<u64> = None;
            let mut chunks = 0usize;
            let mut wrong_chunk_id = false;
            while pos + 8 <= len {
                f.seek(SeekFrom::Start(pos))?;
                let mut ch = [0u8; 8];
                if f.read_exact(&mut ch).is_err() {
                    break;
                }
                let raw = [ch[4], ch[5], ch[6], ch[7]];
                let size = if big_endian {
                    u32::from_be_bytes(raw)
                } else {
                    u32::from_le_bytes(raw)
                };
                if ch[..4].eq_ignore_ascii_case(b"id3 ") {
                    chunks += 1;
                    first.get_or_insert(pos + 8);
                    if chunk_container(path).is_some_and(|(_, want)| ch[..4] != want) {
                        wrong_chunk_id = true;
                    }
                }
                pos += 8 + u64::from(size) + u64::from(size & 1);
            }
            if chunks > 1 {
                return Ok(true); // duplicate ID3 chunks
            }
            if wrong_chunk_id {
                // A WAV tag in an uppercase `ID3 ` chunk is invisible to Serato
                // (and an AIFF tag in a lowercase one likewise) — the whole
                // reason older SpinDex writes never showed up there.
                return Ok(true);
            }
            match first {
                Some(p) => p,
                None => return Ok(false),
            }
        }
        _ => return Ok(false),
    };

    f.seek(SeekFrom::Start(tag_start))?;
    let mut th = [0u8; 10];
    if f.read_exact(&mut th).is_err() {
        return Ok(false);
    }
    if &th[..3] != b"ID3" || th[5] != 0 {
        return Ok(false);
    }
    let version = th[3];
    // Frame sizes are syncsafe from v2.4 on, plain big-endian before it.
    let syncsafe_sizes = version >= 4;
    // A RIFF/AIFF container needs v2.4 for its ARTWORK to be readable by
    // Serato — but only then. A v2.3 WAV with no picture shows nothing either
    // way, and the next artwork save converts it, so rewriting it now would
    // move gigabytes across a stick for no visible change.
    let stale_chunk_version = !wants_id3v23(path) && version < 4;
    let tag_size = (u64::from(th[6]) << 21)
        | (u64::from(th[7]) << 14)
        | (u64::from(th[8]) << 7)
        | u64::from(th[9]);
    if tag_size > 64 * 1024 * 1024 {
        return Ok(false);
    }
    let mut body = vec![0u8; tag_size as usize];
    if f.read_exact(&mut body).is_err() {
        return Ok(false);
    }

    let mut pos = 0usize;
    let mut geob_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    while pos + 10 <= body.len() {
        if body[pos] == 0 {
            break; // padding
        }
        let raw = [body[pos + 4], body[pos + 5], body[pos + 6], body[pos + 7]];
        let size = if syncsafe_sizes {
            ((raw[0] as usize) << 21)
                | ((raw[1] as usize) << 14)
                | ((raw[2] as usize) << 7)
                | raw[3] as usize
        } else {
            u32::from_be_bytes(raw) as usize
        };
        let start = pos + 10;
        let end = start.saturating_add(size).min(body.len());
        if &body[pos..pos + 4] == b"APIC" && start < end {
            // UTF-16 (encoding byte 1) is the description form Serato ignores,
            // in any tag version — the one picture defect a v2.4 tag can hold.
            if body[start] == 1 {
                return Ok(true);
            }
            if stale_chunk_version {
                return Ok(true);
            }
        }
        if &body[pos..pos + 4] == b"GEOB" && start < end {
            if let Some(d) = geob_descriptor(&body[start..end]) {
                if !geob_seen.insert(d) {
                    // Two Serato blocks under one descriptor: see
                    // `dedupe_geob_frames`. Not a state Serato can resolve.
                    return Ok(true);
                }
            }
        }
        pos = end;
    }
    Ok(false)
}

/// The descriptor of a GEOB frame body, for duplicate detection only.
///
/// Layout is `encoding, mime\0, filename\0, descriptor\0, data`. Only the
/// single-byte encodings (latin1 and UTF-8) are read — UTF-16 terminates its
/// fields with two zero bytes, and Serato writes latin1 in practice, so the
/// extra parsing would buy nothing. An unreadable frame yields `None` and is
/// simply not considered a duplicate.
fn geob_descriptor(body: &[u8]) -> Option<String> {
    let (&encoding, rest) = body.split_first()?;
    if encoding != 0 && encoding != 3 {
        return None;
    }
    let mut fields = rest.splitn(4, |&b| b == 0);
    fields.next()?; // mime type
    fields.next()?; // file name
    let descriptor = fields.next()?;
    Some(String::from_utf8_lossy(descriptor).into_owned())
}

fn repair_duplicate_id3_chunks(path: &Path) {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let big_endian = match ext.as_deref() {
        Some("aif" | "aiff") => true,
        Some("wav") => false,
        _ => return,
    };

    let Ok(bytes) = std::fs::read(path) else { return };
    let Some(stripped) = strip_id3_chunks(&bytes, big_endian) else { return };

    // The tag to keep is the one a read produces TODAY, so consolidating never
    // changes what SpinDex reports for the file — it only makes every other
    // reader agree with it.
    let Ok(tagged) = probe_relaxed(path) else { return };
    if tagged.primary_tag().is_none() {
        return;
    }

    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let dir = match path.parent() {
        Some(d) => d.to_path_buf(),
        None => return,
    };
    let tmp = dir.join(format!("{stem}.spindex-tmp.{}", ext.as_deref().unwrap_or("tmp")));

    if std::fs::write(&tmp, &stripped).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if let Err(e) = save_tagged(&tagged, &tmp, "merged ID3 chunks") {
        tracing::warn!("consolidating ID3 chunks in {}: {e:#}", path.display());
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    // `save_tagged` already put the surviving chunk under the id this container
    // calls for (`chunk_container`), so the merge needs no id decision of its
    // own — one rule, not a heuristic that could disagree with the writer.
    if let Err(e) = std::fs::rename(&tmp, path) {
        tracing::warn!("replacing {} after ID3 merge: {e}", path.display());
        let _ = std::fs::remove_file(&tmp);
    }
}

fn repair_trailing_chunk_pad(path: &Path) {
    use std::io::{Read, Seek, SeekFrom, Write};

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let big_endian = match ext.as_deref() {
        Some("aif" | "aiff") => true,
        Some("wav") => false,
        _ => return,
    };

    let needs_pad = (|| -> std::io::Result<bool> {
        let mut f = std::fs::File::open(path)?;
        let len = f.metadata()?.len();
        let mut hdr = [0u8; 12];
        f.read_exact(&mut hdr)?;
        if (big_endian && &hdr[..4] != b"FORM") || (!big_endian && &hdr[..4] != b"RIFF") {
            return Ok(false);
        }
        let mut pos: u64 = 12;
        while pos + 8 <= len {
            f.seek(SeekFrom::Start(pos))?;
            let mut ch = [0u8; 8];
            f.read_exact(&mut ch)?;
            let raw = [ch[4], ch[5], ch[6], ch[7]];
            let size = if big_endian {
                u32::from_be_bytes(raw)
            } else {
                u32::from_le_bytes(raw)
            };
            let padded = u64::from(size) + u64::from(size & 1);
            let next = pos + 8 + padded;
            if next == len + 1 && size % 2 == 1 {
                return Ok(true); // final chunk odd-sized, pad byte truncated
            }
            if next >= len {
                return Ok(false); // well-formed end, or corrupt beyond repair
            }
            pos = next;
        }
        Ok(false)
    })()
    .unwrap_or(false);

    if needs_pad {
        // Append the pad byte AND correct the FORM/RIFF size field: Serato
        // also writes the container size without the pad, and lofty derives
        // the insert position of the replacement chunk from that field — one
        // short and the new tag lands a byte early, unreadable.
        let repaired = (|| -> std::io::Result<()> {
            let mut f = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
            f.seek(SeekFrom::End(0))?;
            f.write_all(&[0])?;
            let container_size = f.metadata()?.len().saturating_sub(8) as u32;
            f.seek(SeekFrom::Start(4))?;
            let bytes = if big_endian {
                container_size.to_be_bytes()
            } else {
                container_size.to_le_bytes()
            };
            f.write_all(&bytes)?;
            Ok(())
        })();
        match repaired {
            Ok(()) => tracing::info!(
                "appended missing IFF pad byte to {} before tag write",
                path.display()
            ),
            // Read-only etc. — lofty's own open-for-write will report it.
            Err(e) => tracing::warn!("could not repair pad byte in {}: {e}", path.display()),
        }
    }
}

/// Repairs files poisoned by the pre-fix ID3v2.3 writer (SpinDex saves after
/// the 2026-07-22 v2.3 switch, before `sanitize_picture_descriptions`): an
/// APIC frame carrying a UTF-16 encoding byte but a latin1-style single-0x00
/// empty description. lofty fails the WHOLE tag on it in every parse mode,
/// so the file could never be edited again. Fix: flip that APIC's encoding
/// byte to latin1 — the mime string is latin1 already and the single 0x00 is
/// exactly a valid latin1 empty description. One byte, tag readable again.
fn repair_apic_empty_desc(path: &Path) {
    use std::io::{Read, Seek, SeekFrom, Write};

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());

    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::open(path)?;
        // absolute offset of the "ID3" tag header
        let tag_start: u64 = match ext.as_deref() {
            Some("mp3") => 0,
            Some("wav" | "aif" | "aiff") => {
                let big_endian = !matches!(ext.as_deref(), Some("wav"));
                let len = f.metadata()?.len();
                let mut hdr = [0u8; 12];
                f.read_exact(&mut hdr)?;
                if &hdr[..4] != if big_endian { b"FORM" } else { b"RIFF" } {
                    return Ok(());
                }
                let mut pos: u64 = 12;
                let mut found: Option<u64> = None;
                while pos + 8 <= len {
                    f.seek(SeekFrom::Start(pos))?;
                    let mut ch = [0u8; 8];
                    f.read_exact(&mut ch)?;
                    let raw = [ch[4], ch[5], ch[6], ch[7]];
                    let size = if big_endian {
                        u32::from_be_bytes(raw)
                    } else {
                        u32::from_le_bytes(raw)
                    };
                    if ch[..4].eq_ignore_ascii_case(b"id3 ") {
                        found = Some(pos + 8);
                        break;
                    }
                    pos += 8 + u64::from(size) + u64::from(size & 1);
                }
                match found {
                    Some(p) => p,
                    None => return Ok(()),
                }
            }
            _ => return Ok(()),
        };

        f.seek(SeekFrom::Start(tag_start))?;
        let mut th = [0u8; 10];
        if f.read_exact(&mut th).is_err() {
            return Ok(());
        }
        // only plain v2.3 tags without extended header — all we ever wrote
        if &th[..3] != b"ID3" || th[3] != 3 || th[5] != 0 {
            return Ok(());
        }
        let tag_size = (u64::from(th[6]) << 21)
            | (u64::from(th[7]) << 14)
            | (u64::from(th[8]) << 7)
            | u64::from(th[9]);
        if tag_size > 64 * 1024 * 1024 {
            return Ok(());
        }
        let mut body = vec![0u8; tag_size as usize];
        if f.read_exact(&mut body).is_err() {
            return Ok(());
        }

        let mut poisoned_offsets: Vec<u64> = Vec::new();
        let mut pos = 0usize;
        while pos + 10 <= body.len() {
            let id = &body[pos..pos + 4];
            if id[0] == 0 {
                break; // padding
            }
            if !id
                .iter()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            {
                break;
            }
            let size =
                u32::from_be_bytes([body[pos + 4], body[pos + 5], body[pos + 6], body[pos + 7]])
                    as usize;
            let frame = pos + 10;
            if frame + size > body.len() {
                break;
            }
            if id == b"APIC" && size > 2 {
                let b = &body[frame..frame + size];
                // encoding UTF-16, mime up to its null, picture type, then
                // the description: poisoned = single 0x00 not followed by a
                // second 0x00 (a valid empty UTF-16 string is BOM+0000)
                if b[0] == 1 {
                    if let Some(mime_end) = b[1..].iter().position(|&c| c == 0) {
                        let d = 1 + mime_end + 1 + 1; // past mime null + pic type
                        if d < b.len() && b[d] == 0 && (d + 1 >= b.len() || b[d + 1] != 0) {
                            poisoned_offsets.push(tag_start + 10 + frame as u64);
                        }
                    }
                }
            }
            pos = frame + size;
        }
        drop(f);

        if poisoned_offsets.is_empty() {
            return Ok(());
        }
        let mut f = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
        for off in &poisoned_offsets {
            f.seek(SeekFrom::Start(*off))?;
            f.write_all(&[0])?; // encoding byte -> latin1
        }
        tracing::info!(
            "repaired {} poisoned APIC frame(s) in {}",
            poisoned_offsets.len(),
            path.display()
        );
        Ok(())
    })();
    if let Err(e) = result {
        tracing::warn!("apic repair scan failed for {}: {e}", path.display());
    }
}

/// Runs a lofty read/save closure with a panic guard: tag parsers crash on
/// hostile real-world files, and a panic escaping a sync Tauri command kills
/// the entire app. A caught panic becomes a normal error for the editor.
pub(crate) fn guard_tag_panic<T>(path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            bail!(
                "the tag library crashed on {} ({msg}) — the file was not modified",
                path.display()
            )
        }
    }
}

/// Writes the update into the file's primary tag. The single `save_to_path`
/// call either fully succeeds or leaves the file untouched — read-only and
/// locked files surface as one clear error, never a partial write.
pub fn write(path: &Path, update: &TagUpdate) -> Result<()> {
    if !path.is_file() {
        bail!("file not found: {}", path.display());
    }
    repair_duplicate_id3_chunks(path);
    repair_trailing_chunk_pad(path);
    repair_apic_empty_desc(path);
    guard_tag_panic(path, || write_inner(path, update))
}

fn write_inner(path: &Path, update: &TagUpdate) -> Result<()> {
    let mut tagged = Probe::open(path)
        .with_context(|| format!("probing {}", path.display()))?
        .read()
        .with_context(|| format!("reading {}", path.display()))?;

    if tagged.primary_tag_mut().is_none() {
        let tag_type = tagged.file_type().primary_tag_type();
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged
        .primary_tag_mut()
        .context("no writable tag for this file type")?;

    match clean(&update.title) {
        Some(v) => tag.set_title(v),
        None => tag.remove_title(),
    }
    match clean(&update.artist) {
        Some(v) => tag.set_artist(v),
        None => tag.remove_artist(),
    }
    match clean(&update.album) {
        Some(v) => tag.set_album(v),
        None => tag.remove_album(),
    }
    match clean(&update.genre) {
        Some(v) => tag.set_genre(v),
        None => tag.remove_genre(),
    }
    match clean(&update.comment) {
        Some(v) => tag.set_comment(v),
        None => tag.remove_comment(),
    }
    match update.year {
        Some(y) if y > 0 => tag.set_year(y as u32),
        _ => tag.remove_year(),
    }
    match update.bpm {
        Some(b) if b > 0.0 => {
            // Which BPM key a tag accepts is format-dependent in lofty 0.22:
            // ID3v2 takes IntegerBpm (TBPM, integer by spec) and silently
            // rejects Bpm; Vorbis comments take Bpm. The checked insert
            // returns false on rejection, so try both in order.
            let s = format!("{}", b.round() as i64);
            if !tag.insert_text(ItemKey::IntegerBpm, s.clone()) {
                tag.insert_text(ItemKey::Bpm, s);
            }
        }
        _ => {
            tag.remove_key(&ItemKey::IntegerBpm);
            tag.remove_key(&ItemKey::Bpm);
        }
    }
    match clean(&update.key) {
        Some(v) => {
            tag.insert_text(ItemKey::InitialKey, v);
        }
        None => {
            tag.remove_key(&ItemKey::InitialKey);
        }
    }
    match clean(&update.label) {
        Some(v) => {
            tag.insert_text(ItemKey::Label, v);
        }
        None => {
            tag.remove_key(&ItemKey::Label);
        }
    }
    set_text_rating(tag, update.rating);

    save_tagged_rating(&tagged, path, "tags", Some(update.rating))
}

/// Writes ONLY the fields named in `apply` into the file's primary tag,
/// leaving every other tag in place. A listed field with an empty/None value
/// is cleared. Same AIFF-pad + panic protection as `write`. Used by batch
/// editing so untouched fields are never clobbered.
pub fn apply_partial(path: &Path, update: &TagUpdate, apply: &[String]) -> Result<()> {
    if !path.is_file() {
        bail!("file not found: {}", path.display());
    }
    repair_duplicate_id3_chunks(path);
    repair_trailing_chunk_pad(path);
    repair_apic_empty_desc(path);
    guard_tag_panic(path, || apply_partial_inner(path, update, apply))
}

fn apply_partial_inner(path: &Path, update: &TagUpdate, apply: &[String]) -> Result<()> {
    let mut tagged = Probe::open(path)
        .with_context(|| format!("probing {}", path.display()))?
        .read()
        .with_context(|| format!("reading {}", path.display()))?;
    if tagged.primary_tag_mut().is_none() {
        let tag_type = tagged.file_type().primary_tag_type();
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged
        .primary_tag_mut()
        .context("no writable tag for this file type")?;

    let has = |f: &str| apply.iter().any(|a| a == f);

    if has("title") {
        match clean(&update.title) {
            Some(v) => tag.set_title(v),
            None => tag.remove_title(),
        }
    }
    if has("artist") {
        match clean(&update.artist) {
            Some(v) => tag.set_artist(v),
            None => tag.remove_artist(),
        }
    }
    if has("album") {
        match clean(&update.album) {
            Some(v) => tag.set_album(v),
            None => tag.remove_album(),
        }
    }
    if has("genre") {
        match clean(&update.genre) {
            Some(v) => tag.set_genre(v),
            None => tag.remove_genre(),
        }
    }
    if has("comment") {
        match clean(&update.comment) {
            Some(v) => tag.set_comment(v),
            None => tag.remove_comment(),
        }
    }
    if has("year") {
        match update.year {
            Some(y) if y > 0 => tag.set_year(y as u32),
            _ => tag.remove_year(),
        }
    }
    if has("bpm") {
        match update.bpm {
            Some(b) if b > 0.0 => {
                let s = format!("{}", b.round() as i64);
                if !tag.insert_text(ItemKey::IntegerBpm, s.clone()) {
                    tag.insert_text(ItemKey::Bpm, s);
                }
            }
            _ => {
                tag.remove_key(&ItemKey::IntegerBpm);
                tag.remove_key(&ItemKey::Bpm);
            }
        }
    }
    if has("key") {
        match clean(&update.key) {
            Some(v) => {
                tag.insert_text(ItemKey::InitialKey, v);
            }
            None => {
                tag.remove_key(&ItemKey::InitialKey);
            }
        }
    }
    if has("label") {
        match clean(&update.label) {
            Some(v) => {
                tag.insert_text(ItemKey::Label, v);
            }
            None => {
                tag.remove_key(&ItemKey::Label);
            }
        }
    }
    if has("rating") {
        set_text_rating(tag, update.rating);
    }

    save_tagged_rating(&tagged, path, "tags", has("rating").then_some(update.rating))
}

/// First embedded picture as (mime, bytes), or None.
pub fn read_artwork(path: &Path) -> Result<Option<(String, Vec<u8>)>> {
    let tagged = probe_relaxed(path)?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    Ok(tag.and_then(|t| {
        t.pictures().first().map(|p| {
            let mime = p
                .mime_type()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "image/jpeg".to_string());
            (mime, p.data().to_vec())
        })
    }))
}

/// Replaces all embedded pictures with one front cover, or removes them all
/// when `image` is None. Same atomic save semantics as `write`.
pub fn write_artwork(path: &Path, image: Option<(Vec<u8>, MimeType)>) -> Result<()> {
    if !path.is_file() {
        bail!("file not found: {}", path.display());
    }
    repair_duplicate_id3_chunks(path);
    repair_trailing_chunk_pad(path);
    repair_apic_empty_desc(path);
    guard_tag_panic(path, || write_artwork_inner(path, image))
}

fn write_artwork_inner(path: &Path, image: Option<(Vec<u8>, MimeType)>) -> Result<()> {
    let mut tagged = Probe::open(path)
        .with_context(|| format!("probing {}", path.display()))?
        .read()
        .with_context(|| format!("reading {}", path.display()))?;
    if tagged.primary_tag_mut().is_none() {
        let tag_type = tagged.file_type().primary_tag_type();
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged
        .primary_tag_mut()
        .context("no writable tag for this file type")?;

    while !tag.pictures().is_empty() {
        tag.remove_picture(0);
    }
    if let Some((data, mime)) = image {
        // Some("") not None: a None description breaks UTF-16 ID3v2.3 tags,
        // see sanitize_picture_descriptions.
        tag.push_picture(Picture::new_unchecked(
            PictureType::CoverFront,
            Some(mime),
            Some(String::new()),
            data,
        ));
    }
    save_tagged(&tagged, path, "artwork")
}

/// Writes a Serato beat grid into the file, replacing any existing one and
/// leaving every other Serato frame (`Markers2`, `Overview`, `Autotags`, …)
/// untouched. Goes through the same save funnel as `write` so every container
/// repair runs, and is wrapped in the lofty panic guard.
///
/// * MP3 / WAV / AIFF: an ID3 `GEOB` frame `Serato BeatGrid`.
/// * FLAC: a base64 `SERATO_BEATGRID` Vorbis comment.
pub fn write_serato_beatgrid(
    path: &Path,
    grid: &crate::core::serato_beatgrid::Beatgrid,
) -> Result<()> {
    write_serato_data(path, Some(grid), None)
}

/// Writes Serato's Markers2 data (cues, loops, track colour, …) into the file,
/// replacing any existing Markers2 and leaving every other frame — the beat
/// grid, `Serato Markers_`, `Overview`, tags, artwork — untouched. Same save
/// funnel and panic guard as [`write_serato_beatgrid`].
///
/// * MP3 / WAV / AIFF: an ID3 `GEOB` frame `Serato Markers2`.
/// * FLAC: a base64 `SERATO_MARKERS_V2` Vorbis comment.
pub fn write_serato_markers(path: &Path, entries: &[crate::core::serato_markers::Entry]) -> Result<()> {
    write_serato_data(path, None, Some(entries))
}

/// Writes a Serato beat grid and/or Markers2 data in ONE save — rewriting a
/// file is a full copy (see `save_via_copy`), so an import that sets both
/// must not pay for it twice. Each given part replaces its own frame (by
/// descriptor) or Vorbis key; everything else in the tag stays as it was.
pub fn write_serato_data(
    path: &Path,
    grid: Option<&crate::core::serato_beatgrid::Beatgrid>,
    markers: Option<&[crate::core::serato_markers::Entry]>,
) -> Result<()> {
    if !path.is_file() {
        bail!("file not found: {}", path.display());
    }
    if grid.is_none() && markers.is_none() {
        return Ok(());
    }
    repair_duplicate_id3_chunks(path);
    repair_trailing_chunk_pad(path);
    repair_apic_empty_desc(path);
    guard_tag_panic(path, || write_serato_data_inner(path, grid, markers))
}

fn write_serato_data_inner(
    path: &Path,
    grid: Option<&crate::core::serato_beatgrid::Beatgrid>,
    markers: Option<&[crate::core::serato_markers::Entry]>,
) -> Result<()> {
    use crate::core::serato_beatgrid as sbg;
    use crate::core::serato_markers as sm;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    // Scoped to the containers whose Serato data lives in a tag. MP4
    // (m4a/aac/mp4) keeps it in an atom, not a GEOB frame, and forcing an
    // Id3v2Tag onto one would error or write a spurious ID3 tag.
    match ext.as_str() {
        "mp3" | "wav" | "aiff" | "aif" | "flac" => {}
        _ => bail!("Serato grid and cue writing is not supported for .{ext} files"),
    }

    if ext == "flac" {
        let mut tagged = Probe::open(path)
            .with_context(|| format!("probing {}", path.display()))?
            .read()
            .with_context(|| format!("reading {}", path.display()))?;
        if tagged.primary_tag_mut().is_none() {
            let tag_type = tagged.file_type().primary_tag_type();
            tagged.insert_tag(Tag::new(tag_type));
        }
        let tag = tagged.primary_tag_mut().context("no writable tag")?;
        // Plain string items under Serato's keys. The keys are not in lofty's
        // ItemKey map, so they go in unchecked — the generic save path writes
        // ItemKey::Unknown items for Vorbis comments.
        let mut put = |key: &str, value: String| {
            let key = ItemKey::Unknown(key.to_string());
            tag.remove_key(&key);
            tag.push_unchecked(lofty::tag::TagItem::new(key, lofty::tag::ItemValue::Text(value)));
        };
        if let Some(g) = grid {
            put(sbg::VORBIS_KEY, sbg::vorbis_value(g));
        }
        if let Some(m) = markers {
            put(sm::VORBIS_KEY, sm::vorbis_value(m));
        }
        return save_tagged(&tagged, path, "Serato data");
    }

    // ID3 containers: work on the concrete tag so single GEOB frames can be
    // dropped and re-added.
    let tagged = Probe::open(path)
        .with_context(|| format!("probing {}", path.display()))?
        .read()
        .with_context(|| format!("reading {}", path.display()))?;
    let mut base = tagged
        .primary_tag()
        .cloned()
        .unwrap_or_else(|| Tag::new(lofty::tag::TagType::Id3v2));
    // Same picture sanitization save_tagged runs before its own `.into()`.
    sanitize_picture_descriptions(&mut base);
    let mut id3: lofty::id3::v2::Id3v2Tag = base.into();

    use lofty::id3::v2::{BinaryFrame, Frame, FrameId};
    const GEOB: FrameId<'static> = FrameId::Valid(std::borrow::Cow::Borrowed("GEOB"));
    let kept: Vec<Frame<'static>> = id3
        .remove(&GEOB)
        .filter(|f| match f {
            Frame::Binary(b) => {
                let desc = parse_geob(&b.data).and_then(|g| g.descriptor);
                // a replaced grid: by descriptor, or any GEOB that parses as a grid
                let old_grid = grid.is_some()
                    && (desc.as_deref() == Some(sbg::DESCRIPTOR)
                        || matches!(sbg::grid_from_geob_body(&b.data), Ok(Some(_))));
                // replaced Markers2: by descriptor only
                let old_markers = markers.is_some() && desc.as_deref() == Some(sm::DESCRIPTOR);
                !old_grid && !old_markers
            }
            _ => true,
        })
        .collect();
    for f in kept {
        id3.insert(f);
    }
    if let Some(g) = grid {
        id3.insert(Frame::Binary(BinaryFrame::new(GEOB, sbg::geob_body(g))));
    }
    if let Some(m) = markers {
        // GEOB body: latin1 encoding, mime\0, empty filename\0, descriptor\0, payload
        let mut body = vec![0u8];
        body.extend_from_slice(b"application/octet-stream");
        body.push(0);
        body.push(0);
        body.extend_from_slice(sm::DESCRIPTOR.as_bytes());
        body.push(0);
        body.extend_from_slice(&sm::encode_payload(m));
        id3.insert(Frame::Binary(BinaryFrame::new(GEOB, body)));
    }
    save_id3(id3, path, "Serato data")
}

/// Reads the Serato beat grid currently stored in a file — the reader
/// counterpart to [`write_serato_beatgrid`]. `Ok(None)` when the file has no
/// grid frame or carries Serato's empty "no grid"; `Err` when the file is
/// unreadable or a `Serato BeatGrid` payload does not parse, so a format
/// mismatch surfaces instead of looking like a missing grid. Used by the live
/// diagnostic to compare the on-disk grid against what `build()` would write.
pub fn read_serato_beatgrid(path: &Path) -> Result<Option<crate::core::serato_beatgrid::Beatgrid>> {
    use crate::core::serato_beatgrid as sbg;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    // Relaxed like every other read path: strict parsing rejects real-world
    // tags (e.g. a non-numeric TDRC) that Serato itself reads fine.
    let tagged = Probe::open(path)?
        .options(ParseOptions::new().parsing_mode(ParsingMode::Relaxed))
        .read()?;
    let Some(tag) = tagged.primary_tag() else { return Ok(None) };
    if ext == "flac" {
        let key = ItemKey::Unknown(sbg::VORBIS_KEY.to_string());
        let Some(v) = tag.get_string(&key) else { return Ok(None) };
        return sbg::grid_from_vorbis_value(v);
    }
    let mut id3: lofty::id3::v2::Id3v2Tag = tag.clone().into();
    use lofty::id3::v2::{Frame, FrameId};
    const GEOB: FrameId<'static> = FrameId::Valid(std::borrow::Cow::Borrowed("GEOB"));
    // Symmetric with the writer's drop-filter: a GEOB whose descriptor is
    // `Serato BeatGrid` is authoritative (and its parse error is reported);
    // otherwise fall back to any other GEOB that happens to parse as a grid.
    let mut fallback = None;
    for f in id3.remove(&GEOB) {
        let Frame::Binary(b) = f else { continue };
        if parse_geob(&b.data).and_then(|g| g.descriptor).as_deref() == Some(sbg::DESCRIPTOR) {
            return sbg::grid_from_geob_body(&b.data);
        }
        if let Ok(Some(grid)) = sbg::grid_from_geob_body(&b.data) {
            fallback.get_or_insert(grid);
        }
    }
    Ok(fallback)
}

/// Reads Serato's cues, loops, track colour and grid lock from a file — the
/// `Serato Markers2` GEOB frame (MP3/WAV/AIFF) or `SERATO_MARKERS_V2` Vorbis
/// comment (FLAC). `Ok(None)` when the file carries no such data; `Err` when
/// the file is unreadable or the data does not parse, so a format mismatch
/// surfaces instead of looking like "no cues".
pub fn read_serato_markers(path: &Path) -> Result<Option<crate::core::serato_markers::SeratoMarkers>> {
    use crate::core::serato_markers as sm;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    // Relaxed like every other read path: strict parsing rejects real-world
    // tags (e.g. a non-numeric TDRC) that Serato itself reads fine.
    let tagged = Probe::open(path)?
        .options(ParseOptions::new().parsing_mode(ParsingMode::Relaxed))
        .read()?;
    let Some(tag) = tagged.primary_tag() else { return Ok(None) };
    if ext == "flac" {
        let key = ItemKey::Unknown(sm::VORBIS_KEY.to_string());
        let Some(v) = tag.get_string(&key) else { return Ok(None) };
        return sm::markers_from_vorbis_value(v).map(Some);
    }
    let mut id3: lofty::id3::v2::Id3v2Tag = tag.clone().into();
    use lofty::id3::v2::{Frame, FrameId};
    const GEOB: FrameId<'static> = FrameId::Valid(std::borrow::Cow::Borrowed("GEOB"));
    for f in id3.remove(&GEOB) {
        let Frame::Binary(b) = f else { continue };
        let Some(obj) = parse_geob(&b.data) else { continue };
        if obj.descriptor.as_deref() == Some(sm::DESCRIPTOR) {
            return sm::parse_markers2(&obj.data).map(Some);
        }
    }
    Ok(None)
}

/// Serato's Markers2 data exactly as stored: the GEOB payload (MP3/WAV/AIFF)
/// or the FLAC Vorbis value. The writing side needs the raw form so a
/// rewrite can keep every entry it does not understand.
#[derive(Debug, Clone, PartialEq)]
pub enum RawMarkers {
    Geob(Vec<u8>),
    Vorbis(String),
}

/// The file's raw Markers2 data; `Ok(None)` when it has none.
pub fn read_serato_markers_raw(path: &Path) -> Result<Option<RawMarkers>> {
    use crate::core::serato_markers as sm;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let tagged = Probe::open(path)?
        .options(ParseOptions::new().parsing_mode(ParsingMode::Relaxed))
        .read()?;
    let Some(tag) = tagged.primary_tag() else { return Ok(None) };
    if ext == "flac" {
        let key = ItemKey::Unknown(sm::VORBIS_KEY.to_string());
        return Ok(tag.get_string(&key).map(|v| RawMarkers::Vorbis(v.to_string())));
    }
    let mut id3: lofty::id3::v2::Id3v2Tag = tag.clone().into();
    use lofty::id3::v2::{Frame, FrameId};
    const GEOB: FrameId<'static> = FrameId::Valid(std::borrow::Cow::Borrowed("GEOB"));
    for f in id3.remove(&GEOB) {
        let Frame::Binary(b) = f else { continue };
        let Some(obj) = parse_geob(&b.data) else { continue };
        if obj.descriptor.as_deref() == Some(sm::DESCRIPTOR) {
            return Ok(Some(RawMarkers::Geob(obj.data.to_vec())));
        }
    }
    Ok(None)
}

/// The file's Markers2 entries (every entry, unknown ones too); `Ok(None)`
/// when the file has no Markers2 data, `Err` when it has data that does not
/// parse — which the importer treats as "do not write this file".
pub fn read_serato_markers_entries(
    path: &Path,
) -> Result<Option<Vec<crate::core::serato_markers::Entry>>> {
    use crate::core::serato_markers as sm;
    match read_serato_markers_raw(path)? {
        None => Ok(None),
        Some(RawMarkers::Geob(p)) => sm::entries_from_payload(&p).map(Some),
        Some(RawMarkers::Vorbis(v)) => sm::entries_from_vorbis_value(&v).map(Some),
    }
}

/// MimeType for a picked image file, by extension. Only formats the tag
/// standards and the webview both handle.
pub fn image_mime_for(path: &Path) -> Option<MimeType> {
    match path
        .extension()?
        .to_str()?
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => Some(MimeType::Jpeg),
        "png" => Some(MimeType::Png),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn audio_extension_filter() {
        assert!(is_audio_file(&PathBuf::from("a/track.MP3")));
        assert!(is_audio_file(&PathBuf::from("track.flac")));
        assert!(is_audio_file(&PathBuf::from("track.aif")));
        assert!(!is_audio_file(&PathBuf::from("cover.jpg")));
        assert!(!is_audio_file(&PathBuf::from("noext")));
    }

    #[test]
    fn normalized_trims_strings_and_drops_invalid_numbers() {
        let update = TagUpdate {
            title: Some("  Song  ".into()),
            artist: Some("   ".into()),
            album: None,
            genre: Some("".into()),
            year: Some(-1),
            bpm: Some(0.0),
            key: Some(" 8A ".into()),
            comment: Some("note".into()),
            label: Some("  Warp  ".into()),
            rating: None,
        };
        let n = update.normalized();
        assert_eq!(n.title.as_deref(), Some("Song"));
        assert_eq!(n.artist, None, "whitespace-only clears the field");
        assert_eq!(n.album, None);
        assert_eq!(n.genre, None, "empty string clears the field");
        assert_eq!(n.year, None, "non-positive year dropped");
        assert_eq!(n.bpm, None, "non-positive bpm dropped");
        assert_eq!(n.key.as_deref(), Some("8A"));
        assert_eq!(n.comment.as_deref(), Some("note"));
        assert_eq!(n.label.as_deref(), Some("Warp"));
    }

    #[test]
    fn normalized_keeps_valid_positive_numbers() {
        let update = TagUpdate {
            title: None,
            artist: None,
            album: None,
            genre: None,
            year: Some(2024),
            bpm: Some(128.0),
            key: None,
            comment: None,
            label: None,
            rating: None,
        };
        let n = update.normalized();
        assert_eq!(n.year, Some(2024));
        assert_eq!(n.bpm, Some(128.0));
    }

    /// Minimal valid WAV (silence) so lofty can probe/read/write real tags
    /// without needing a real music file on disk.
    fn write_silent_wav(path: &Path) {
        let sample_rate: u32 = 22050;
        let samples = vec![0.0f32; sample_rate as usize / 10]; // 0.1s
        let data_len = (samples.len() * 4) as u32;
        let mut wav: Vec<u8> = Vec::with_capacity(44 + samples.len() * 4);
        wav.extend(b"RIFF");
        wav.extend((36 + data_len).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(3u16.to_le_bytes()); // IEEE float
        wav.extend(1u16.to_le_bytes()); // mono
        wav.extend(sample_rate.to_le_bytes());
        wav.extend((sample_rate * 4).to_le_bytes());
        wav.extend(4u16.to_le_bytes());
        wav.extend(32u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(data_len.to_le_bytes());
        for s in &samples {
            wav.extend(s.to_le_bytes());
        }
        std::fs::write(path, wav).unwrap();
    }

    fn test_wav(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spindex-metadata-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        write_silent_wav(&path);
        path
    }

    fn own_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("spindex-safe-save-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn an_interrupted_tag_write_leaves_the_original_intact() {
        // lofty rewrites by truncating the file to 0 and writing it back; an
        // interruption in between emptied a 45 MB AIFF on the acceptance
        // stick. Simulate exactly that: truncate, then fail.
        let dir = own_dir("interrupted");
        let path = dir.join("track.wav");
        write_silent_wav(&path);
        let before = std::fs::read(&path).unwrap();
        let r = save_via_copy(&path, "test", |p| {
            std::fs::OpenOptions::new().write(true).open(p)?.set_len(0)?;
            anyhow::bail!("device removed")
        });
        assert!(r.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before, "original must be byte-identical");
        assert_eq!(names_in(&dir), vec!["track.wav"], "no temp file left behind");
    }

    #[test]
    fn a_tag_write_that_leaves_an_empty_file_is_refused() {
        let dir = own_dir("empty");
        let path = dir.join("track.wav");
        write_silent_wav(&path);
        let before = std::fs::read(&path).unwrap();
        let r = save_via_copy(&path, "test", |p| {
            std::fs::OpenOptions::new().write(true).open(p)?.set_len(0)?;
            Ok(())
        });
        assert!(r.is_err(), "an empty result must never replace the file");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(names_in(&dir), vec!["track.wav"]);
    }

    #[test]
    fn a_successful_tag_write_replaces_the_file_and_cleans_up() {
        let dir = own_dir("ok");
        let path = dir.join("track.wav");
        write_silent_wav(&path);
        write(&path, &TagUpdate { title: Some("Safe".into()), ..Default::default() }).unwrap();
        assert_eq!(read(&path).unwrap().title.as_deref(), Some("Safe"));
        assert_eq!(names_in(&dir), vec!["track.wav"], "no temp or aside file left behind");
    }

    #[test]
    fn a_read_only_file_is_refused_with_a_clear_error() {
        let dir = own_dir("readonly");
        let path = dir.join("track.wav");
        write_silent_wav(&path);
        let mut perm = std::fs::metadata(&path).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&path, perm.clone()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let err = write(&path, &TagUpdate { title: Some("x".into()), ..Default::default() })
            .expect_err("a read-only file must not be replaced behind the user's back");
        assert!(format!("{err:#}").contains("read-only"), "{err:#}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        perm.set_readonly(false);
        std::fs::set_permissions(&path, perm).unwrap();
    }

    #[test]
    fn read_serato_markers_finds_the_frame_and_reports_absence() {
        let dir = own_dir("markers");
        let path = dir.join("m.wav");
        write_silent_wav(&path);
        assert_eq!(read_serato_markers(&path).unwrap(), None, "no frame yet");

        // attach a real Markers2 payload as a GEOB frame
        let mut payload = vec![0x01, 0x01];
        payload.extend_from_slice(b"AQFDT0xPUgAAAAAEAP///0NVRQAAAAANAAAAAHF4AMwAAAAAAEJQTUxPQ0sAAAAAAQAA\0");
        let mut body = vec![0u8];
        body.extend_from_slice(b"application/octet-stream\0\0Serato Markers2\0");
        body.extend_from_slice(&payload);
        let tagged = Probe::open(&path).unwrap().read().unwrap();
        let base = tagged
            .primary_tag()
            .cloned()
            .unwrap_or_else(|| Tag::new(lofty::tag::TagType::Id3v2));
        let mut id3: lofty::id3::v2::Id3v2Tag = base.into();
        use lofty::id3::v2::{BinaryFrame, Frame, FrameId};
        id3.insert(Frame::Binary(BinaryFrame::new(
            FrameId::Valid(std::borrow::Cow::Borrowed("GEOB")),
            body,
        )));
        save_id3(id3, &path, "test markers").unwrap();

        let m = read_serato_markers(&path).unwrap().expect("frame present");
        assert_eq!(m.cues.len(), 1);
        assert_eq!(m.cues[0].pos_ms, 0x7178);
    }

    /// A WAV whose `id3 ` chunk holds an ID3v2.4 tag with a non-numeric TDRC
    /// (common in the wild; strict parsing rejects it) and a real Markers2 GEOB.
    fn wav_with_sloppy_tag_and_markers(path: &Path) {
        fn syncsafe(n: usize) -> [u8; 4] {
            [((n >> 21) & 0x7f) as u8, ((n >> 14) & 0x7f) as u8, ((n >> 7) & 0x7f) as u8, (n & 0x7f) as u8]
        }
        fn frame(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
            let mut f = id.to_vec();
            f.extend_from_slice(&syncsafe(body.len()));
            f.extend_from_slice(&[0, 0]);
            f.extend_from_slice(body);
            f
        }
        let mut geob = vec![0u8];
        geob.extend_from_slice(b"application/octet-stream\0\0Serato Markers2\0\x01\x01");
        geob.extend_from_slice(b"AQFDT0xPUgAAAAAEAP///0NVRQAAAAANAAAAAHF4AMwAAAAAAEJQTUxPQ0sAAAAAAQAA\0");
        let mut frames = frame(b"TDRC", b"\x03abcd");
        frames.extend(frame(b"GEOB", &geob));
        let mut tag = b"ID3\x04\x00\x00".to_vec();
        tag.extend_from_slice(&syncsafe(frames.len()));
        tag.extend(frames);

        write_silent_wav(path);
        let mut wav = std::fs::read(path).unwrap();
        wav.extend_from_slice(b"id3 ");
        wav.extend_from_slice(&(tag.len() as u32).to_le_bytes());
        wav.extend_from_slice(&tag);
        if tag.len() % 2 == 1 {
            wav.push(0);
        }
        let riff = (wav.len() - 8) as u32;
        wav[4..8].copy_from_slice(&riff.to_le_bytes());
        std::fs::write(path, wav).unwrap();
    }

    #[test]
    fn read_serato_markers_tolerates_a_tag_strict_mode_rejects() {
        let dir = own_dir("markers-sloppy");
        let path = dir.join("sloppy.wav");
        wav_with_sloppy_tag_and_markers(&path);
        assert!(
            Probe::open(&path).unwrap().read().is_err(),
            "fixture must be one strict parsing rejects"
        );
        let m = read_serato_markers(&path).unwrap().expect("markers read despite the bad date");
        assert_eq!(m.cues.len(), 1);
    }

    #[test]
    fn read_serato_markers_errors_on_an_empty_file() {
        let dir = own_dir("markers-empty");
        let path = dir.join("empty.aiff");
        std::fs::write(&path, b"").unwrap();
        assert!(read_serato_markers(&path).is_err());
    }

    #[test]
    fn rating_round_trips_and_clears_in_an_id3_file() {
        let path = test_wav("rating.wav");
        let update = TagUpdate { title: Some("Rated".into()), rating: Some(4), ..Default::default() };
        write(&path, &update).unwrap();
        assert_eq!(read(&path).unwrap().rating, Some(4));
        // a partial write that does not name the rating leaves it alone
        apply_partial(&path, &TagUpdate { title: Some("Renamed".into()), ..Default::default() }, &["title".into()])
            .unwrap();
        assert_eq!(read(&path).unwrap().rating, Some(4));
        // and one that names it sets it
        apply_partial(&path, &TagUpdate { rating: Some(2), ..Default::default() }, &["rating".into()]).unwrap();
        assert_eq!(read(&path).unwrap().rating, Some(2));
        write(&path, &TagUpdate { title: Some("Rated".into()), ..Default::default() }).unwrap();
        assert_eq!(read(&path).unwrap().rating, None);
    }

    #[test]
    fn clearing_a_rating_also_clears_another_programs_but_keeps_its_play_count() {
        use lofty::id3::v2::{Frame, Id3v2Tag, PopularimeterFrame};
        let path = test_wav("rating-other.wav");
        write(&path, &TagUpdate { title: Some("x".into()), ..Default::default() }).unwrap();
        let tagged = Probe::open(&path).unwrap().read().unwrap();
        let mut id3: Id3v2Tag = tagged.primary_tag().unwrap().clone().into();
        id3.insert(Frame::Popularimeter(PopularimeterFrame::new("traktor@native-instruments.de".into(), 196, 7)));
        save_id3(id3, &path, "test").unwrap();
        assert_eq!(read(&path).unwrap().rating, Some(4), "another program's rating is read");

        write(&path, &TagUpdate { title: Some("x".into()), ..Default::default() }).unwrap();
        assert_eq!(read(&path).unwrap().rating, None, "a clear does not come back from the other frame");
        let tagged = Probe::open(&path).unwrap().read().unwrap();
        let id3: Id3v2Tag = tagged.primary_tag().unwrap().clone().into();
        let counts: Vec<u64> = id3
            .into_iter()
            .filter_map(|f| match f {
                Frame::Popularimeter(p) => Some(p.counter),
                _ => None,
            })
            .collect();
        assert_eq!(counts, vec![7], "the other program's play count survives");
    }

    #[test]
    fn rating_scales_map_to_stars() {
        assert_eq!(stars_from_popm(0), None);
        for (stars, byte) in [(1, 1u8), (2, 64), (3, 128), (4, 196), (5, 255)] {
            assert_eq!(popm_from_stars(stars), byte);
            assert_eq!(stars_from_popm(byte), Some(stars));
        }
        assert_eq!(stars_from_text("80"), Some(4));
        assert_eq!(stars_from_text("100"), Some(5));
        assert_eq!(stars_from_text("3"), Some(3));
        assert_eq!(stars_from_text("0"), None);
        assert_eq!(stars_from_text("x"), None);
        assert_eq!(TagUpdate { rating: Some(9), ..Default::default() }.normalized().rating, None);
    }

    #[test]
    fn write_then_read_round_trips_tags() {
        let path = test_wav("roundtrip.wav");
        let update = TagUpdate {
            title: Some("One More Time".into()),
            artist: Some("Daft Punk".into()),
            album: Some("Discovery".into()),
            genre: Some("House".into()),
            year: Some(2001),
            bpm: Some(123.0),
            key: Some("8A".into()),
            comment: Some("classic".into()),
            label: Some("Parlophone".into()),
            rating: None,
        };
        write(&path, &update).unwrap();

        let meta = read(&path).unwrap();
        assert_eq!(meta.title.as_deref(), Some("One More Time"));
        assert_eq!(meta.artist.as_deref(), Some("Daft Punk"));
        assert_eq!(meta.album.as_deref(), Some("Discovery"));
        assert_eq!(meta.genre.as_deref(), Some("House"));
        assert_eq!(meta.year, Some(2001));
        assert_eq!(meta.bpm, Some(123.0));
        assert_eq!(meta.key.as_deref(), Some("8A"));
        assert_eq!(meta.comment.as_deref(), Some("classic"));
        assert_eq!(meta.label.as_deref(), Some("Parlophone"));
        assert_eq!(meta.format.as_deref(), Some("wav"));

        // clearing a field (None) removes the tag rather than leaving it stale
        let cleared = TagUpdate {
            title: None,
            artist: Some("Daft Punk".into()),
            album: None,
            genre: None,
            year: None,
            bpm: None,
            key: None,
            comment: None,
            label: None,
            rating: None,
        };
        write(&path, &cleared).unwrap();
        let meta = read(&path).unwrap();
        assert_eq!(meta.title, None);
        assert_eq!(meta.artist.as_deref(), Some("Daft Punk"));
        assert_eq!(meta.bpm, None);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn writes_and_reads_label() {
        let path = test_wav("label-roundtrip.wav");
        let update = TagUpdate {
            title: None,
            artist: None,
            album: None,
            genre: None,
            year: None,
            bpm: None,
            key: None,
            comment: None,
            label: Some("Transmat".into()),
            rating: None,
        };
        write(&path, &update.normalized()).unwrap();
        let meta = read(&path).unwrap();
        assert_eq!(meta.label.as_deref(), Some("Transmat"));

        let _ = std::fs::remove_file(&path);
    }

    /// ID3v2.4 syncsafe integer (7 bits per byte).
    fn syncsafe(n: u32) -> [u8; 4] {
        [
            ((n >> 21) & 0x7f) as u8,
            ((n >> 14) & 0x7f) as u8,
            ((n >> 7) & 0x7f) as u8,
            (n & 0x7f) as u8,
        ]
    }

    fn id3v24_text_frame(id: &[u8; 4], encoding: u8, body: &[u8]) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend(id);
        f.extend(syncsafe(1 + body.len() as u32));
        f.extend([0u8, 0u8]); // flags
        f.push(encoding);
        f.extend(body);
        f
    }

    #[test]
    fn write_serato_data_writes_grid_and_cues_in_one_save() {
        use crate::core::serato_beatgrid::Beatgrid;
        use crate::core::serato_markers as sm;
        let mut frames = Vec::new();
        frames.extend(id3v24_text_frame(b"TIT2", 0x00, b"Keep"));
        let path = test_wav_with_id3("serato-data-write.wav", &frames);
        let grid = Beatgrid::Constant { anchor_sec: 0.05, bpm: 128.0 };
        let entries: Vec<sm::Entry> = vec![("COLOR".into(), sm::colour_body([1, 2, 3]))];
        write_serato_data(&path, Some(&grid), Some(&entries)).unwrap();
        assert_eq!(read_serato_beatgrid(&path).unwrap(), Some(grid));
        assert_eq!(read_serato_markers_entries(&path).unwrap(), Some(entries));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_serato_markers_replaces_markers2_and_keeps_other_frames() {
        use crate::core::serato_markers as sm;
        let mut frames = Vec::new();
        frames.extend(id3v24_text_frame(b"TIT2", 0x00, b"Keep"));
        frames.extend(geob_frame("Serato BeatGrid", &[1, 0, 0, 0, 0, 0, 0]));
        frames.extend(geob_frame(sm::DESCRIPTOR, &sm::encode_payload(&[("BPMLOCK".into(), vec![0])])));
        let path = test_wav_with_id3("markers-write.wav", &frames);

        let entries: Vec<sm::Entry> =
            vec![("COLOR".into(), sm::colour_body([1, 2, 3])), ("BPMLOCK".into(), vec![0])];
        write_serato_markers(&path, &entries).unwrap();

        assert_eq!(read_serato_markers_entries(&path).unwrap(), Some(entries));
        let tagged = probe_relaxed(&path).unwrap();
        let tag = tagged.primary_tag().unwrap();
        assert_eq!(tag.title().as_deref(), Some("Keep"), "text frame kept");
        let id3: lofty::id3::v2::Id3v2Tag = tag.clone().into();
        assert_eq!(count_geob(&id3, sm::DESCRIPTOR), 1, "one Markers2 frame");
        assert_eq!(count_geob(&id3, "Serato BeatGrid"), 1, "the grid frame survives");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_serato_beatgrid_replaces_grid_and_keeps_other_frames() {
        use crate::core::serato_beatgrid::{self as sbg, Beatgrid, DESCRIPTOR};
        let mut frames = Vec::new();
        frames.extend(id3v24_text_frame(b"TIT2", 0x00, b"Keep"));
        frames.extend(geob_frame("Serato Markers2", &[9, 9, 9, 9]));
        frames.extend(geob_frame(DESCRIPTOR, &[0x01, 0x00, 0, 0, 0, 0])); // junk old grid
        let path = test_wav_with_id3("beatgrid-write.wav", &frames);

        let g = Beatgrid::Constant { anchor_sec: 0.05, bpm: 128.0 };
        write_serato_beatgrid(&path, &g).unwrap();

        let tagged = probe_relaxed(&path).unwrap();
        let id3: lofty::id3::v2::Id3v2Tag = tagged.primary_tag().unwrap().clone().into();

        assert_eq!(
            count_geob(&id3, DESCRIPTOR),
            1,
            "one and only one beat grid frame"
        );
        let payload = last_geob_payload(&id3, DESCRIPTOR).unwrap();
        assert_eq!(sbg::parse_payload(&payload).unwrap(), Some(g));
        assert_eq!(
            count_geob(&id3, "Serato Markers2"),
            1,
            "the cue frame is untouched"
        );

        let bytes = std::fs::read(&path).unwrap();
        assert!(
            bytes.windows(15).any(|w| w == b"Serato Markers2"),
            "cues survive on disk"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The beat-grid path runs the same picture sanitization `save_tagged`
    /// does, so it must not poison an APIC frame: after the write the whole tag
    /// must still parse, and any surviving v2.3 WAV picture must be latin1.
    #[test]
    fn write_serato_beatgrid_keeps_a_valid_picture_encoding() {
        use crate::core::serato_beatgrid::Beatgrid;
        let path = wav_with_v23_apic("beatgrid-apic.wav", 1); // UTF-16 BOM-only desc
        write_serato_beatgrid(&path, &Beatgrid::Constant { anchor_sec: 0.0, bpm: 120.0 }).unwrap();

        // The tag must still parse — a poisoned APIC makes the whole tag
        // unreadable in every mode.
        let tagged = probe_relaxed(&path).unwrap();
        assert!(
            tagged.primary_tag().is_some(),
            "tag still parses — the picture frame did not poison it"
        );
        assert_eq!(tagged.primary_tag().unwrap().pictures().len(), 1, "picture kept");

        // Check the on-disk APIC encoding byte directly: converting the generic
        // Tag back to Id3v2Tag re-encodes pictures as UTF-8, so it cannot be
        // read from a round-tripped tag. The byte immediately before the mime
        // string is the frame's text-encoding byte; v2.3 must be latin1 (0).
        let bytes = std::fs::read(&path).unwrap();
        let mime_at = bytes
            .windows(10)
            .position(|w| w == b"image/jpeg")
            .expect("APIC mime on disk");
        assert_eq!(
            bytes[mime_at - 1],
            0,
            "a v2.3 WAV picture must be latin1-encoded after a beat-grid write"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Scoped to MP3/WAV/AIFF/FLAC — an MP4-family file must be refused
    /// untouched, not have a spurious ID3 tag forced onto it.
    #[test]
    fn write_serato_beatgrid_rejects_an_unsupported_container() {
        use crate::core::serato_beatgrid::Beatgrid;
        let dir = std::env::temp_dir().join(format!("spindex-metadata-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("beatgrid-unsupported.m4a");
        std::fs::write(&path, b"not really an m4a").unwrap();
        let before = std::fs::read(&path).unwrap();

        let err = write_serato_beatgrid(&path, &Beatgrid::Constant { anchor_sec: 0.0, bpm: 120.0 })
            .unwrap_err();
        assert!(err.to_string().contains("not supported"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "file left untouched");
        let _ = std::fs::remove_file(&path);
    }

    /// A valid silent WAV with an appended RIFF `id3 ` chunk containing the
    /// given ID3v2.4 frames — lets tests craft byte-exact malformed tags
    /// without having to synthesize a spec-perfect MPEG stream.
    fn test_wav_with_id3(name: &str, frames: &[u8]) -> PathBuf {
        let path = test_wav(name);
        let mut tag = Vec::new();
        tag.extend(b"ID3\x04\x00\x00");
        tag.extend(syncsafe(frames.len() as u32));
        tag.extend(frames);

        let mut wav = std::fs::read(&path).unwrap();
        wav.extend(b"id3 ");
        wav.extend((tag.len() as u32).to_le_bytes());
        wav.extend(&tag);
        if tag.len() % 2 != 0 {
            wav.push(0); // RIFF chunks are padded to even length
        }
        // patch the RIFF size field (total file minus the 8-byte RIFF header)
        let riff_size = (wav.len() - 8) as u32;
        wav[4..8].copy_from_slice(&riff_size.to_le_bytes());
        std::fs::write(&path, wav).unwrap();
        path
    }

    fn wav_with_v23_apic(name: &str, encoding: u8) -> PathBuf {
        wav_with_apic(name, 3, encoding)
    }

    /// A WAV carrying one hand-built ID3v2 tag (minor `version`) with a single
    /// APIC frame using the given text encoding (0 = latin1, 1 = UTF-16 with
    /// BOM, 3 = UTF-8). Frame sizes are syncsafe from v2.4 on.
    fn wav_with_apic(name: &str, version: u8, encoding: u8) -> PathBuf {
        let mut apic: Vec<u8> = vec![encoding];
        apic.extend(b"image/jpeg\0");
        apic.push(3); // front cover
        match encoding {
            1 => apic.extend([0xff, 0xfe, 0x00, 0x00]), // BOM + UTF-16 terminator
            _ => apic.push(0),                          // empty latin1/UTF-8 description
        }
        apic.extend([0xff, 0xd8, 0xff, 0xe0]); // JPEG magic, enough to be a picture

        let mut frames: Vec<u8> = Vec::new();
        frames.extend(b"APIC");
        if version >= 4 {
            frames.extend(syncsafe(apic.len() as u32));
        } else {
            frames.extend((apic.len() as u32).to_be_bytes());
        }
        frames.extend([0u8, 0u8]); // flags
        frames.extend(&apic);

        let mut tag = Vec::new();
        tag.extend(b"ID3");
        tag.extend([version, 0, 0]);
        tag.extend(syncsafe(frames.len() as u32));
        tag.extend(&frames);

        let path = test_wav(name);
        let mut wav = std::fs::read(&path).unwrap();
        wav.extend(b"id3 ");
        wav.extend((tag.len() as u32).to_le_bytes());
        wav.extend(&tag);
        if tag.len() % 2 != 0 {
            wav.push(0);
        }
        let riff_size = (wav.len() - 8) as u32;
        wav[4..8].copy_from_slice(&riff_size.to_le_bytes());
        std::fs::write(&path, wav).unwrap();
        path
    }

    /// The sweep must rewrite a file whose APIC description is UTF-16 — that
    /// is the frame Serato silently ignores.
    #[test]
    fn needs_tag_repair_flags_a_utf16_apic() {
        let path = wav_with_v23_apic("repair-utf16.wav", 1);
        assert!(needs_tag_repair(&path));
        let _ = std::fs::remove_file(&path);
    }

    /// …and must leave the shape Serato actually reads alone, or the sweep
    /// rewrites the whole library every time it runs. For a WAV that shape is
    /// a v2.4 tag with a UTF-8 picture description — what the untouched files
    /// on the acceptance stick carry.
    #[test]
    fn needs_tag_repair_passes_a_v24_utf8_apic_in_a_wav() {
        let path = wav_with_apic("repair-v24-utf8.wav", 4, 3);
        assert!(
            !needs_tag_repair(&path),
            "the form Serato reads must never be flagged, or the sweep never converges"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Two ID3 chunks make readers disagree about the file's tags, so the
    /// consolidating repair has work to do.
    #[test]
    fn needs_tag_repair_flags_duplicate_id3_chunks() {
        let path = test_wav_with_id3("repair-dupe-chunks.wav", b"");
        // append a second ID3 chunk
        let mut wav = std::fs::read(&path).unwrap();
        let mut tag = Vec::new();
        tag.extend(b"ID3\x04\x00\x00");
        tag.extend(syncsafe(0));
        wav.extend(b"ID3 ");
        wav.extend((tag.len() as u32).to_le_bytes());
        wav.extend(&tag);
        let riff_size = (wav.len() - 8) as u32;
        wav[4..8].copy_from_slice(&riff_size.to_le_bytes());
        std::fs::write(&path, wav).unwrap();

        assert_eq!(count_id3_chunks(&path), 2, "fixture really has two chunks");
        assert!(needs_tag_repair(&path));
        let _ = std::fs::remove_file(&path);
    }

    /// A file our own writer produced needs nothing — the sweep must converge,
    /// so running it twice rewrites nothing the second time.
    #[test]
    fn needs_tag_repair_passes_a_file_our_writer_produced() {
        let path = test_wav("repair-freshly-written.wav");
        write(&path, &simple_update("Fresh")).unwrap();
        let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4];
        write_artwork(&path, Some((png, MimeType::Png))).unwrap();
        assert!(
            !needs_tag_repair(&path),
            "our own output must not be flagged, or the sweep never converges"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A v2.3 WAV tag in a lowercase `id3 ` chunk with a latin1 picture is
    /// exactly what Serato writes and reads, so the sweep must leave it alone —
    /// flagging it would rewrite whole multi-gigabyte libraries for nothing.
    #[test]
    fn needs_tag_repair_passes_a_v23_latin1_wav_in_a_lowercase_chunk() {
        let path = wav_with_v23_apic("repair-v23-wav.wav", 0);
        assert!(
            !needs_tag_repair(&path),
            "the shape Serato reads must never be flagged, or the sweep never converges"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// …but a v2.3 WAV with NO picture is left alone. Nothing is displayed
    /// either way, and the next artwork save converts it — flagging it would
    /// rewrite 117 multi-megabyte files on the acceptance stick for no visible
    /// change.
    #[test]
    fn needs_tag_repair_passes_a_v23_wav_without_artwork() {
        let path = test_wav_with_id3("repair-v23-noart.wav", b"");
        // the helper writes a v2.4 header; rewrite the version byte to v2.3
        let mut bytes = std::fs::read(&path).unwrap();
        let at = bytes
            .windows(4)
            .position(|w| w == b"ID3\x04")
            .expect("hand-built tag present");
        bytes[at + 3] = 3;
        std::fs::write(&path, &bytes).unwrap();

        assert!(
            !needs_tag_repair(&path),
            "a pictureless v2.3 WAV is not worth a full-file rewrite"
        );
        let _ = std::fs::remove_file(&path);
    }

    fn db_update() -> TagUpdate {
        TagUpdate {
            title: Some("Da Funk".into()),
            artist: Some("Daft Punk".into()),
            album: None,
            genre: Some("House".into()),
            year: Some(1995),
            bpm: Some(110.5),
            key: Some("6A".into()),
            comment: None,
            label: None,
            rating: None,
        }
    }

    /// Only fields SpinDex actually holds are written. An unanalyzed track has
    /// no BPM or key, and blanking the file's own would destroy data.
    #[test]
    fn fields_with_values_lists_only_what_spindex_knows() {
        assert_eq!(
            fields_with_values(&db_update()),
            vec!["title", "artist", "genre", "year", "bpm", "key"],
        );
        let empty = TagUpdate {
            title: Some("  ".into()), // whitespace is not a value
            ..TagUpdate::default()
        };
        assert!(fields_with_values(&empty).is_empty());
    }

    /// A file already carrying SpinDex's values is skipped — rewriting a WAV
    /// rewrites the whole file, so the sweep must converge to zero work.
    #[test]
    fn tags_already_match_detects_an_up_to_date_file() {
        let u = db_update();
        let apply = fields_with_values(&u);
        let same = TrackMeta {
            title: u.title.clone(),
            artist: u.artist.clone(),
            genre: u.genre.clone(),
            year: u.year,
            bpm: Some(110.5),
            key: u.key.clone(),
            ..TrackMeta::default()
        };
        assert!(tags_already_match(&same, &u, &apply));

        // a differing key is the whole point of the export
        let mut stale = same.clone();
        stale.key = Some("Abm".into());
        assert!(!tags_already_match(&stale, &u, &apply));

        // and a missing BPM must count as different
        let mut no_bpm = same.clone();
        no_bpm.bpm = None;
        assert!(!tags_already_match(&no_bpm, &u, &apply));
    }

    /// BPM survives a tag round trip as text, so compare at the precision a
    /// tag preserves — otherwise every sweep rewrites every analyzed file.
    #[test]
    fn tags_already_match_tolerates_bpm_rounding() {
        let u = TagUpdate { bpm: Some(128.0), ..TagUpdate::default() };
        let apply = fields_with_values(&u);
        let meta = TrackMeta { bpm: Some(128.0000001), ..TrackMeta::default() };
        assert!(tags_already_match(&meta, &u, &apply));
        let meta = TrackMeta { bpm: Some(128.4), ..TrackMeta::default() };
        assert!(!tags_already_match(&meta, &u, &apply));
    }

    /// A file with no tag at all has nothing to repair.
    #[test]
    fn needs_tag_repair_passes_a_tagless_file() {
        let path = test_wav("repair-tagless.wav");
        assert!(!needs_tag_repair(&path));
        let _ = std::fs::remove_file(&path);
    }

    /// Counts the file's ID3 chunks, case-insensitively — RIFF ids are
    /// case-sensitive codes and taggers disagree about which case to use.
    fn count_id3_chunks(path: &Path) -> usize {
        let bytes = std::fs::read(path).unwrap();
        let mut n = 0;
        let mut i = 12usize;
        while i + 8 <= bytes.len() {
            let id = &bytes[i..i + 4];
            let size = u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]])
                as usize;
            if id.eq_ignore_ascii_case(b"id3 ") {
                n += 1;
            }
            i += 8 + size + (size % 2);
        }
        n
    }

    /// Appends a second ID3 chunk under the OTHER case, as seen on 18 of 208
    /// files on the acceptance stick (Serato writes `id3 `, lofty `ID3 `).
    fn append_second_id3(path: &Path, chunk_id: &[u8; 4], frames: &[u8]) {
        let mut tag = Vec::new();
        tag.extend(b"ID3  ");
        tag.extend(syncsafe(frames.len() as u32));
        tag.extend(frames);

        let mut wav = std::fs::read(path).unwrap();
        wav.extend(chunk_id);
        wav.extend((tag.len() as u32).to_le_bytes());
        wav.extend(&tag);
        if tag.len() % 2 != 0 {
            wav.push(0);
        }
        let riff_size = (wav.len() - 8) as u32;
        wav[4..8].copy_from_slice(&riff_size.to_le_bytes());
        std::fs::write(path, wav).unwrap();
    }

    /// A file with two ID3 chunks answers differently depending on chunk
    /// order, so a saved key reads back correct once and reverts later, and
    /// the chunk Serato reads never receives the edit at all. Consolidating
    /// before the write must leave exactly one chunk and lose nothing —
    /// especially not Serato's GEOB beatgrids.
    #[test]
    fn duplicate_id3_chunks_are_merged_before_a_write() {
        // chunk 1 (`id3 `): the Serato-style one — musical key + GEOB
        let mut serato = Vec::new();
        serato.extend(id3v24_text_frame(b"TKEY", 0x00, b"G#m"));
        let geob = b" Serato Markers2 payload";
        serato.extend(id3v24_text_frame(b"GEOB", geob[0], &geob[1..]));
        let path = test_wav_with_id3("dupe-id3.wav", &serato);
        // chunk 2 (`ID3 `): a later writer's chunk with a different key
        append_second_id3(&path, b"ID3 ", &id3v24_text_frame(b"TKEY", 0x00, b"1A"));
        assert_eq!(count_id3_chunks(&path), 2, "fixture must have two chunks");

        let before = read(&path).unwrap().key;

        let update = TagUpdate {
            title: None, artist: None, album: None, genre: None, year: None,
            bpm: None, key: Some("5A".into()), comment: None, label: None,
            rating: None,
        };
        apply_partial(&path, &update, &["key".to_string()]).unwrap();

        assert_eq!(count_id3_chunks(&path), 1, "the chunks must be merged into one");
        let after = read(&path).unwrap();
        assert_eq!(after.key.as_deref(), Some("5A"), "the written key must stick");
        assert!(before.is_some(), "fixture had a key to begin with");

        let bytes = std::fs::read(&path).unwrap();
        let has = |m: &[u8]| bytes.windows(m.len()).any(|w| w == m);
        assert!(has(b"GEOB"), "Serato's GEOB frame must survive the merge");
        assert!(has(b"Serato Markers2"), "GEOB payload must survive the merge");
    }

    /// RIFF chunk ids are case-sensitive and Serato reads a WAV's tag only from
    /// the lowercase `id3 ` one, so consolidating two chunks must leave the
    /// survivor there — merging into `ID3 ` would leave Serato unable to find
    /// any tag on the file at all.
    #[test]
    fn the_merged_chunk_keeps_the_lowercase_wav_chunk_id() {
        let mut serato = Vec::new();
        serato.extend(id3v24_text_frame(b"TKEY", 0x00, b"G#m"));
        let geob = b" Serato Markers2 payload";
        serato.extend(id3v24_text_frame(b"GEOB", geob[0], &geob[1..]));
        // fixture writes the FIRST chunk as lowercase `id3 `
        let path = test_wav_with_id3("chunk-id-case.wav", &serato);
        append_second_id3(&path, b"ID3 ", &id3v24_text_frame(b"TKEY", 0x00, b"1A"));

        repair_duplicate_id3_chunks(&path);
        assert_eq!(count_id3_chunks(&path), 1);
        assert_eq!(
            id3_chunk_ids(&path),
            vec![*b"id3 "],
            "the surviving chunk must be the lowercase one Serato reads"
        );
    }

    /// A file with a single ID3 chunk must be left completely alone — the
    /// repair is only for the duplicate case.
    #[test]
    fn a_single_id3_chunk_is_never_rewritten() {
        let path = test_wav_with_id3(
            "single-id3.wav",
            &id3v24_text_frame(b"TKEY", 0x00, b"8A"),
        );
        assert_eq!(count_id3_chunks(&path), 1);
        let before = std::fs::read(&path).unwrap();
        repair_duplicate_id3_chunks(&path);
        let after = std::fs::read(&path).unwrap();
        assert_eq!(before, after, "a single-chunk file must be byte-identical");
    }

    #[test]
    fn strip_id3_chunks_declines_a_file_with_fewer_than_two() {
        let path = test_wav_with_id3("strip-one.wav", &id3v24_text_frame(b"TKEY", 0x00, b"8A"));
        let bytes = std::fs::read(&path).unwrap();
        assert!(strip_id3_chunks(&bytes, false).is_none(), "one chunk: nothing to do");
        assert!(strip_id3_chunks(&bytes, true).is_none(), "wrong endianness: decline");
    }

    /// The most common malformation on a real DJ library (289/15119 files on
    /// the acceptance USB stick): a TDRC timestamp with non-digit characters.
    /// Relaxed parsing discards the bad frame; valid fields must survive.
    #[test]
    fn read_survives_malformed_timestamp() {
        let mut frames = Vec::new();
        frames.extend(id3v24_text_frame(b"TIT2", 0x00, b"Keep Me")); // valid, latin1
        frames.extend(id3v24_text_frame(b"TDRC", 0x00, b"20xx")); // non-digit timestamp
        let path = test_wav_with_id3("malformed-timestamp.wav", &frames);

        let meta = read(&path).expect("a malformed timestamp must not reject the whole file");
        assert_eq!(
            meta.title.as_deref(),
            Some("Keep Me"),
            "valid fields survive alongside the malformed one"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Undecodable text (odd-length UTF-16, 130/15119 files on the acceptance
    /// stick) is fatal to lofty's tag parser in every parsing mode, so the
    /// best we can do is fall back to a properties-only read: the file stays
    /// indexed (duration/bitrate/format) instead of disappearing entirely.
    #[test]
    fn read_survives_undecodable_text_via_properties_fallback() {
        let mut frames = Vec::new();
        frames.extend(id3v24_text_frame(b"TIT2", 0x00, b"Keep Me"));
        // UTF-16 with BOM but an odd byte count
        frames.extend(id3v24_text_frame(b"TALB", 0x01, &[0xff, 0xfe, 0x41, 0x00, 0x41]));
        let path = test_wav_with_id3("undecodable-text.wav", &frames);

        let meta = read(&path).expect("an undecodable tag must not reject the whole file");
        assert_eq!(meta.format.as_deref(), Some("wav"));
        assert!(
            meta.duration_secs.is_some(),
            "audio properties survive even when the tag is unreadable"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Minimal AIFF whose trailing `ID3 ` chunk is odd-sized and missing the
    /// spec-mandated pad byte — exactly how Serato writes AIFFs. lofty 0.22
    /// assumes the pad exists and slices one byte past EOF when replacing the
    /// chunk (38/304 files on the real stick panicked the metadata editor).
    fn write_aiff_with_unpadded_id3(path: &Path) {
        // ID3v2.4 tag with one TIT2 frame ("Hi", utf-8) — 23 bytes, odd.
        let mut id3: Vec<u8> = Vec::new();
        id3.extend(b"ID3");
        id3.extend([4, 0, 0]);
        id3.extend([0, 0, 0, 13]); // syncsafe tag body size
        id3.extend(b"TIT2");
        id3.extend([0, 0, 0, 3]); // syncsafe frame size
        id3.extend([0, 0]); // frame flags
        id3.push(3); // utf-8
        id3.extend(b"Hi");
        assert_eq!(id3.len(), 23, "fixture tag must be odd-sized");

        let mut comm: Vec<u8> = Vec::new();
        comm.extend(1u16.to_be_bytes()); // mono
        comm.extend(8u32.to_be_bytes()); // sample frames
        comm.extend(16u16.to_be_bytes()); // bit depth
        comm.extend([0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]); // 44100.0 as 80-bit float

        let mut ssnd: Vec<u8> = Vec::new();
        ssnd.extend(0u32.to_be_bytes()); // offset
        ssnd.extend(0u32.to_be_bytes()); // block size
        ssnd.extend([0u8; 16]); // 8 mono 16-bit frames of silence

        let mut body: Vec<u8> = Vec::new();
        body.extend(b"AIFF");
        body.extend(b"COMM");
        body.extend((comm.len() as u32).to_be_bytes());
        body.extend(&comm);
        body.extend(b"SSND");
        body.extend((ssnd.len() as u32).to_be_bytes());
        body.extend(&ssnd);
        body.extend(b"ID3 ");
        body.extend((id3.len() as u32).to_be_bytes());
        body.extend(&id3); // no pad byte — the bug trigger

        let mut file: Vec<u8> = Vec::new();
        file.extend(b"FORM");
        file.extend((body.len() as u32).to_be_bytes());
        file.extend(&body);
        std::fs::write(path, file).unwrap();
    }

    #[test]
    fn pad_repair_appends_one_byte_only_when_truncated() {
        let dir = std::env::temp_dir().join(format!("spindex-metadata-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Truncated: repair appends exactly one zero byte, and only once.
        let truncated = dir.join("pad-truncated.aiff");
        write_aiff_with_unpadded_id3(&truncated);
        let before = std::fs::metadata(&truncated).unwrap().len();
        repair_trailing_chunk_pad(&truncated);
        assert_eq!(std::fs::metadata(&truncated).unwrap().len(), before + 1);
        repair_trailing_chunk_pad(&truncated);
        assert_eq!(
            std::fs::metadata(&truncated).unwrap().len(),
            before + 1,
            "a repaired file is well-formed, second pass must be a no-op"
        );

        // Well-formed wav: untouched.
        let wav = dir.join("pad-ok.wav");
        write_silent_wav(&wav);
        let before = std::fs::metadata(&wav).unwrap().len();
        repair_trailing_chunk_pad(&wav);
        assert_eq!(std::fs::metadata(&wav).unwrap().len(), before);

        // Non-IFF format: untouched even with a bogus extension check miss.
        let mp3 = dir.join("pad-ok.mp3");
        std::fs::write(&mp3, b"ID3junk").unwrap();
        repair_trailing_chunk_pad(&mp3);
        assert_eq!(std::fs::metadata(&mp3).unwrap().len(), 7);

        let _ = std::fs::remove_file(&truncated);
        let _ = std::fs::remove_file(&wav);
        let _ = std::fs::remove_file(&mp3);
    }

    #[test]
    fn write_survives_aiff_with_unpadded_trailing_id3_chunk() {
        let dir = std::env::temp_dir().join(format!("spindex-metadata-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("unpadded.aiff");
        write_aiff_with_unpadded_id3(&path);

        let before = read(&path).expect("fixture must be readable");
        assert_eq!(before.title.as_deref(), Some("Hi"), "fixture tag readable");

        let update = TagUpdate {
            title: Some("Hi".into()),
            artist: None,
            album: None,
            genre: None,
            year: None,
            bpm: None,
            key: Some("8A".into()),
            comment: None,
            label: None,
            rating: None,
        };
        write(&path, &update).expect("unpadded trailing chunk must not panic the write");

        let after = read(&path).unwrap();
        assert_eq!(after.key.as_deref(), Some("8A"));
        assert_eq!(after.title.as_deref(), Some("Hi"));

        let _ = std::fs::remove_file(&path);
    }

    /// The ID3 version is chosen per container. A previous revision wrote WAV
    /// as v2.4 because SpinDex-written v2.3 WAVs showed no cover in Serato
    /// while untouched v2.4 ones did — but every file in that comparison sat in
    /// an uppercase `ID3 ` chunk, which Serato ignores at any version, so the
    /// version was never the variable. Measured properly: all 169 lowercase
    /// `id3 ` chunks across two Serato libraries are v2.3, and Serato reads
    /// them. WAV therefore matches Serato's own output — v2.3 in `id3 `.
    #[test]
    fn write_emits_id3v23_for_wav_matching_what_serato_itself_writes() {
        assert!(wants_id3v23(&PathBuf::from("a/track.mp3")), "mp3 stays v2.3");
        assert!(wants_id3v23(&PathBuf::from("a/track.wav")), "wav is v2.3 too");
        assert!(!wants_id3v23(&PathBuf::from("a/track.aiff")), "aiff needs v2.4");
        assert!(!wants_id3v23(&PathBuf::from("a/track.AIF")), "case-insensitive");

        let path = test_wav("id3-version.wav");
        let update = TagUpdate {
            title: Some("Version Check".into()),
            artist: Some("Üñïçødé Artist".into()), // non-latin1 must survive v2.3 (UTF-16)
            album: None,
            genre: None,
            year: Some(2020),
            bpm: Some(128.0),
            key: Some("8A".into()),
            comment: None,
            label: None,
            rating: None,
        };
        write(&path, &update).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let has = |magic: &[u8]| bytes.windows(magic.len()).any(|w| w == magic);
        assert!(
            has(b"ID3\x03\x00"),
            "a WAV tag must be ID3v2.3 — the shape Serato writes and reads"
        );
        assert!(
            !has(b"ID3\x04\x00"),
            "no v2.4 tag header may remain in a WAV after a save"
        );
        assert_eq!(id3_chunk_ids(&path), vec![*b"id3 "], "and in a lowercase chunk");

        // and the v2.3 tag must still round-trip through our own reader
        let meta = read(&path).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Version Check"));
        assert_eq!(meta.artist.as_deref(), Some("Üñïçødé Artist"));
        assert_eq!(meta.year, Some(2020));
        assert_eq!(meta.bpm, Some(128.0));
        assert_eq!(meta.key.as_deref(), Some("8A"));

        let _ = std::fs::remove_file(&path);
    }

    /// Serato stores beatgrids/cues/overview in GEOB frames inside the ID3
    /// tag. A SpinDex tag edit rewrites the whole tag (now as v2.3), and the
    /// unknown-frame remainder must ride along — dropping GEOB would wipe a
    /// track's Serato analysis on every save.
    #[test]
    fn write_preserves_unknown_binary_frames() {
        let mut geob = vec![0u8]; // latin1 encoding byte
        geob.extend(b"application/octet-stream\0");
        geob.extend(b"\0"); // empty filename
        geob.extend(b"Serato Markers2\0");
        geob.extend([0x01, 0x02, 0x03, 0x04]); // opaque payload
        let mut frames = Vec::new();
        frames.extend(id3v24_text_frame(b"TIT2", 0x00, b"Keep Me"));
        // id3v24_text_frame prepends the encoding byte GEOB also starts with,
        // so pass the body minus that byte.
        frames.extend(id3v24_text_frame(b"GEOB", geob[0], &geob[1..]));
        let path = test_wav_with_id3("geob-preserved.wav", &frames);

        let update = TagUpdate {
            title: Some("Edited".into()),
            artist: None,
            album: None,
            genre: None,
            year: None,
            bpm: None,
            key: None,
            comment: None,
            label: None,
            rating: None,
        };
        write(&path, &update).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let has = |m: &[u8]| bytes.windows(m.len()).any(|w| w == m);
        assert!(has(b"GEOB"), "GEOB frame must survive a tag edit");
        assert!(
            has(b"Serato Markers2"),
            "GEOB description must survive a tag edit"
        );
        let meta = read(&path).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Edited"));

        let _ = std::fs::remove_file(&path);
    }

    /// Builds a GEOB frame body carrying `descriptor` and `payload`.
    fn geob_frame(descriptor: &str, payload: &[u8]) -> Vec<u8> {
        let mut body = vec![0u8]; // latin1 encoding byte
        body.extend(b"application/octet-stream\0");
        body.extend(b"\0"); // empty filename
        body.extend(descriptor.as_bytes());
        body.push(0);
        body.extend(payload);
        // id3v24_text_frame prepends the encoding byte, so pass body minus it.
        id3v24_text_frame(b"GEOB", body[0], &body[1..])
    }

    /// Merging two ID3 chunks keeps every GEOB frame whose bytes differ, so a
    /// file that carried a beatgrid in both chunks ends up with several
    /// `Serato BeatGrid` frames holding *different* payloads. Serato keys those
    /// blocks by descriptor, so more than one is not a state it can resolve.
    /// The last frame wins, matching the read order that decides what every
    /// other value in a merged tag resolves to.
    #[test]
    fn dedupe_geob_keeps_only_the_last_frame_per_descriptor() {
        use lofty::id3::v2::Id3v2Tag;

        let mut frames = Vec::new();
        frames.extend(id3v24_text_frame(b"TIT2", 0x00, b"Track"));
        frames.extend(geob_frame("Serato BeatGrid", &[0xAA]));
        frames.extend(geob_frame("Serato Markers2", &[0xBB]));
        frames.extend(geob_frame("Serato BeatGrid", &[0xCC]));
        frames.extend(geob_frame("Serato BeatGrid", &[0xDD]));
        let path = test_wav_with_id3("geob-dupes.wav", &frames);

        let tagged = probe_relaxed(&path).unwrap();
        let tag = tagged.primary_tag().unwrap().clone();
        let mut id3: Id3v2Tag = tag.into();
        assert_eq!(count_geob(&id3, "Serato BeatGrid"), 3, "fixture precondition");

        dedupe_geob_frames(&mut id3);

        assert_eq!(
            count_geob(&id3, "Serato BeatGrid"),
            1,
            "conflicting beatgrids must collapse to one"
        );
        assert_eq!(
            count_geob(&id3, "Serato Markers2"),
            1,
            "a descriptor that was never duplicated must be untouched"
        );
        assert_eq!(
            last_geob_payload(&id3, "Serato BeatGrid"),
            Some(vec![0xDD]),
            "the surviving beatgrid must be the last one, not the first"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// The unit test above proves the function; this proves it is actually
    /// wired into the write path, so a damaged file heals on its next tag save
    /// rather than needing a special repair pass. Also pins that the sweep can
    /// *find* such files — without detection, `tags_already_match` would skip
    /// them and they would never be offered for repair at all.
    #[test]
    fn a_tag_save_collapses_duplicate_geob_frames_in_the_file() {
        let mut frames = Vec::new();
        frames.extend(id3v24_text_frame(b"TIT2", 0x00, b"Track"));
        frames.extend(geob_frame("Serato BeatGrid", &[0xAA]));
        frames.extend(geob_frame("Serato BeatGrid", &[0xDD]));
        frames.extend(geob_frame("Serato Markers2", &[0xBB]));
        let path = test_wav_with_id3("geob-dupes-write.wav", &frames);

        assert!(
            needs_tag_repair(&path),
            "the sweep must see duplicate descriptors, or it would skip the file"
        );

        write(&path, &simple_update("Edited")).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let count = |m: &[u8]| bytes.windows(m.len()).filter(|w| *w == m).count();
        assert_eq!(count(b"Serato BeatGrid"), 1, "duplicates must collapse");
        assert_eq!(count(b"Serato Markers2"), 1, "the unique block must survive");
        assert!(
            bytes.windows(1).any(|w| w == [0xDD]),
            "the surviving beatgrid must be the last one"
        );
        assert_eq!(read(&path).unwrap().title.as_deref(), Some("Edited"));
        assert!(
            !needs_tag_repair(&path),
            "a repaired file must not be flagged again, or the sweep never converges"
        );

        let _ = std::fs::remove_file(&path);
    }

    fn geob_objects(id3: &lofty::id3::v2::Id3v2Tag) -> Vec<(Option<String>, Vec<u8>)> {
        id3.into_iter()
            .filter(|f| f.id_str() == "GEOB")
            .filter_map(|f| match f {
                lofty::id3::v2::Frame::Binary(b) => parse_geob(&b.data).map(|g| (g.descriptor, g.data)),
                _ => None,
            })
            .collect()
    }

    fn count_geob(id3: &lofty::id3::v2::Id3v2Tag, descriptor: &str) -> usize {
        geob_objects(id3)
            .iter()
            .filter(|(d, _)| d.as_deref() == Some(descriptor))
            .count()
    }

    fn last_geob_payload(id3: &lofty::id3::v2::Id3v2Tag, descriptor: &str) -> Option<Vec<u8>> {
        geob_objects(id3)
            .into_iter()
            .filter(|(d, _)| d.as_deref() == Some(descriptor))
            .next_back()
            .map(|(_, data)| data)
    }

    fn simple_update(title: &str) -> TagUpdate {
        TagUpdate {
            title: Some(title.into()),
            artist: None,
            album: None,
            genre: None,
            year: None,
            bpm: None,
            key: Some("8A".into()),
            comment: None,
            label: None,
            rating: None,
        }
    }

    /// lofty writes a `None` picture description as a single 0x00 terminator
    /// regardless of encoding. In an ID3v2.3 tag (UTF-16 text) its own reader
    /// then fails the WHOLE tag with "UTF-16 string has an invalid byte order
    /// mark" — one artwork save made the file permanently un-editable (the
    /// live "cannot save anymore" report). Tag + artwork + re-edit must
    /// round-trip.
    #[test]
    fn artwork_save_keeps_tag_readable_and_editable() {
        let path = test_wav("apic-desc.wav");
        write(&path, &simple_update("Cover Test")).unwrap();
        let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4];
        write_artwork(&path, Some((png, MimeType::Png))).unwrap();

        let meta = read(&path).unwrap();
        assert!(meta.has_artwork, "artwork present");
        assert_eq!(
            meta.title.as_deref(),
            Some("Cover Test"),
            "text frames must survive an artwork save"
        );

        write(&path, &simple_update("Edited Again"))
            .expect("file must stay editable after an artwork save");
        let meta = read(&path).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Edited Again"));
        assert!(meta.has_artwork, "artwork survives the tag edit");

        let _ = std::fs::remove_file(&path);
    }

    /// The APIC payload of the first picture frame in a written file.
    fn apic_payload(bytes: &[u8]) -> Option<&[u8]> {
        let pos = bytes.windows(4).position(|w| w == b"APIC")?;
        // frame id (4) + size (4) + flags (2)
        Some(&bytes[pos + 10..])
    }

    /// Serato never showed artwork SpinDex had written, while reading every
    /// other file on the same stick. Byte forensics found the difference: our
    /// APIC carried encoding byte 1 (UTF-16) with a BOM-only description
    /// (`fffe`), where every file Serato DOES read uses a latin1/UTF-8
    /// description. lofty builds picture frames as UTF-8 and the v2.3 writer
    /// substitutes UTF-16 for it, so the frame must be forced to latin1 —
    /// where an empty description is the conventional single 0x00.
    #[test]
    fn artwork_is_written_with_a_description_serato_reads() {
        let path = test_wav("apic-latin1.wav");
        let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4];
        write_artwork(&path, Some((png, MimeType::Png))).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let apic = apic_payload(&bytes).expect("APIC frame present");
        // A WAV is written v2.3, which forbids UTF-8, so lofty's picture frame
        // would degrade to UTF-16 (1) — the form Serato silently ignores. The
        // latin1 rescue must have run, leaving encoding 0.
        assert_ne!(apic[0], 1, "APIC description must never be UTF-16");
        assert_eq!(apic[0], 0, "a v2.3 WAV picture must be forced to latin1");
        // encoding byte, then "image/png\0", then the picture type byte
        let desc = 1 + b"image/png\0".len() + 1;
        assert_eq!(apic[desc], 0, "the description is empty (a single terminator)");

        // and the tag must still round-trip through our own reader
        let meta = read(&path).unwrap();
        assert!(meta.has_artwork, "artwork still readable after the save");

        // A plain tag save rewrites the picture frames too, so a file written
        // by an older writer heals on its next edit — no artwork re-pick.
        write(&path, &simple_update("Healed")).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let apic = apic_payload(&bytes).expect("APIC survives a tag save");
        assert_ne!(apic[0], 1, "a later tag save must not reintroduce UTF-16");
        assert!(read(&path).unwrap().has_artwork, "artwork survives the tag save");

        let _ = std::fs::remove_file(&path);
    }

    /// The latin1 rescue still applies where it is needed — an ID3v2.3 save,
    /// which forbids UTF-8 and would otherwise degrade lofty's picture frame
    /// to the UTF-16 form Serato ignores. Exercised directly, because our
    /// only file fixtures are WAVs and those are written v2.4 now.
    #[test]
    fn force_latin1_picture_frames_rewrites_the_v23_picture_encoding() {
        use lofty::id3::v2::{Frame, Id3v2Tag};

        let mut tag = Id3v2Tag::default();
        tag.insert_picture(Picture::new_unchecked(
            PictureType::CoverFront,
            Some(MimeType::Jpeg),
            Some(String::new()),
            vec![0xff, 0xd8, 0xff, 0xe0],
        ));
        let before: Vec<TextEncoding> = (&tag)
            .into_iter()
            .filter_map(|f| match f {
                Frame::Picture(p) => Some(p.encoding),
                _ => None,
            })
            .collect();
        assert_eq!(before, vec![TextEncoding::UTF8], "lofty builds pictures as UTF-8");

        force_latin1_picture_frames(&mut tag);
        let after: Vec<TextEncoding> = (&tag)
            .into_iter()
            .filter_map(|f| match f {
                Frame::Picture(p) => Some(p.encoding),
                _ => None,
            })
            .collect();
        assert_eq!(
            after,
            vec![TextEncoding::Latin1],
            "latin1 survives the v2.3 downgrade; UTF-8 would become UTF-16"
        );
    }

    /// A file poisoned by the pre-fix writer (v2.3 APIC, UTF-16 encoding
    /// byte, latin1-style single-0x00 empty description) must be repaired on
    /// the next save instead of erroring forever.
    #[test]
    fn write_repairs_poisoned_apic_description() {
        // v2.3 frames: sizes are plain big-endian, tag header size syncsafe
        let mut frames: Vec<u8> = Vec::new();
        frames.extend(b"TIT2");
        frames.extend(5u32.to_be_bytes());
        frames.extend([0, 0]); // flags
        frames.push(0); // latin1
        frames.extend(b"Keep");

        let mut apic: Vec<u8> = Vec::new();
        apic.push(1); // UTF-16 encoding — the poison half
        apic.extend(b"image/png\0");
        apic.push(3); // cover front
        apic.push(0); // single-0x00 "description" — latin1-style, invalid UTF-16
        apic.extend([0x89, b'P', b'N', b'G', 9, 9, 9, 9]);
        frames.extend(b"APIC");
        frames.extend((apic.len() as u32).to_be_bytes());
        frames.extend([0, 0]);
        frames.extend(&apic);

        let mut tag: Vec<u8> = Vec::new();
        tag.extend(b"ID3\x03\x00\x00");
        tag.extend(syncsafe(frames.len() as u32));
        tag.extend(&frames);

        let path = test_wav("apic-poisoned.wav");
        let mut wav = std::fs::read(&path).unwrap();
        wav.extend(b"id3 ");
        wav.extend((tag.len() as u32).to_le_bytes());
        wav.extend(&tag);
        if tag.len() % 2 != 0 {
            wav.push(0);
        }
        let riff_size = (wav.len() - 8) as u32;
        wav[4..8].copy_from_slice(&riff_size.to_le_bytes());
        std::fs::write(&path, wav).unwrap();

        // sanity: the fixture reproduces the live failure (tag unreadable)
        let before = read(&path).unwrap();
        assert_eq!(before.title, None, "fixture must be poisoned");

        write(&path, &simple_update("Fixed")).expect("save must repair the poisoned APIC");
        let after = read(&path).unwrap();
        assert_eq!(after.title.as_deref(), Some("Fixed"));
        assert!(after.has_artwork, "picture survives the repair");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_to_missing_file_errors_without_panicking() {
        let path = PathBuf::from(std::env::temp_dir())
            .join(format!("spindex-metadata-missing-{}.wav", std::process::id()));
        let update = TagUpdate {
            title: Some("x".into()),
            artist: None,
            album: None,
            genre: None,
            year: None,
            bpm: None,
            key: None,
            comment: None,
            label: None,
            rating: None,
        };
        assert!(write(&path, &update).is_err());
    }

    #[test]
    fn apply_partial_writes_only_listed_fields() {
        let path = test_wav("partial.wav");
        // seed title + artist + genre via a full write
        write(
            &path,
            &TagUpdate {
                title: Some("Orig Title".into()),
                artist: Some("Orig Artist".into()),
                album: None,
                genre: Some("Orig Genre".into()),
                year: None,
                bpm: None,
                key: None,
                comment: None,
                label: None,
                rating: None,
            },
        )
        .unwrap();

        // apply ONLY genre
        let update = TagUpdate {
            genre: Some("House".into()),
            // other fields carry values that must be ignored because they're not in `apply`
            title: Some("SHOULD NOT WIN".into()),
            artist: None,
            album: None,
            year: None,
            bpm: None,
            key: None,
            comment: None,
            label: None,
            rating: None,
        };
        apply_partial(&path, &update, &["genre".to_string()]).unwrap();

        let meta = read(&path).unwrap();
        assert_eq!(meta.genre.as_deref(), Some("House"), "genre updated");
        assert_eq!(meta.title.as_deref(), Some("Orig Title"), "title untouched");
        assert_eq!(meta.artist.as_deref(), Some("Orig Artist"), "artist untouched");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn apply_partial_empty_value_clears_listed_field() {
        let path = test_wav("partial-clear.wav");
        write(
            &path,
            &TagUpdate {
                title: Some("Keep".into()),
                comment: Some("Bye".into()),
                artist: None, album: None, genre: None, year: None, bpm: None, key: None, label: None,
                rating: None,
            },
        )
        .unwrap();

        // ticked comment, empty value → clear comment, keep title
        let update = TagUpdate {
            comment: None,
            title: None, artist: None, album: None, genre: None, year: None, bpm: None, key: None, label: None,
            rating: None,
        };
        apply_partial(&path, &update, &["comment".to_string()]).unwrap();

        let meta = read(&path).unwrap();
        assert_eq!(meta.comment, None, "comment cleared");
        assert_eq!(meta.title.as_deref(), Some("Keep"), "title preserved");
        let _ = std::fs::remove_file(&path);
    }

    /// Every ID3 chunk id in the file, in order, exactly as stored.
    fn id3_chunk_ids(path: &Path) -> Vec<[u8; 4]> {
        let bytes = std::fs::read(path).unwrap();
        let mut out = Vec::new();
        let mut i = 12usize;
        while i + 8 <= bytes.len() {
            let size = u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]])
                as usize;
            if bytes[i..i + 4].eq_ignore_ascii_case(b"id3 ") {
                out.push([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
            }
            i += 8 + size + (size % 2);
        }
        out
    }

    /// **Serato reads a WAV's ID3 tag only out of a lowercase `id3 ` chunk.**
    ///
    /// Measured across two independent Serato libraries: of the WAVs carrying a
    /// TPE1 frame, all 41 whose tag sits in a lowercase `id3 ` chunk have their
    /// artist in Serato's `database V2` and it matches the file, while the
    /// uppercase-only ones are blank there. The 3 files carrying BOTH chunks
    /// settle it outright — Serato takes the lowercase one every time. lofty
    /// writes `ID3 `, so without this every WAV tag SpinDex saved was invisible
    /// to Serato, artwork and text alike.
    #[test]
    fn write_puts_a_wav_tag_in_a_lowercase_id3_chunk() {
        let path = test_wav("chunk-case-write.wav");
        write(&path, &simple_update("Lowercase")).unwrap();

        assert_eq!(
            id3_chunk_ids(&path),
            vec![*b"id3 "],
            "a WAV must end up with exactly one, lowercase, ID3 chunk"
        );
        assert_eq!(read(&path).unwrap().title.as_deref(), Some("Lowercase"));

        let _ = std::fs::remove_file(&path);
    }

    /// The artwork path saves through the same place, so it must land in the
    /// same chunk — a cover in an uppercase chunk is exactly the case Serato
    /// has never displayed.
    #[test]
    fn write_artwork_leaves_a_wav_tag_in_a_lowercase_id3_chunk() {
        let path = test_wav("chunk-case-art.wav");
        let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4];
        write_artwork(&path, Some((png, MimeType::Png))).unwrap();

        assert_eq!(id3_chunk_ids(&path), vec![*b"id3 "]);
        assert!(read(&path).unwrap().has_artwork);

        let _ = std::fs::remove_file(&path);
    }

    /// So the sweep can heal the library that older builds wrote: a WAV whose
    /// tag sits in an uppercase chunk is unreadable to Serato and must be
    /// rewritten, with or without a picture.
    #[test]
    fn needs_tag_repair_flags_an_uppercase_id3_chunk_in_a_wav() {
        let path = test_wav_with_id3("repair-upper-chunk.wav", b"");
        let mut bytes = std::fs::read(&path).unwrap();
        let at = bytes
            .windows(4)
            .position(|w| w == b"id3 ")
            .expect("fixture chunk present");
        bytes[at..at + 4].copy_from_slice(b"ID3 ");
        std::fs::write(&path, &bytes).unwrap();

        assert!(
            needs_tag_repair(&path),
            "an uppercase ID3 chunk in a WAV is invisible to Serato"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// …and an AIFF must keep the uppercase `ID3 ` its own spec defines. All 78
    /// AIFF/AIF files across both measured libraries carry `ID3 ` and Serato
    /// reads every one of them, so the lowercase rule is RIFF-only.
    #[test]
    fn aiff_keeps_the_uppercase_id3_chunk() {
        assert_eq!(chunk_container(&PathBuf::from("a/t.wav")), Some((false, *b"id3 ")));
        assert_eq!(chunk_container(&PathBuf::from("a/t.aiff")), Some((true, *b"ID3 ")));
        assert_eq!(chunk_container(&PathBuf::from("a/t.AIF")), Some((true, *b"ID3 ")));
        assert_eq!(chunk_container(&PathBuf::from("a/t.mp3")), None, "no chunk in an MP3");
    }
}

