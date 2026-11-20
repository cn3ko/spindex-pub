//! Online metadata enrichment: a MusicBrainz text search by artist + title,
//! then the full recording lookup. One user-invoked, opt-in lookup that needs
//! no account or key (open data). Every failure path returns
//! `MatchSource::None`, never a hard error.
//!
//! An AcoustID fingerprint path existed until 2026-09-30. AcoustID needs a
//! registered application key and SpinDex uses no service that does, so the
//! path never ran and was removed (see CLAUDE.md, Confirmed decisions).

/// MusicBrainz requires a descriptive User-Agent identifying the application.
pub const USER_AGENT: &str = concat!(
    "SpinDex/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/cn3ko/spindex )"
);

use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct EnrichFields {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<i64>,
    pub label: Option<String>,
    pub genre: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchSource {
    TextSearch,
    None,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnrichResult {
    pub source: MatchSource,
    pub loose: bool,
    pub fields: EnrichFields,
}

fn nonempty(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Parse the leading 4-digit year from a MusicBrainz date like "1991-05-03".
fn year_from_date(date: Option<&str>) -> Option<i64> {
    let d = date?;
    d.get(0..4).and_then(|y| y.parse::<i64>().ok()).filter(|y| *y > 0)
}

/// Pick the earliest-dated release from a MusicBrainz recording's `releases`
/// array. MusicBrainz does not guarantee array order, so `releases[0]` can be
/// a much later reissue/compilation. Releases with a missing or unparseable
/// `date` never win over a dated one (they sort as "no date" and lose ties).
fn earliest_release(releases: &[serde_json::Value]) -> Option<&serde_json::Value> {
    releases
        .iter()
        .filter(|r| year_from_date(r["date"].as_str()).is_some())
        .min_by_key(|r| r["date"].as_str().unwrap_or_default().to_string())
        .or_else(|| releases.first())
}

/// Map a MusicBrainz recording JSON object to enrichment fields. Any missing
/// or malformed field simply stays None.
pub fn map_recording(v: &serde_json::Value) -> EnrichFields {
    let title = nonempty(v["title"].as_str());

    let artist = v["artist-credit"]
        .as_array()
        .map(|credits| {
            credits
                .iter()
                .filter_map(|c| c["name"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .and_then(|s| nonempty(Some(&s)));

    let first_release = v["releases"].as_array().and_then(|r| earliest_release(r));
    let album = first_release.and_then(|r| nonempty(r["title"].as_str()));
    let year = year_from_date(first_release.and_then(|r| r["date"].as_str()));
    let label = first_release
        .and_then(|r| r["label-info"].as_array())
        .and_then(|li| li.first())
        .and_then(|l| nonempty(l["label"]["name"].as_str()));

    // Prefer a curated genre; fall back to the top folksonomy tag, since
    // MusicBrainz genre coverage is sparse for dance/hip-hop.
    let genre = top_named_by_count(&v["genres"]).or_else(|| top_named_by_count(&v["tags"]));

    EnrichFields { title, artist, album, year, label, genre }
}

/// Highest-`count` `name` from a MusicBrainz `[{name, count}]` array (genres/tags).
fn top_named_by_count(v: &serde_json::Value) -> Option<String> {
    v.as_array()?
        .iter()
        .max_by_key(|g| g["count"].as_i64().unwrap_or(0))
        .and_then(|g| nonempty(g["name"].as_str()))
}

/// Turn the text-search outcome into a result. A text match is always loose
/// ("approximate" in the UI): it matched on artist + title, not on the audio.
pub fn decide(text: Option<EnrichFields>) -> EnrichResult {
    if let Some(fields) = text {
        EnrichResult { source: MatchSource::TextSearch, loose: true, fields }
    } else {
        EnrichResult { source: MatchSource::None, loose: false, fields: EnrichFields::default() }
    }
}

/// Shared agent with a global timeout so no lookup hangs the caller.
fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .into()
}

/// MusicBrainz asks for <= 1 request/second. A single enrichment now makes a
/// short chain of calls (search -> recording -> release), so pace them to stay
/// polite and avoid a 503. Runs on the blocking pool, so a sleep is fine.
fn pace_musicbrainz() {
    std::thread::sleep(std::time::Duration::from_millis(400));
}

/// GET a MusicBrainz JSON document, paced and with the required User-Agent.
fn musicbrainz_get(agent: &ureq::Agent, url: &str) -> Option<serde_json::Value> {
    pace_musicbrainz();
    let mut resp = agent.get(url).header("User-Agent", USER_AGENT).call().ok()?;
    resp.body_mut().read_json().ok()
}

/// Resolve a recording MBID to enrichment fields. The recording lookup carries
/// title/artist/album/year/genre; the record **label** lives only on the
/// release (the recording endpoint has no label-info and rejects `inc=labels`
/// with a 400), so it is fetched with a second release lookup and filled in.
fn recording_to_fields(agent: &ureq::Agent, mbid: &str) -> Option<EnrichFields> {
    if !is_mbid(mbid) {
        return None;
    }
    let url = format!(
        "https://musicbrainz.org/ws/2/recording/{mbid}?fmt=json&inc=artists+releases+release-groups+genres+tags"
    );
    let recording = musicbrainz_get(agent, &url)?;
    let mut fields = map_recording(&recording);
    if fields.label.is_none() {
        if let Some(rel_id) = earliest_release_id(&recording) {
            fields.label = release_label(agent, &rel_id);
        }
    }
    Some(fields)
}

/// Whether a string is a MusicBrainz identifier: a canonical UUID, `8-4-4-4-12`
/// lowercase-or-uppercase hex.
///
/// MBIDs arrive inside JSON from MusicBrainz and are interpolated
/// straight into request URLs. They are not user input and neither service has
/// any reason to return something else, but nothing in the code guaranteed the
/// shape — a value carrying `?`, `#`, `/` or a space would silently change the
/// endpoint or its query rather than fail. Checking is a two-line guard, so the
/// URL is built only from something that cannot alter its structure.
fn is_mbid(s: &str) -> bool {
    let groups = [8usize, 4, 4, 4, 12];
    let mut parts = s.split('-');
    for len in groups {
        match parts.next() {
            Some(p) if p.len() == len && p.bytes().all(|b| b.is_ascii_hexdigit()) => {}
            _ => return false,
        }
    }
    parts.next().is_none()
}

/// The earliest-dated release's MBID from a recording lookup, for the label fetch.
fn earliest_release_id(recording: &serde_json::Value) -> Option<String> {
    recording["releases"]
        .as_array()
        .and_then(|r| earliest_release(r))
        .and_then(|r| nonempty(r["id"].as_str()))
}

/// Fetch a release's record label. Labels are only exposed on the release
/// endpoint (`inc=labels`), never on the recording endpoint.
fn release_label(agent: &ureq::Agent, release_id: &str) -> Option<String> {
    if !is_mbid(release_id) {
        return None;
    }
    let url = format!("https://musicbrainz.org/ws/2/release/{release_id}?fmt=json&inc=labels");
    let v = musicbrainz_get(agent, &url)?;
    v["label-info"]
        .as_array()?
        .iter()
        .find_map(|li| nonempty(li["label"]["name"].as_str()))
}

/// Escape Lucene special characters in a quoted phrase: backslash and double-quote.
/// Backslash must be escaped first to avoid double-escaping.
fn lucene_escape(s: &str) -> String {
    let mut result = String::new();
    for c in s.chars() {
        match c {
            '\\' => result.push_str("\\\\"),
            '"' => result.push_str("\\\""),
            _ => result.push(c),
        }
    }
    result
}

/// Drop *all* trailing bracketed qualifiers — " (Original Mix)", " [Extended]",
/// " (BVRNOUT Remix) [CLEAN]" — that DJ tags stack on but MusicBrainz recording
/// titles usually don't, leaving the base song title. Bootleg/edit remixes are
/// frequently absent from MusicBrainz entirely, so the base title is the best
/// (approximate) match available. Returns the title unchanged when there is no
/// trailing bracket, and stops before emptying the string. Only used as a
/// *retry* after the exact-title search finds nothing, so genuine parenthetical
/// titles ("Lady (Hear Me Tonight)") match on the first attempt and never reach
/// this.
fn strip_mix_suffix(title: &str) -> &str {
    let mut t = title.trim();
    loop {
        let open = if t.ends_with(')') {
            '('
        } else if t.ends_with(']') {
            '['
        } else {
            break;
        };
        let Some(idx) = t.rfind(open) else { break };
        let head = t[..idx].trim_end();
        if head.is_empty() {
            break;
        }
        t = head;
    }
    t
}

/// One MusicBrainz recording search; returns the top hit's MBID. `artist` is
/// added as a hard `AND artist:"..."` clause when present.
fn search_once(agent: &ureq::Agent, artist: Option<&str>, title: &str) -> Option<String> {
    let mut query = format!("recording:\"{}\"", lucene_escape(title));
    if let Some(a) = artist {
        query.push_str(&format!(" AND artist:\"{}\"", lucene_escape(a)));
    }
    pace_musicbrainz();
    let mut resp = agent
        .get("https://musicbrainz.org/ws/2/recording")
        .query("fmt", "json")
        .query("limit", "1")
        .query("query", &query)
        .header("User-Agent", USER_AGENT)
        .call()
        .ok()?;
    let search: serde_json::Value = resp.body_mut().read_json().ok()?;
    nonempty(search["recordings"].as_array()?.first()?["id"].as_str())
}

/// Find a recording MBID by text, trying progressively looser queries and
/// returning the first hit:
///   1. exact title + artist    (precise; also lets a real parenthetical title
///                               like "Lady (Hear Me Tonight)" match as-is)
///   2. base title + artist     (strips DJ mix suffixes MB doesn't store)
///   3. base title, no artist   (last resort: the tag may be the remixer, or a
///                               "feat."/comma credit MB doesn't index that way,
///                               which otherwise zeroes out the whole result)
///
/// Step 3 can match a same-titled song by a different artist, so results stay
/// marked `loose` and every field is user-reviewed before Save.
fn search_recording_mbid(
    agent: &ureq::Agent,
    artist: Option<&str>,
    title: Option<&str>,
) -> Option<String> {
    let title = title.map(str::trim).filter(|s| !s.is_empty())?;
    let artist = artist.map(str::trim).filter(|s| !s.is_empty());
    let base = strip_mix_suffix(title);

    if let Some(a) = artist {
        if let Some(id) = search_once(agent, Some(a), title) {
            return Some(id);
        }
        if base != title {
            if let Some(id) = search_once(agent, Some(a), base) {
                return Some(id);
            }
        }
        return search_once(agent, None, base);
    }

    if let Some(id) = search_once(agent, None, title) {
        return Some(id);
    }
    if base != title {
        return search_once(agent, None, base);
    }
    None
}

/// Text search: resolve a recording MBID by artist+title, then map it through
/// the full recording lookup.
fn text_lookup(artist: Option<&str>, title: Option<&str>) -> Option<EnrichFields> {
    let agent = http_agent();
    let mbid = search_recording_mbid(&agent, artist, title)?;
    recording_to_fields(&agent, &mbid)
}

/// Full enrichment by artist + title. Any failure degrades to
/// MatchSource::None — never an error.
pub fn enrich_track(artist: Option<&str>, title: Option<&str>) -> EnrichResult {
    decide(text_lookup(artist, title))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn is_mbid_accepts_a_canonical_uuid_and_rejects_url_altering_shapes() {
        assert!(is_mbid("b9b0f2d8-5f2e-4f1a-9c3d-1a2b3c4d5e6f"));
        assert!(is_mbid("B9B0F2D8-5F2E-4F1A-9C3D-1A2B3C4D5E6F"), "hex case-insensitive");

        // The shapes that matter: anything that could change the endpoint or
        // its query once interpolated into the URL.
        assert!(!is_mbid("b9b0f2d8-5f2e-4f1a-9c3d-1a2b3c4d5e6f?inc=labels"));
        assert!(!is_mbid("b9b0f2d8-5f2e-4f1a-9c3d-1a2b3c4d5e6f/../release"));
        assert!(!is_mbid("../../etc"));
        assert!(!is_mbid("b9b0f2d8 5f2e 4f1a 9c3d 1a2b3c4d5e6f"));
        assert!(!is_mbid("b9b0f2d8-5f2e-4f1a-9c3d-1a2b3c4d5e6f-extra"), "trailing group");
        assert!(!is_mbid("zzzzzzzz-5f2e-4f1a-9c3d-1a2b3c4d5e6f"), "non-hex");
        assert!(!is_mbid("b9b0f2d8-5f2e-4f1a-9c3d"), "too few groups");
        assert!(!is_mbid(""));
    }

    #[test]
    fn maps_full_recording() {
        let v = json!({
            "title": "Strings of Life",
            "artist-credit": [{ "name": "Derrick May" }],
            "releases": [{
                "title": "Innovator",
                "date": "1991-05",
                "label-info": [{ "label": { "name": "Transmat" } }]
            }],
            "genres": [
                { "name": "house", "count": 2 },
                { "name": "techno", "count": 5 }
            ]
        });
        let f = map_recording(&v);
        assert_eq!(f.title.as_deref(), Some("Strings of Life"));
        assert_eq!(f.artist.as_deref(), Some("Derrick May"));
        assert_eq!(f.album.as_deref(), Some("Innovator"));
        assert_eq!(f.year, Some(1991));
        assert_eq!(f.label.as_deref(), Some("Transmat"));
        assert_eq!(f.genre.as_deref(), Some("techno")); // highest count wins
    }

    #[test]
    fn genre_falls_back_to_top_tag_when_genres_empty() {
        let v = json!({
            "title": "Some Track",
            "genres": [],
            "tags": [
                { "name": "hip hop", "count": 3 },
                { "name": "rap", "count": 1 }
            ]
        });
        assert_eq!(map_recording(&v).genre.as_deref(), Some("hip hop"));
    }

    #[test]
    fn genre_prefers_curated_genre_over_tag() {
        let v = json!({
            "title": "Some Track",
            "genres": [{ "name": "techno", "count": 5 }],
            "tags": [{ "name": "electronic", "count": 9 }]
        });
        assert_eq!(map_recording(&v).genre.as_deref(), Some("techno"));
    }

    #[test]
    fn maps_sparse_recording() {
        let v = json!({ "title": "Untitled" });
        let f = map_recording(&v);
        assert_eq!(f.title.as_deref(), Some("Untitled"));
        assert_eq!(f.artist, None);
        assert_eq!(f.album, None);
        assert_eq!(f.year, None);
        assert_eq!(f.label, None);
        assert_eq!(f.genre, None);
    }

    #[test]
    fn a_text_match_is_loose() {
        let text = EnrichFields { title: Some("X".into()), ..Default::default() };
        let r = decide(Some(text));
        assert_eq!(r.source, MatchSource::TextSearch);
        assert!(r.loose);
    }

    #[test]
    fn decide_none_when_nothing_matched() {
        let r = decide(None);
        assert_eq!(r.source, MatchSource::None);
        assert!(!r.loose);
    }

    #[test]
    fn lucene_escape_plain_string() {
        assert_eq!(lucene_escape("hello"), "hello");
    }

    #[test]
    fn lucene_escape_embedded_quote() {
        assert_eq!(lucene_escape("6\" EP"), "6\\\" EP");
    }

    #[test]
    fn lucene_escape_embedded_backslash() {
        assert_eq!(lucene_escape("foo\\bar"), "foo\\\\bar");
    }

    #[test]
    fn lucene_escape_both_backslash_and_quote() {
        // Input string has a backslash followed by a quote
        assert_eq!(lucene_escape("foo\\\"bar"), "foo\\\\\\\"bar");
    }

    #[test]
    fn map_recording_picks_earliest_release_out_of_order() {
        // Releases deliberately out of chronological order: a 2015 reissue
        // listed before the 1991 original. The earliest-dated release must win.
        let v = json!({
            "title": "Strings of Life",
            "artist-credit": [{ "name": "Derrick May" }],
            "releases": [
                {
                    "title": "Innovator (2015 Reissue)",
                    "date": "2015-03-10",
                    "label-info": [{ "label": { "name": "Reissue Label" } }]
                },
                {
                    "title": "Innovator",
                    "date": "1991-05",
                    "label-info": [{ "label": { "name": "Transmat" } }]
                }
            ]
        });
        let f = map_recording(&v);
        assert_eq!(f.album.as_deref(), Some("Innovator"));
        assert_eq!(f.year, Some(1991));
        assert_eq!(f.label.as_deref(), Some("Transmat"));
    }

    #[test]
    fn strip_mix_suffix_removes_trailing_parenthetical() {
        assert_eq!(strip_mix_suffix("Strings of Life (Original Mix)"), "Strings of Life");
        assert_eq!(strip_mix_suffix("Track [Extended Mix]"), "Track");
        assert_eq!(strip_mix_suffix("Song (Radio Edit)"), "Song");
    }

    #[test]
    fn strip_mix_suffix_removes_stacked_qualifiers() {
        // Multiple trailing groups are all removed down to the base song title.
        assert_eq!(
            strip_mix_suffix("Hate It Or Love It (BVRNOUT Remix) [CLEAN]"),
            "Hate It Or Love It"
        );
        assert_eq!(strip_mix_suffix("Track (Extended Mix) [Dirty]"), "Track");
    }

    #[test]
    fn strip_mix_suffix_leaves_plain_titles() {
        assert_eq!(strip_mix_suffix("Strings of Life"), "Strings of Life");
        assert_eq!(strip_mix_suffix("Lady"), "Lady");
    }

    #[test]
    fn strip_mix_suffix_only_strips_trailing_bracket() {
        // A parenthetical that is not at the end is left alone.
        assert_eq!(strip_mix_suffix("(Intro) Real Title"), "(Intro) Real Title");
    }

    #[test]
    fn strip_mix_suffix_keeps_title_when_stripping_empties_it() {
        // Nothing but a bracket group -> keep the original rather than return "".
        assert_eq!(strip_mix_suffix("(Untitled)"), "(Untitled)");
    }

    #[test]
    fn map_recording_ignores_undated_release_in_favor_of_dated_one() {
        // An undated release must not win over a dated one, regardless of order.
        let v = json!({
            "title": "Some Track",
            "releases": [
                {
                    "title": "Undated Comp",
                    "label-info": [{ "label": { "name": "Unknown Label" } }]
                },
                {
                    "title": "Original Album",
                    "date": "1998-01-01",
                    "label-info": [{ "label": { "name": "Real Label" } }]
                }
            ]
        });
        let f = map_recording(&v);
        assert_eq!(f.album.as_deref(), Some("Original Album"));
        assert_eq!(f.year, Some(1998));
        assert_eq!(f.label.as_deref(), Some("Real Label"));
    }
}
