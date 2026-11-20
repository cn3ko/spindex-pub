//! Resolution of stick-relative paths to track IDs.
//!
//! A Serato crate's `ptrk` entries and a Rekordbox library's track references
//! store paths relative to the stick root (e.g., "MP3/track.mp3"). This module
//! converts those paths to SpinDex track IDs by matching against the indexed
//! libraries on a given USB device, using longest-root-first matching so that a
//! nested library's root cannot shadow itself.

use rusqlite::Connection;

/// Libraries living on one drive, longest root first so a nested library
/// ("D:\MP3\") is matched before a library at the drive root ("D:\").
///
/// Public so a caller resolving MANY paths (a Serato crate's links, a
/// Rekordbox playlist's entries) can fetch this ONCE and feed it to
/// [`resolve_with`] for each path. `db::list_libraries` runs a correlated
/// per-library track-count subquery, so calling it per path — which
/// [`resolve_stick_relative_path`] necessarily does — turns a crate listing
/// into one such query per track, on the Tauri event loop.
pub fn libraries(
    conn: &Connection,
    letter: &str,
) -> Result<Vec<(i64, String)>, String> {
    let prefix = format!("{letter}\\");
    let mut libs: Vec<(i64, String)> = crate::core::db::list_libraries(conn)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|l| l.root_path.starts_with(&prefix))
        .map(|l| (l.id, l.root_path))
        .collect();
    libs.sort_by_key(|(_, root)| std::cmp::Reverse(root.len()));
    Ok(libs)
}

/// The track id behind one crate entry's stick-relative path, or None
/// when the link dangles (file renamed, moved, or never indexed). The single
/// definition of "which track is this stick path", shared by the crate viewer
/// and the crate writer so they can never disagree.
///
/// Convenience wrapper for one-off lookups: it fetches the drive's library
/// list and delegates to [`resolve_with`]. A caller in a loop should call
/// [`libraries`] once and [`resolve_with`] per path instead.
pub fn resolve_stick_relative_path(
    conn: &Connection,
    letter: &str,
    rel: &str,
) -> Option<i64> {
    // A database error here is NOT "this path dangles" — say so, since the
    // Option return has no room to carry the distinction. Callers that can
    // propagate an error should call `libraries` themselves.
    let libs = match libraries(conn, letter) {
        Ok(libs) => libs,
        Err(e) => {
            tracing::warn!("could not list libraries on {letter} while resolving {rel}: {e}");
            return None;
        }
    };
    resolve_with(&libs, conn, letter, rel)
}

/// [`resolve_stick_relative_path`]'s matching half, against an already
/// fetched library list. Returning None here really does mean "no indexed
/// track matches this path".
pub fn resolve_with(
    libs: &[(i64, String)],
    conn: &Connection,
    letter: &str,
    rel: &str,
) -> Option<i64> {
    let full = format!("{letter}\\{}", rel.replace('/', "\\"));
    libs.iter().find_map(|(id, root)| {
        let tail = full.strip_prefix(root.as_str())?;
        // A root stored without a trailing separator ("D:\MP3") must be
        // followed by one, both to strip it and so "D:\MP" never claims
        // "D:\MP3\x.mp3". A drive root ("D:\") already ends in one.
        let tail = if root.ends_with('\\') { tail } else { tail.strip_prefix('\\')? };
        crate::core::db::track_id_by_rel(conn, *id, &tail.replace('\\', "/"))
            .ok()
            .flatten()
    })
}

/// Turns a crate's entries (one `Option<String>` per chunk — `None` for a
/// header chunk) into the single track-id sequence the crate viewer and the
/// crate writer must both index against: the per-entry resolution, the id
/// sequence itself, and how many entries dangle.
///
/// Duplicates are collapsed to their FIRST occurrence. A crate may legitimately
/// link the same file twice, but the table cannot show it twice: `list_tracks`
/// filters on an id set and returns one row per track. If the writer counted
/// both entries its indices would run one ahead of the screen's after the
/// duplicate, and a drop would land in the wrong slot. Resolved to `None`, the
/// later duplicate behaves exactly like a dangling link — it does not
/// participate in the reorder, keeps its file index, and is never lost.
pub fn resolve_crate_entries<I: IntoIterator<Item = Option<String>>>(
    conn: &rusqlite::Connection,
    letter: &str,
    entries: I,
) -> (Vec<Option<i64>>, Vec<i64>, u64) {
    let mut resolved = Vec::new();
    let mut order: Vec<i64> = Vec::new();
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut missing = 0u64;
    // Fetched ONCE for the whole crate: `db::list_libraries` carries a
    // correlated per-library track-count subquery, so resolving each path
    // through the convenience wrapper would re-run it per crate entry.
    let libs = match libraries(conn, letter) {
        Ok(libs) => libs,
        Err(e) => {
            tracing::warn!("could not list libraries on {letter}: {e}");
            Vec::new()
        }
    };
    for entry in entries {
        // a header chunk carries no path: it never resolves and never counts
        // as missing, it simply holds its place in the chunk list
        let Some(rel) = entry else {
            resolved.push(None);
            continue;
        };
        match resolve_with(&libs, conn, letter, &rel) {
            Some(id) if seen.insert(id) => {
                resolved.push(Some(id));
                order.push(id);
            }
            Some(_) => resolved.push(None), // duplicate link: the first one owns the track
            None => {
                resolved.push(None);
                missing += 1;
            }
        }
    }
    (resolved, order, missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stick indexed the way real ones are: a recursive library at the drive
    /// root plus a nested library for a folder inside it, so the SAME file is
    /// reachable through both ("MP3\a.mp3" under the root library, "a.mp3"
    /// under the nested one) and both rows exist in the index.
    fn resolver_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::core::db::SCHEMA).unwrap();
        crate::core::db::migrate(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO library (id, name, root_path) VALUES
                 (1, 'root', 'D:\\'), (2, 'mp3', 'D:\\MP3\\');
             INSERT INTO track (id, library_id, rel_path, file_size, modified_at) VALUES
                 (10, 1, 'loose.mp3', 1, '0'),
                 (11, 1, 'MP3/a.mp3', 1, '0'),
                 (20, 2, 'a.mp3', 1, '0');",
        )
        .unwrap();
        conn
    }

    #[test]
    fn a_root_without_trailing_separator_resolves_on_a_folder_boundary_only() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::core::db::SCHEMA).unwrap();
        crate::core::db::migrate(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO library (id, name, root_path) VALUES
                 (1, 'mp', 'Z:\\MP'), (2, 'mp3', 'Z:\\MP3');
             INSERT INTO track (id, library_id, rel_path, file_size, modified_at) VALUES
                 (10, 2, 'x.mp3', 1, '0');",
        )
        .unwrap();
        let libs = libraries(&conn, "Z:").unwrap();
        assert_eq!(resolve_with(&libs, &conn, "Z:", "MP3/x.mp3"), Some(10));
        assert_eq!(resolve_with(&libs, &conn, "Z:", "MP3x.mp3"), None, "no false prefix match");
    }

    #[test]
    fn longest_root_wins_so_a_nested_library_is_not_swallowed() {
        let conn = resolver_db();
        let libs = libraries(&conn, "D:").unwrap();
        assert_eq!(libs[0].1, "D:\\MP3\\", "longest root must be tried first");
        // "MP3/a.mp3" belongs to library 2, not to the root library — even
        // though the root library also indexes that exact file as "MP3/a.mp3"
        // (the normal configuration on a stick with loose root audio). Pinned
        // so a change to the sort key cannot silently flip which row wins.
        assert_eq!(resolve_stick_relative_path(&conn, "D:", "MP3/a.mp3"), Some(20));
        assert_eq!(resolve_stick_relative_path(&conn, "D:", "loose.mp3"), Some(10));
    }

    #[test]
    fn an_unknown_path_dangles() {
        let conn = resolver_db();
        assert_eq!(resolve_stick_relative_path(&conn, "D:", "MP3/gone.mp3"), None);
    }
}
