import { useCallback, useEffect, useRef, useState } from "react";
import * as api from "../api";
import ModalBackdrop from "./ModalBackdrop";
import { trackDisplayName, type DeleteItem } from "./DeleteConfirm";
import { formatDuration, qualityColor } from "../theme";

interface Props {
  /** Opens the shared delete confirm; resolves with the user's answer. */
  askDelete: (items: DeleteItem[]) => Promise<boolean>;
  libraries: api.LibraryInfo[];
  /** A library, folder crate, Serato crate or Rekordbox playlist. */
  set: api.TrackSetRef;
  setName: string;
  /** Report only: no ticking, no delete (Rekordbox playlists). */
  readOnly?: boolean;
  /** Serato crates: drop one copy's link from the crate, file untouched. */
  onRemoveLink?: (trackIds: number[]) => Promise<void>;
  /** Loads a track in the deck so a weak group can be checked by ear. */
  onPlay: (trackId: number) => void;
  /** Fired after a successful delete with the submitted ids, so the caller can
   *  refresh health counts and clear the deck if the loaded track was deleted. */
  onDeleted: (deletedIds: number[]) => void;
  onClose: () => void;
  onError: (message: string) => void;
  /** Non-error notifications (e.g. a clean delete summary) — rendered without
   *  error styling, unlike onError. */
  onNotify: (message: string) => void;
}

const LEVELS: {
  key: api.DupeGroup["level"];
  label: string;
  reason: string;
  onByDefault: boolean;
}[] = [
  {
    key: "identical",
    label: "Identical content",
    reason: "Same file content (SHA-256 match)",
    onByDefault: true,
  },
  {
    key: "same_tags",
    label: "Same artist + title",
    reason: "Artist and title match after normalization",
    onByDefault: true,
  },
  {
    key: "similar",
    label: "Similar",
    reason: "Duration within 2s and titles nearly match",
    onByDefault: true,
  },
  {
    key: "same_filename",
    label: "Same filename",
    reason: "Only the filename matches — verify before deleting",
    onByDefault: false,
  },
];

function formatSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${Math.round(bytes / 1024)} KB`;
}

/** "Artist — Title", falling back to the filename stem when untagged. */
const displayName = (t: api.DupeTrackInfo): string => trackDisplayName(t);

/** Splits a library-relative path into its crate/folder part and the filename. */
function splitPath(relPath: string): { folder: string | null; file: string } {
  const cut = relPath.lastIndexOf("/");
  if (cut < 0) return { folder: null, file: relPath };
  return { folder: relPath.slice(0, cut + 1), file: relPath.slice(cut + 1) };
}

export default function DuplicatesModal({
  askDelete,
  libraries,
  set,
  setName,
  readOnly,
  onRemoveLink,
  onPlay,
  onDeleted,
  onClose,
  onError,
  onNotify,
}: Props) {
  const [report, setReport] = useState<api.DupeReport | null>(null);
  const [hashing, setHashing] = useState<api.HashProgress | null>(null);
  const [levelsOn, setLevelsOn] = useState<Set<api.DupeGroup["level"]>>(
    () => new Set(LEVELS.filter((l) => l.onByDefault).map((l) => l.key)),
  );
  const [selected, setSelected] = useState<Set<number>>(new Set());

  // Held in a ref (not a `load` dependency) so an unrelated App re-render that
  // passes a fresh inline `onError` identity can't retrigger the fetch effect
  // below and silently wipe an in-progress selection.
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;

  const key = api.setKey(set);
  const load = useCallback(() => {
    api
      .findDuplicatesInSet(JSON.parse(key))
      .then((r) => {
        setReport(r);
        setSelected(new Set());
      })
      .catch((e) => onErrorRef.current(String(e)));
  }, [key]);

  useEffect(load, [load]);

  useEffect(() => {
    const unsubs = [
      api.onHashProgress((p) => setHashing(p)),
      api.onHashDone(() => {
        setHashing(null);
        load();
      }),
    ];
    return () => {
      unsubs.forEach((u) => u.then((f) => f()).catch(() => {}));
    };
  }, [load]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onClose]);

  const startHashing = () => {
    api
      .startHashing(
        set.kind === "library" ? [set.id] : set.kind === "folder" ? [set.library_id] : null,
      )
      .catch((e) => onError(String(e)));
    setHashing({ done: 0, failed: 0, total: report?.unhashed ?? 0 });
  };

  const visible = (report?.groups ?? []).filter((g) => levelsOn.has(g.level));
  const counts = (level: api.DupeGroup["level"]) =>
    (report?.groups ?? []).filter((g) => g.level === level).length;

  // A track hidden by the chip filter must never be counted or deletable —
  // toggling a chip off can't leave stale, invisible ids in the "selected"
  // count that the delete button would then act on.
  const visibleIds = new Set(visible.flatMap((g) => g.tracks.map((t) => t.id)));
  const selectedVisible = Array.from(selected).filter((id) => visibleIds.has(id));

  const toggle = (id: number) =>
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const selectOthers = (g: api.DupeGroup) =>
    setSelected((s) => {
      const next = new Set(s);
      g.tracks.forEach((t) => {
        if (t.id !== g.suggested_keeper_id) next.add(t.id);
      });
      return next;
    });

  // Bulk is scoped to the visible (chip-filtered) groups — never hidden ones.
  const selectAllLosers = () =>
    setSelected(
      new Set(
        visible.flatMap((g) =>
          g.tracks.filter((t) => t.id !== g.suggested_keeper_id).map((t) => t.id),
        ),
      ),
    );

  const deleteSelected = async () => {
    const ids = selectedVisible;
    if (ids.length === 0) return;
    // The keeper is a recommendation, not a lock — but a selection that would
    // remove every copy of a track is refused, so a duplicate cleanup can never
    // delete the track itself.
    const chosen = new Set(ids);
    const wipedOut = visible.filter(
      (g) => g.tracks.length > 0 && g.tracks.every((t) => chosen.has(t.id)),
    );
    if (wipedOut.length > 0) {
      const names = wipedOut
        .slice(0, 3)
        .map((g) => displayName(g.tracks.find((t) => t.id === g.suggested_keeper_id) ?? g.tracks[0]))
        .join(", ");
      onError(
        `That would delete every copy of ${wipedOut.length} track${
          wipedOut.length === 1 ? "" : "s"
        } (${names}${wipedOut.length > 3 ? ", …" : ""}). Leave one copy of each ticked off.`,
      );
      return;
    }
    const removable = new Set(libraries.filter((l) => l.is_removable).map((l) => l.id));
    const byId = new Map(visible.flatMap((g) => g.tracks).map((t) => [t.id, t]));
    const items: DeleteItem[] = ids.flatMap((id) => {
      const t = byId.get(id);
      return t
        ? [{ name: trackDisplayName(t), onStick: removable.has(t.library_id), missing: false }]
        : [];
    });
    const ok = await askDelete(items);
    if (!ok) return;
    try {
      const res = await api.deleteTracks(ids);
      const summary =
        `Deleted ${res.deleted}` +
        (res.failed.length > 0 ? ` · failed: ${res.failed.join(", ")}` : "");
      // A partial failure is worth flagging as an error; a clean delete is not.
      if (res.failed.length > 0) onError(summary);
      else onNotify(summary);
      onDeleted(ids);
      load();
    } catch (e) {
      onError(String(e));
    }
  };

  return (
    <ModalBackdrop onClose={onClose}>
      <div className="modal wide" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title">Duplicates — {setName}</div>
        <div className="modal-sub">
          {report
            ? `${visible.length} of ${report.groups.length} groups shown · ` +
              `${visible.reduce((n, g) => n + g.tracks.length, 0)} tracks`
            : "scanning…"}
        </div>

        {report && (
          <div className="dupe-chips">
            {LEVELS.map((l) => {
              const n = counts(l.key);
              return (
                <button
                  key={l.key}
                  className={`dupe-chip ${l.key} ${levelsOn.has(l.key) ? "on" : ""}`}
                  disabled={n === 0}
                  onClick={() =>
                    setLevelsOn((s) => {
                      const next = new Set(s);
                      if (next.has(l.key)) next.delete(l.key);
                      else next.add(l.key);
                      return next;
                    })
                  }
                >
                  {l.label} <span className="count mono">{n}</span>
                </button>
              );
            })}
          </div>
        )}

        {report && report.unhashed > 0 && (
          <div className="dupe-banner">
            {hashing ? (
              <span>
                Hashing {hashing.done + hashing.failed}/{hashing.total}…
              </span>
            ) : (
              <>
                <span>
                  {report.unhashed.toLocaleString()} tracks not yet hashed — identical-content
                  detection is incomplete.
                </span>
                <button className="btn" onClick={startHashing}>
                  Hash now
                </button>
              </>
            )}
          </div>
        )}

        <div className="dupe-list">
          {report && visible.length === 0 && (
            <div className="empty-state">
              {report.groups.length === 0
                ? "No duplicates found here."
                : "No groups match the selected levels."}
            </div>
          )}
          {visible.map((g, i) => {
            const meta = LEVELS.find((l) => l.key === g.level);
            // Inside a group the track is constant and the LOCATION is what
            // differs — so the track is named once here and every row leads
            // with its path.
            const keeperTrack =
              g.tracks.find((t) => t.id === g.suggested_keeper_id) ?? g.tracks[0];
            const groupName = displayName(keeperTrack);
            return (
              <div className="dupe-group" key={i}>
                <div className={`dupe-level ${g.level}`}>
                  <span className="dupe-level-tag">{meta?.label ?? g.level}</span>
                  <span className="dupe-track-name">{groupName}</span>
                  <span className="dupe-reason">{meta?.reason}</span>
                  {!readOnly && (
                    <button className="link-btn" onClick={() => selectOthers(g)}>
                      Select others
                    </button>
                  )}
                </div>
                {g.tracks.map((t) => {
                  const keeper = t.id === g.suggested_keeper_id;
                  const { folder, file } = splitPath(t.rel_path);
                  const own = displayName(t);
                  return (
                    <div className={`dupe-track ${keeper ? "keeper" : ""}`} key={t.id}>
                      <div className="dupe-pick">
                        {/* Every copy is tickable; KEEP only marks the one
                            SpinDex recommends. Deleting a whole group is
                            refused in deleteSelected, not by hiding the box. */}
                        {!readOnly && (
                          <input
                            type="checkbox"
                            checked={selected.has(t.id)}
                            onChange={() => toggle(t.id)}
                            title={keeper ? "Recommended keeper — tick to delete it anyway" : undefined}
                          />
                        )}
                        {keeper && <span className="keep-badge">KEEP</span>}
                      </div>
                      <div className="dupe-where mono" title={`${t.rel_path}\n${own}`}>
                        {folder ? (
                          <span className="dupe-folder">{folder}</span>
                        ) : (
                          <span className="dupe-root">(root) </span>
                        )}
                        <span className="dupe-file">{file}</span>
                        {/* Every row states its own tags, so all levels read the
                            same way; a copy disagreeing with the group header
                            stands out in full text rather than muted. */}
                        <span className={own === groupName ? "dupe-alt" : "dupe-alt differs"}>
                          {" · "}
                          {`“${own}”`}
                        </span>
                      </div>
                      <div className="dupe-tech mono">
                        <span>{t.format?.toUpperCase() ?? "—"}</span>
                        <span>{t.bitrate_kbps != null ? `${t.bitrate_kbps}k` : "—"}</span>
                        <span>{formatDuration(t.duration_secs)}</span>
                        <span>{formatSize(t.file_size)}</span>
                        <span
                          style={
                            t.quality_score != null
                              ? { color: qualityColor(t.quality_score) }
                              : undefined
                          }
                        >
                          {t.quality_score ?? "—"}
                        </span>
                      </div>
                      <div className="dupe-row-actions">
                        <button className="link-btn" onClick={() => onPlay(t.id)}>
                          Play
                        </button>
                        <button
                          className="link-btn"
                          onClick={() => api.showInExplorer(t.id).catch((e) => onError(String(e)))}
                        >
                          Show
                        </button>
                        {onRemoveLink && !keeper && (
                          <button
                            className="link-btn"
                            data-tip="Remove this copy from the crate; the file stays"
                            onClick={() => {
                              const ok = window.confirm(
                                "Remove this copy's link from the Serato crate?\n\n" +
                                  "The file stays on disk. Close Serato first: it rewrites " +
                                  "its crates when it exits.",
                              );
                              if (!ok) return;
                              onRemoveLink([t.id])
                                .then(() => {
                                  onNotify("Link removed from the crate");
                                  load();
                                })
                                .catch((e) => onError(String(e)));
                            }}
                          >
                            Remove link
                          </button>
                        )}
                      </div>
                    </div>
                  );
                })}
              </div>
            );
          })}
        </div>

        {readOnly && (
          <p className="modal-note">Rekordbox playlists are read-only here.</p>
        )}
        <div className="modal-actions dupe-actions">
          {!readOnly && (
          <>
          <button className="btn" onClick={selectAllLosers} disabled={visible.length === 0}>
            Select all suggested losers (
            {visible.reduce(
              (n, g) => n + g.tracks.filter((t) => t.id !== g.suggested_keeper_id).length,
              0,
            )}
            )
          </button>
          <button
            className="btn danger"
            onClick={deleteSelected}
            disabled={selectedVisible.length === 0}
          >
            Delete {selectedVisible.length} selected to Recycle Bin
          </button>
          </>
          )}
          <button className="btn" onClick={onClose}>
            Close
          </button>
        </div>
      </div>
    </ModalBackdrop>
  );
}
