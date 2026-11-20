import { Fragment, useEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import * as api from "../api";
import { ALL_COLUMNS } from "../columns";
import type { ColumnDef, SortState } from "../columns";
import {
  SPLIT_CHOICES,
  SPLIT_MAX_COLS,
  cycleSort,
  dragIds,
  isEditableSource,
  moveCol,
  sourceKey,
  toggleCol,
} from "../splitView";
import type { SourceOption, SplitSource } from "../splitView";
import { TrackCell } from "./TrackTable";

const ROW_HEIGHT = 42;
// a crate is rarely this big; past it the overview stops loading rows
const OVERVIEW_LIMIT = 5000;
const PAGE = 500;

export function colsFor(keys: string[]): ColumnDef[] {
  return keys
    .map((k) => ALL_COLUMNS.find((c) => c.key === k))
    .filter((c): c is ColumnDef => c !== undefined);
}

/** A pane header's dropdown of libraries and crates, grouped by library or
 *  drive (options with an empty group sit above the groups). */
export function SourcePicker({
  options,
  value,
  tip,
  placeholder,
  onPick,
}: {
  options: SourceOption[];
  value: string;
  tip: string;
  /** shown when `value` is none of the options (the running order on All tracks) */
  placeholder?: string;
  onPick: (s: SplitSource) => void;
}) {
  const unknown = !options.some((o) => o.key === value);
  const groups: { group: string; items: SourceOption[] }[] = [];
  for (const o of options) {
    const last = groups[groups.length - 1];
    if (last && last.group === o.group) last.items.push(o);
    else groups.push({ group: o.group, items: [o] });
  }
  const item = (o: SourceOption) => (
    <option key={o.key} value={o.key}>
      {o.label}
    </option>
  );
  return (
    <select
      className="pane-source"
      value={value}
      data-tip={tip}
      onChange={(e) => {
        const o = options.find((x) => x.key === e.target.value);
        if (o) onPick(o.source);
      }}
    >
      {unknown && (
        <option value={value} disabled hidden>
          {placeholder ?? "Choose…"}
        </option>
      )}
      {groups.map((g) =>
        g.group === "" ? (
          g.items.map(item)
        ) : (
          <optgroup key={g.group} label={g.group}>
            {g.items.map(item)}
          </optgroup>
        ),
      )}
    </select>
  );
}

/** Header strip above a split pane: its title, a hint, and the gear that
 *  picks that pane's columns (at most SPLIT_MAX_COLS). */
export function PaneHeader({
  title,
  hint,
  cols,
  setCols,
  children,
  onClose,
}: {
  title: string;
  hint: string;
  cols: string[];
  setCols: (c: string[]) => void;
  /** extra controls after the title (the overview's source picker and search) */
  children?: React.ReactNode;
  /** shows a close button at the end (the middle pane) */
  onClose?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const full = cols.length >= SPLIT_MAX_COLS;
  return (
    <div className="pane-head">
      <span className="pane-title">{title}</span>
      {children}
      <span className="pane-hint">{hint}</span>
      <div className="analyze-wrap pane-cols">
        <button
          className="btn icon"
          data-tip={`Choose this side's columns (up to ${SPLIT_MAX_COLS})`}
          onClick={() => setOpen((v) => !v)}
        >
          <svg viewBox="0 0 24 24">
            <path d="M12 8.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7z" />
            <path d="M19.4 13.5a7.6 7.6 0 0 0 0-3l2-1.5-2-3.5-2.4 1a7.6 7.6 0 0 0-2.6-1.5L14 2.5h-4l-.4 2.5A7.6 7.6 0 0 0 7 6.5l-2.4-1-2 3.5 2 1.5a7.6 7.6 0 0 0 0 3l-2 1.5 2 3.5 2.4-1a7.6 7.6 0 0 0 2.6 1.5l.4 2.5h4l.4-2.5a7.6 7.6 0 0 0 2.6-1.5l2.4 1 2-3.5-2-1.5z" />
          </svg>
        </button>
        {open && (
          <div className="menu" onMouseLeave={() => setOpen(false)}>
            <div className="menu-label">
              Columns · {cols.length}/{SPLIT_MAX_COLS}
            </div>
            {SPLIT_CHOICES.map((c) => {
              const on = cols.includes(c.key);
              return (
                <label className={`menu-check${!on && full ? " disabled" : ""}`} key={c.key}>
                  <input
                    type="checkbox"
                    checked={on}
                    disabled={(!on && full) || (on && cols.length === 1)}
                    onChange={() => setCols(toggleCol(cols, c.key))}
                  />
                  {c.label}
                </label>
              );
            })}
          </div>
        )}
      </div>
      {onClose && (
        <button className="btn icon pane-close" data-tip="Close this pane" onClick={onClose}>
          ×
        </button>
      )}
    </div>
  );
}

interface OverviewProps {
  /** lists one source in the given order, filtered by the pane's own search */
  fetchSource: (
    source: SplitSource,
    query: string,
    sort: SortState | null,
    offset: number,
    limit: number,
  ) => Promise<api.TrackPage>;
  source: SplitSource;
  setSource: (s: SplitSource) => void;
  options: SourceOption[];
  /** tracks the running order already holds: greyed in a read-only source */
  inCrate: Set<number>;
  /** changes whenever any crate's contents or order change */
  refreshKey: unknown;
  sort: SortState | null;
  setSort: (s: SortState | null) => void;
  cols: string[];
  setCols: (c: string[]) => void;
  colWidths: Record<string, number>;
  selectedIds: Set<number>;
  playing: api.TrackRow | null;
  onPick: (t: api.TrackRow) => void;
  playTrack: (t: api.TrackRow) => void;
  artToken: number;
  libraryName: (id: number) => string;
  setQualityTrack: (t: api.TrackRow) => void;
  dragTracksRef: React.MutableRefObject<number[] | null>;
  /** set while a drag from a pane runs (the dragged rows by id), so the
   *  running order adds the tracks instead of reordering them */
  foreignDragRef: React.MutableRefObject<Map<number, api.TrackRow> | null>;
  /** the source key of the pane a drag started in (null: the running order) */
  dragFromRef: React.MutableRefObject<string | null>;
  /** dropped tracks land in this pane's crate before row `before` */
  onAdd: (target: SplitSource, ids: number[], before: number) => void;
  /** this pane's own rows moved before row `before` */
  onReorder: (target: SplitSource, ids: number[], before: number) => void;
  /** the middle pane can be closed */
  onClose?: () => void;
}

const NUM_COL: ColumnDef = ALL_COLUMNS.find((c) => c.key === "num")!;

// A split pane other than the running order: any library, folder crate,
// Serato crate or Rekordbox playlist. A library or playlist is a read-only
// source, sorted any way the user likes; a folder or Serato crate shows its
// own order and takes drops, so two crates can be built side by side. A click
// on a track the running order holds finds it there.
export function OverviewPane(props: OverviewProps) {
  const {
    fetchSource,
    source,
    setSource,
    options,
    inCrate,
    refreshKey,
    sort,
    setSort,
    cols,
    setCols,
    colWidths,
    selectedIds,
    playing,
    onPick,
    playTrack,
    artToken,
    libraryName,
    setQualityTrack,
    dragTracksRef,
    foreignDragRef,
    dragFromRef,
    onAdd,
    onReorder,
    onClose,
  } = props;
  const [rows, setRows] = useState<api.TrackRow[]>([]);
  const [total, setTotal] = useState(0);
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  // the pane's own selection, used while showing another source
  const [picked, setPicked] = useState<Set<number>>(new Set());
  const anchor = useRef<number | null>(null);
  const [dragOver, setDragOver] = useState<string | null>(null);
  const [dropAt, setDropAt] = useState<number | null>(null);
  const dragCol = useRef<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const foreign = source.kind !== "crate";
  const editable = isEditableSource(source);
  const key = sourceKey(source);
  // a crate is listed in its own order (a folder crate by position, a Serato
  // crate by its file); a search hides rows, so then it only shows
  const orderKey = editable ? (source.kind === "folder" ? "num:false" : "") : sort ? `${sort.key}:${sort.desc}` : "";
  const canDrop = editable && debounced === "";
  // the latest source object for the fetch, which is keyed by `key`: a fresh
  // object for the same source must not reload
  const sourceRef = useRef(source);
  sourceRef.current = source;

  useEffect(() => {
    const t = setTimeout(() => setDebounced(query), 150);
    return () => clearTimeout(t);
  }, [query]);

  // a new source starts unselected and at the top
  useEffect(() => {
    setPicked(new Set());
    anchor.current = null;
    scrollRef.current?.scrollTo({ top: 0 });
  }, [key]);

  useEffect(() => {
    let stale = false;
    const src = sourceRef.current;
    const [ok, od] = orderKey.split(":");
    const ord: SortState | null = orderKey ? { key: ok, desc: od === "true" } : null;
    (async () => {
      try {
        let page = await fetchSource(src, debounced, ord, 0, PAGE);
        const all = [...page.rows];
        while (all.length < page.total && all.length < OVERVIEW_LIMIT && page.rows.length > 0) {
          if (stale) return;
          page = await fetchSource(src, debounced, ord, all.length, PAGE);
          all.push(...page.rows);
        }
        if (!stale) {
          setRows(all);
          setTotal(page.total);
        }
      } catch {
        if (!stale) {
          setRows([]);
          setTotal(0);
        }
      }
    })();
    return () => {
      stale = true;
    };
  }, [fetchSource, key, debounced, orderKey, refreshKey]);

  const columns = editable ? [NUM_COL, ...colsFor(cols)] : colsFor(cols);
  const template = columns
    .map((c) => (colWidths[c.key] != null ? `${colWidths[c.key]}px` : c.width))
    .join(" ");

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 12,
  });

  const click = (index: number, t: api.TrackRow, e: React.MouseEvent) => {
    if (!foreign) {
      onPick(t);
      return;
    }
    if (e.shiftKey && anchor.current !== null) {
      const [a, b] = [anchor.current, index].sort((x, y) => x - y);
      setPicked(new Set(rows.slice(a, b + 1).map((r) => r.id)));
      return;
    }
    if (e.ctrlKey || e.metaKey) {
      setPicked((cur) => {
        const next = new Set(cur);
        if (next.has(t.id)) next.delete(t.id);
        else next.add(t.id);
        return next;
      });
    } else {
      setPicked(new Set([t.id]));
      if (inCrate.has(t.id)) onPick(t);
    }
    anchor.current = index;
  };

  // a drop into this pane: its own rows reorder, anything else is added
  const acceptsDrag = () => canDrop && dragTracksRef.current !== null;
  const drop = (before: number) => {
    const ids = dragTracksRef.current;
    const own = dragFromRef.current === key;
    dragTracksRef.current = null;
    foreignDragRef.current = null;
    dragFromRef.current = null;
    setDropAt(null);
    if (!ids || ids.length === 0) return;
    if (own) onReorder(source, ids, before);
    else onAdd(source, ids, before);
  };
  const dropEffect = () => (dragFromRef.current === key ? "move" : "copy");

  const known = options.some((o) => o.key === key);

  return (
    <div className="split-pane">
      <PaneHeader
        title={editable ? "Crate" : "Overview"}
        hint={
          editable
            ? canDrop
              ? "drag to reorder · drop tracks to add"
              : "clear the search to edit"
            : foreign
              ? "drag into a crate to add"
              : "sort by any column · click to find"
        }
        cols={cols}
        setCols={setCols}
        onClose={onClose}
      >
        <SourcePicker
          options={options}
          value={known ? key : (options[0]?.key ?? "")}
          tip="Show another library or crate in this pane"
          onPick={setSource}
        />
        <input
          className="pane-search"
          placeholder="Search…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") setQuery("");
          }}
        />
      </PaneHeader>
      <div
        className="table-wrap"
        ref={scrollRef}
        onDragOver={(e) => {
          // below the last row, or an empty crate: the end
          if (!acceptsDrag()) return;
          e.preventDefault();
          e.dataTransfer.dropEffect = dropEffect();
          setDropAt(rows.length);
        }}
        onDragLeave={(e) => {
          if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDropAt(null);
        }}
        onDrop={(e) => {
          if (!acceptsDrag()) return;
          e.preventDefault();
          // past the loaded rows means the crate's true end
          drop(Number.MAX_SAFE_INTEGER);
        }}
      >
        <div className="thead" style={{ gridTemplateColumns: template }}>
          {columns.map((c) => {
            const fixed = c.key === "num";
            const targets = editable
              ? []
              : (c.sorts ?? (c.sortKey ? [{ label: c.label, key: c.sortKey }] : []));
            return (
              <div
                key={c.key}
                className={`${targets.length ? "sortable" : ""} ${dragOver === c.key ? "drag-over" : ""}`}
                draggable={!fixed}
                onDragStart={(e) => {
                  dragCol.current = c.key;
                  e.dataTransfer.effectAllowed = "move";
                }}
                onDragOver={(e) => {
                  if (!dragCol.current || fixed) return;
                  e.preventDefault();
                  e.stopPropagation();
                  e.dataTransfer.dropEffect = "move";
                  setDragOver(c.key);
                }}
                onDragLeave={() => setDragOver((cur) => (cur === c.key ? null : cur))}
                onDrop={(e) => {
                  if (!dragCol.current) return;
                  e.preventDefault();
                  e.stopPropagation();
                  setCols(moveCol(cols, dragCol.current, c.key));
                  dragCol.current = null;
                  setDragOver(null);
                }}
                onDragEnd={() => {
                  dragCol.current = null;
                  setDragOver(null);
                }}
                data-tip={
                  fixed
                    ? "Position in the crate"
                    : targets.length
                      ? "Click a field to sort · drag to reorder"
                      : "Drag to reorder"
                }
              >
                {targets.length === 0
                  ? c.label
                  : targets.map((tg, i) => {
                      const active = sort?.key === tg.key;
                      return (
                        <Fragment key={tg.key}>
                          {i > 0 && <span className="sort-sep">/</span>}
                          <span
                            className="sort-target"
                            onClick={(e) => {
                              e.stopPropagation();
                              setSort(cycleSort(sort, tg.key));
                            }}
                          >
                            {tg.label}
                            <span className={`sort-icon ${active ? "active" : ""}`}>
                              {active ? (sort!.desc ? "▼" : "▲") : "⇅"}
                            </span>
                          </span>
                        </Fragment>
                      );
                    })}
              </div>
            );
          })}
        </div>
        {rows.length === 0 ? (
          <div className={`empty-state${dropAt !== null ? " drop-target" : ""}`}>
            {debounced
              ? "No tracks match the search."
              : editable
                ? "This crate is empty — drop tracks here."
                : foreign
                  ? "Nothing here."
                  : "This crate is empty."}
          </div>
        ) : (
          <div className="tbody" style={{ height: virtualizer.getTotalSize() }}>
            {virtualizer.getVirtualItems().map((v) => {
              const t = rows[v.index];
              const held = foreign && !editable && inCrate.has(t.id);
              const sel = foreign ? picked.has(t.id) : selectedIds.has(t.id);
              const marker =
                dropAt === v.index
                  ? "drop-before"
                  : dropAt === v.index + 1 && v.index === rows.length - 1
                    ? "drop-after"
                    : "";
              return (
                <div
                  key={t.id}
                  className={`trow ${sel ? "selected" : ""} ${playing?.id === t.id ? "playing" : ""} ${
                    t.is_missing ? "missing" : ""
                  } ${held ? "in-crate" : ""} ${marker}`}
                  style={{
                    transform: `translateY(${v.start}px)`,
                    height: v.size,
                    gridTemplateColumns: template,
                  }}
                  data-tip={held ? "Already in this crate · click to find it" : undefined}
                  onClick={(e) => click(v.index, t, e)}
                  onDoubleClick={() => playTrack(t)}
                  draggable={foreign && !held}
                  onDragStart={(e) => {
                    // in a crate the selection drags in its crate order
                    const ids = editable
                      ? picked.has(t.id)
                        ? rows.filter((r) => picked.has(r.id)).map((r) => r.id)
                        : [t.id]
                      : dragIds(t.id, picked, inCrate);
                    if (ids.length === 0) {
                      e.preventDefault();
                      return;
                    }
                    if (!picked.has(t.id)) setPicked(new Set([t.id]));
                    const byId = new Map(rows.map((r) => [r.id, r]));
                    dragTracksRef.current = ids;
                    dragFromRef.current = key;
                    foreignDragRef.current = new Map(
                      ids.flatMap((id) => {
                        const r = byId.get(id);
                        return r ? [[id, r] as [number, api.TrackRow]] : [];
                      }),
                    );
                    e.dataTransfer.setData("spindex/tracks", String(ids.length));
                    e.dataTransfer.effectAllowed = "copyMove";
                  }}
                  onDragOver={(e) => {
                    if (!acceptsDrag()) return;
                    e.preventDefault();
                    e.stopPropagation();
                    e.dataTransfer.dropEffect = dropEffect();
                    const r = e.currentTarget.getBoundingClientRect();
                    const after = e.clientY > r.top + r.height / 2;
                    setDropAt(v.index + (after ? 1 : 0));
                  }}
                  onDrop={(e) => {
                    if (!acceptsDrag()) return;
                    e.preventDefault();
                    e.stopPropagation();
                    drop(dropAt ?? v.index);
                  }}
                  onDragEnd={() => {
                    dragTracksRef.current = null;
                    foreignDragRef.current = null;
                    dragFromRef.current = null;
                    setDropAt(null);
                  }}
                >
                  {columns.map((c) =>
                    c.key === "num" ? (
                      <div className="mono rownum" key={c.key}>
                        <span className="rownum-value">{v.index + 1}</span>
                      </div>
                    ) : (
                      <TrackCell
                        key={c.key}
                        colKey={c.key}
                        t={t}
                        artToken={artToken}
                        libraryName={libraryName}
                        setQualityTrack={setQualityTrack}
                      />
                    ),
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
      {foreign && rows.length > 0 && (
        <div className="pane-foot">
          {total.toLocaleString()} track{total === 1 ? "" : "s"}
          {total > rows.length ? ` · first ${rows.length.toLocaleString()} shown` : ""}
          {picked.size > 0 ? ` · ${picked.size} selected` : ""}
        </div>
      )}
    </div>
  );
}
