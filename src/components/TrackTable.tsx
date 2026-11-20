import { Fragment, useEffect } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import * as api from "../api";
import TrackThumb from "./TrackThumb";
import StarRating from "./StarRating";
import {
  formatDate,
  formatDateFull,
  formatDuration,
  isLossless,
  keyBadgeColor,
  normalizeCamelot,
  qualityColor,
} from "../theme";
import type { ColumnDef, SortState } from "../columns";

function trackDisplayName(t: api.TrackRow): string {
  if (t.title) return t.title;
  const file = t.rel_path.split("/").pop() ?? t.rel_path;
  return file.replace(/\.[^.]+$/, "");
}

interface TrackTableProps {
  activeScanning: boolean;
  scanning: api.ScanProgress | null;
  fileDropTable: boolean;
  setFileDropTable: (v: boolean) => void;
  scrollRef: React.RefObject<HTMLDivElement>;
  dragTracksRef: React.MutableRefObject<number[] | null>;
  /** set while the drag comes from the split view's overview */
  foreignDragRef: React.MutableRefObject<Map<number, api.TrackRow> | null>;
  /** adds overview tracks to the open crate before row `beforeIndex` */
  onAddAt: (ids: number[], beforeIndex: number) => void;
  dragCol: React.MutableRefObject<string | null>;
  activeLibrary: number | null;
  showToast: (text: string, error?: boolean) => void;
  importOsFiles: (libraryId: number, folder: string, files: FileList) => void;
  activeFolder: string | null;
  gridTemplate: string;
  columns: ColumnDef[];
  crateSelected: boolean;
  dragOver: string | null;
  setDragOver: React.Dispatch<React.SetStateAction<string | null>>;
  moveColumn: (from: string | null, to: string) => void;
  effectiveSort: SortState | null;
  toggleSort: (sortKey: string) => void;
  startColResize: (e: React.MouseEvent, key: string) => void;
  setColWidths: React.Dispatch<React.SetStateAction<Record<string, number>>>;
  tracks: api.TrackRow[];
  libraries: api.LibraryInfo[];
  query: string;
  selectedIds: Set<number>;
  setSelectedIds: React.Dispatch<React.SetStateAction<Set<number>>>;
  setAnchorIdx: (i: number | null) => void;
  playing: api.TrackRow | null;
  dropRow: number | null;
  setDropRow: React.Dispatch<React.SetStateAction<number | null>>;
  selectRow: (index: number, e: React.MouseEvent) => void;
  playTrack: (t: api.TrackRow) => void;
  setCtxMenu: (v: { x: number; y: number } | null) => void;
  anyManualMode: boolean;
  seratoManualMode: boolean;
  seratoFilter: { letter: string; file: string; name: string; ids: number[] } | null;
  keepScrollRef: React.MutableRefObject<boolean>;
  setSeratoToken: React.Dispatch<React.SetStateAction<number>>;
  refreshTracks: (opts?: { keepScroll?: boolean }) => Promise<void>;
  /** Fetches the next page; called when the last loaded rows scroll into view. */
  loadMoreTracks: () => void;
  moveSelection: (delta: number, ids?: number[]) => void;
  artToken: number;
  libraryName: (id: number) => string;
  setQualityTrack: (t: api.TrackRow) => void;
  setDropLib: React.Dispatch<React.SetStateAction<number | null>>;
  /** false in the split view's running-order pane: it always shows the
   *  crate's own order, so its headers only drag, never sort */
  sortable?: boolean;
}

// The virtualized track table: header (sort/drag-reorder/resize), the
// scrollable body, drag-and-drop (both internal track drags for the
// sidebar/crate-reorder and OS file drops), and per-cell rendering for
// every column. This owns the react-virtual instance since nothing outside
// the table needs it.
//
// Presentational — all mutation (reorder, delete-on-drop targets elsewhere,
// selection state) is driven by callback/state props owned by App.tsx.
export default function TrackTable(props: TrackTableProps) {
  const {
    activeScanning,
    scanning,
    fileDropTable,
    setFileDropTable,
    scrollRef,
    dragTracksRef,
    foreignDragRef,
    onAddAt,
    dragCol,
    activeLibrary,
    showToast,
    importOsFiles,
    activeFolder,
    gridTemplate,
    columns,
    crateSelected,
    dragOver,
    setDragOver,
    moveColumn,
    effectiveSort,
    toggleSort,
    startColResize,
    setColWidths,
    tracks,
    libraries,
    query,
    selectedIds,
    setSelectedIds,
    setAnchorIdx,
    playing,
    dropRow,
    setDropRow,
    selectRow,
    playTrack,
    setCtxMenu,
    anyManualMode,
    seratoManualMode,
    seratoFilter,
    keepScrollRef,
    setSeratoToken,
    refreshTracks,
    loadMoreTracks,
    moveSelection,
    artToken,
    libraryName,
    setQualityTrack,
    setDropLib,
    sortable = true,
  } = props;

  const virtualizer = useVirtualizer({
    count: tracks.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 42,
    overscan: 12,
  });

  // Only the first page is loaded up front; ask for more once the rows near
  // the end of what is loaded come into view.
  const visible = virtualizer.getVirtualItems();
  const lastVisible = visible.length > 0 ? visible[visible.length - 1].index : -1;
  useEffect(() => {
    if (tracks.length > 0 && lastVisible >= tracks.length - 100) loadMoreTracks();
  }, [lastVisible, tracks.length, loadMoreTracks]);

  return (
    <>
      {activeScanning && scanning && (
        <div className="scan-banner">
          <span className="scan-spinner" />
          Scanning… <b>{scanning.found.toLocaleString()}</b> files found so far
        </div>
      )}

      <div
        className={`table-wrap${fileDropTable ? " file-drop" : ""}`}
        ref={scrollRef}
        onDragOver={(e) => {
          // overview tracks dropped below the last row (or into an empty
          // crate) land at the end; rows handle drops onto themselves
          if (foreignDragRef.current && dragTracksRef.current && anyManualMode) {
            e.preventDefault();
            e.dataTransfer.dropEffect = "copy";
            setDropRow(tracks.length);
            return;
          }
          // internal drags (tracks, column headers) are handled elsewhere
          if (dragTracksRef.current || dragCol.current) return;
          if (!e.dataTransfer.types.includes("Files")) return;
          e.preventDefault();
          const ok = activeLibrary !== null;
          e.dataTransfer.dropEffect = ok ? "copy" : "none";
          setFileDropTable(ok);
        }}
        onDragLeave={(e) => {
          // ignore transitions into child rows; clear only on a real exit
          if (!e.currentTarget.contains(e.relatedTarget as Node | null)) {
            setFileDropTable(false);
            if (foreignDragRef.current) setDropRow(null);
          }
        }}
        onDrop={(e) => {
          if (foreignDragRef.current && dragTracksRef.current && anyManualMode) {
            e.preventDefault();
            const ids = dragTracksRef.current;
            dragTracksRef.current = null;
            foreignDragRef.current = null;
            setDropRow(null);
            // past the loaded rows means the crate's true end
            onAddAt(ids, Number.MAX_SAFE_INTEGER);
            return;
          }
          if (dragTracksRef.current || dragCol.current) return;
          if (!e.dataTransfer.types.includes("Files")) return;
          e.preventDefault();
          setFileDropTable(false);
          if (e.dataTransfer.files.length === 0) return;
          if (activeLibrary === null) {
            showToast(
              "Select a library or crate first — or drop onto a sidebar row",
              true,
            );
            return;
          }
          importOsFiles(activeLibrary, activeFolder ?? "", e.dataTransfer.files);
        }}
      >
        <div className="thead" style={{ gridTemplateColumns: gridTemplate }}>
          {columns.map((c) => {
            const targets =
              !sortable || (c.key === "num" && !crateSelected)
                ? []
                : c.sorts ?? (c.sortKey ? [{ label: c.label, key: c.sortKey }] : []);
            return (
              <div
                key={c.key}
                className={`${targets.length ? "sortable" : ""} ${
                  dragOver === c.key ? "drag-over" : ""
                }`}
                draggable
                onDragStart={(e) => {
                  dragCol.current = c.key;
                  e.dataTransfer.effectAllowed = "move";
                }}
                onDragOver={(e) => {
                  e.preventDefault();
                  e.dataTransfer.dropEffect = "move";
                  setDragOver(c.key);
                }}
                onDragLeave={() => setDragOver((cur) => (cur === c.key ? null : cur))}
                onDrop={(e) => {
                  e.preventDefault();
                  moveColumn(dragCol.current, c.key);
                  dragCol.current = null;
                  setDragOver(null);
                }}
                onDragEnd={() => {
                  dragCol.current = null;
                  setDragOver(null);
                }}
                data-tip={
                  targets.length ? "Click a field to sort · drag to reorder" : "Drag to reorder"
                }
              >
                {targets.length === 0
                  ? c.label
                  : targets.map((tg, i) => {
                      const active = effectiveSort?.key === tg.key;
                      return (
                        <Fragment key={tg.key}>
                          {i > 0 && <span className="sort-sep">/</span>}
                          <span
                            className="sort-target"
                            onClick={(e) => {
                              e.stopPropagation();
                              toggleSort(tg.key);
                            }}
                          >
                            {tg.label}
                            <span className={`sort-icon ${active ? "active" : ""}`}>
                              {active ? (effectiveSort!.desc ? "▼" : "▲") : "⇅"}
                            </span>
                          </span>
                        </Fragment>
                      );
                    })}
                <span
                  className="col-resizer"
                  draggable={false}
                  data-tip="Drag to resize · double-click to reset"
                  onMouseDown={(e) => startColResize(e, c.key)}
                  onClick={(e) => e.stopPropagation()}
                  onDoubleClick={(e) => {
                    e.stopPropagation();
                    setColWidths((prev) => {
                      const next = { ...prev };
                      delete next[c.key];
                      localStorage.setItem("spindex.colwidths", JSON.stringify(next));
                      return next;
                    });
                  }}
                  onDragStart={(e) => {
                    e.preventDefault();
                    e.stopPropagation();
                  }}
                />
              </div>
            );
          })}
        </div>
        {tracks.length === 0 ? (
          <div className="empty-state">
            {libraries.length === 0 ? (
              <>
                <b>No library yet.</b>
                <br />
                Click “Add library” in the sidebar and point SpinDex at your music.
              </>
            ) : activeScanning ? (
              <>Scanning… tracks will appear as they are indexed.</>
            ) : query ? (
              <>No tracks match “{query}”.</>
            ) : (
              <>Library is empty — double-click it in the sidebar to rescan.</>
            )}
          </div>
        ) : (
          <div className="tbody" style={{ height: virtualizer.getTotalSize() }}>
            {virtualizer.getVirtualItems().map((v) => {
              const t = tracks[v.index];
              return (
                <div
                  key={t.id}
                  className={`trow ${selectedIds.has(t.id) ? "selected" : ""} ${
                    playing?.id === t.id ? "playing" : ""
                  } ${t.is_missing ? "missing" : ""} ${
                    dropRow === v.index ? "drop-before" : ""
                  } ${
                    dropRow === v.index + 1 && v.index === tracks.length - 1
                      ? "drop-after"
                      : ""
                  }`}
                  data-tip={
                    t.is_missing
                      ? "File not found on disk — deleted or moved outside SpinDex"
                      : undefined
                  }
                  style={{
                    transform: `translateY(${v.start}px)`,
                    height: v.size,
                    gridTemplateColumns: gridTemplate,
                  }}
                  onClick={(e) => selectRow(v.index, e)}
                  onDoubleClick={() => playTrack(t)}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    if (!selectedIds.has(t.id)) {
                      setSelectedIds(new Set([t.id]));
                      setAnchorIdx(v.index);
                    }
                    setCtxMenu({ x: e.clientX, y: e.clientY });
                  }}
                  draggable
                  onDragStart={(e) => {
                    // dragging a selected row takes the whole selection along
                    const ids = selectedIds.has(t.id) ? Array.from(selectedIds) : [t.id];
                    dragTracksRef.current = ids;
                    e.dataTransfer.setData("spindex/tracks", String(ids.length));
                    // One drag, two legal gestures: dropping on the sidebar
                    // copies, dropping on another row inside a crate
                    // reorders. A dropEffect the browser finds outside
                    // effectAllowed is reset to "none", and on Windows that
                    // turns the release into a dragleave — the reorder drop
                    // would never fire at all.
                    e.dataTransfer.effectAllowed = "copyMove";
                  }}
                  onDragOver={(e) => {
                    // only an internal track drag, and only in manual mode —
                    // an OS file drop is the table wrapper's business and a
                    // drag onto the sidebar must still copy
                    if (!anyManualMode || !dragTracksRef.current) return;
                    e.preventDefault();
                    e.stopPropagation();
                    e.dataTransfer.dropEffect = "move";
                    // above the row's midpoint drops before it, below drops after
                    const r = e.currentTarget.getBoundingClientRect();
                    const after = e.clientY > r.top + r.height / 2;
                    setDropRow(v.index + (after ? 1 : 0));
                  }}
                  onDragLeave={() => setDropRow(null)}
                  onDrop={(e) => {
                    if (!anyManualMode || !dragTracksRef.current) return;
                    e.preventDefault();
                    e.stopPropagation();
                    const ids = dragTracksRef.current;
                    const target = dropRow;
                    const fromOverview = foreignDragRef.current !== null;
                    dragTracksRef.current = null;
                    foreignDragRef.current = null;
                    setDropRow(null);
                    if (target === null) return;
                    if (fromOverview) {
                      onAddAt(ids, target);
                      return;
                    }
                    if (seratoManualMode && seratoFilter) {
                      api
                        .seratoReorderTracks(
                          seratoFilter.letter,
                          seratoFilter.file,
                          ids,
                          target,
                        )
                        .then(() => {
                          // keep the scroll position and the selection: the
                          // fresh crate order arrives via seratoToken
                          keepScrollRef.current = true;
                          setSeratoToken((t) => t + 1);
                        })
                        .catch((err) => showToast(`Could not reorder: ${err}`, true));
                    } else if (activeLibrary !== null && activeFolder !== null) {
                      api
                        .reorderCrateTracks(ids, activeLibrary, activeFolder, target)
                        .then(() => refreshTracks({ keepScroll: true }))
                        .catch((err) => showToast(`Could not reorder: ${err}`, true));
                    }
                  }}
                  onDragEnd={() => {
                    dragTracksRef.current = null;
                    setDropLib(null);
                    setDropRow(null);
                  }}
                >
                  {columns.map((c) => {
                    switch (c.key) {
                      case "num":
                        return (
                          <div className="mono rownum" key={c.key}>
                            <span className="rownum-value">
                              {crateSelected ? (t.crate_pos ?? v.index + 1) : v.index + 1}
                            </span>
                            {anyManualMode && (
                              <span className="rowmove">
                                <button
                                  type="button"
                                  disabled={v.index === 0}
                                  data-tip="Move up"
                                  onClick={(e) => {
                                    e.stopPropagation();
                                    moveSelection(-1, selectedIds.has(t.id) ? undefined : [t.id]);
                                  }}
                                >
                                  ▲
                                </button>
                                <button
                                  type="button"
                                  disabled={v.index === tracks.length - 1}
                                  data-tip="Move down"
                                  onClick={(e) => {
                                    e.stopPropagation();
                                    moveSelection(1, selectedIds.has(t.id) ? undefined : [t.id]);
                                  }}
                                >
                                  ▼
                                </button>
                              </span>
                            )}
                          </div>
                        );
                      default:
                        return (
                          <TrackCell
                            key={c.key}
                            colKey={c.key}
                            t={t}
                            artToken={artToken}
                            libraryName={libraryName}
                            setQualityTrack={setQualityTrack}
                          />
                        );
                    }
                  })}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </>
  );
}

/** One body cell of a track row — every column but "#", whose content depends
 *  on the table it sits in. Shared by the main table and the split overview. */
export function TrackCell({
  colKey,
  t,
  artToken,
  libraryName,
  setQualityTrack,
}: {
  colKey: string;
  t: api.TrackRow;
  artToken: number;
  libraryName: (id: number) => string;
  setQualityTrack: (t: api.TrackRow) => void;
}) {
  const camelot = normalizeCamelot(t.key_camelot);
  const metaOk = t.bpm != null && camelot != null;
  switch (colKey) {
    case "cover":
      return (
        <div className="t-cover">
          <TrackThumb trackId={t.id} hasArtwork={t.has_artwork} artToken={artToken} />
        </div>
      );
    case "title":
      return (
        <div className="t-title">
          <div className="name">{trackDisplayName(t)}</div>
          <div className="artist">{t.artist ?? "—"}</div>
        </div>
      );
    case "album":
      return <div className="dim">{t.album ?? "—"}</div>;
    case "genre":
      return <div className="dim">{t.genre ?? "—"}</div>;
    case "library":
      return (
        <div className="dim">
          {libraryName(t.library_id)}
        </div>
      );
    case "bpm":
      return (
        <div className="mono">
          {t.bpm != null ? t.bpm.toFixed(0) : "—"}
        </div>
      );
    case "key": {
      const uncertain =
        t.key_source !== "manual" &&
        t.key_confidence != null &&
        t.key_confidence < 0.7;
      const cands = api.parseKeyCandidates(t.key_candidates);
      const alts = cands
        .slice(1)
        .map((k) => `${k.key} (${Math.round(k.p * 100)}%)`)
        .join(", ");
      const tip =
        t.key_source === "manual"
          ? "Key set manually (locked)"
          : cands.length > 0
            ? `Key ${cands[0].key} (${Math.round(cands[0].p * 100)}%)${
                alts ? ` · also possible: ${alts}` : ""
              } — edit to lock`
            : uncertain
              ? `Key uncertain (${Math.round((t.key_confidence ?? 0) * 100)}%) — verify by ear, edit to lock`
              : undefined;
      return (
        <div>
          {t.key_camelot ? (
            <span
              className={`key-badge ${uncertain ? "uncertain" : ""}`}
              style={{ background: keyBadgeColor(t.key_camelot) }}
              data-tip={tip}
            >
              {camelot ?? t.key_camelot}
            </span>
          ) : (
            <span className="dim">—</span>
          )}
        </div>
      );
    }
    case "rating":
      return (
        <div>
          {t.rating != null ? <StarRating value={t.rating} /> : <span className="dim">—</span>}
        </div>
      );
    case "time":
      return (
        <div className="mono">
          {formatDuration(t.duration_secs)}
        </div>
      );
    case "format":
      return (
        <div
          className={isLossless(t.format) ? "mono lossless" : "mono dim"}
        >
          {t.format ? t.format.toUpperCase() : "—"}
        </div>
      );
    case "bitrate":
      return (
        <div className="mono dim">
          {t.bitrate_kbps != null ? `${t.bitrate_kbps}k` : "—"}
        </div>
      );
    case "quality":
      return (
        <div>
          {t.quality_score != null ? (
            <span
              className="quality-badge"
              style={{ color: qualityColor(t.quality_score) }}
              onClick={(e) => {
                e.stopPropagation();
                setQualityTrack(t);
              }}
              data-tip={
                t.cutoff_hz != null
                  ? `Score ${t.quality_score} · bandwidth ~${(t.cutoff_hz / 1000).toFixed(1)} kHz — click for details`
                  : `Score ${t.quality_score} — click for details`
              }
            >
              {t.quality_score}
            </span>
          ) : (
            <span className="dim">—</span>
          )}
        </div>
      );
    case "added":
      return (
        <div className="mono dim" data-tip={formatDateFull(t.added_at)}>
          {formatDate(t.added_at)}
        </div>
      );
    case "updated":
      return (
        <div
          className="mono dim"
          data-tip={formatDateFull(t.modified_at)}
        >
          {formatDate(t.modified_at)}
        </div>
      );
    case "status":
      return (
        <div>
          <span className={`flag ${metaOk ? "ok" : "miss"}`}>
            {metaOk ? "OK" : "META"}
          </span>
        </div>
      );
    default:
      return <div />;
  }
}
