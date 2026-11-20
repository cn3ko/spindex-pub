// Split view: a read-only overview of the open crate (left) beside its
// running order (right). Each pane shows at most SPLIT_MAX_COLS columns of
// its own choosing; the right pane always shows "#" on top of those.
// Everything here is a per-machine view preference, kept in localStorage.

import { ALL_COLUMNS } from "./columns";
import type { SortState } from "./columns";

export const SPLIT_MAX_COLS = 6;
export const SPLIT_DEFAULT_COLS = ["title", "key", "bpm", "time", "added"];
/** The default before "added" joined it: a pane still on it gets the new one. */
const OLD_DEFAULT_COLS = ["title", "key", "bpm", "time"];
export const SPLIT_DEFAULT_SORT: SortState = { key: "key", desc: false };
export const SPLIT_MIN_FRAC = 0.2;
export const SPLIT_MAX_FRAC = 0.8;

/** Columns a pane may show: "#" belongs to the running order alone, and
 *  "library" means nothing inside a single crate. */
export const SPLIT_CHOICES = ALL_COLUMNS.filter((c) => c.key !== "num" && c.key !== "library");

/** Keeps known, distinct choices, at most SPLIT_MAX_COLS; never empty. */
export function cleanCols(cols: unknown): string[] {
  if (!Array.isArray(cols)) return [...SPLIT_DEFAULT_COLS];
  if (JSON.stringify(cols) === JSON.stringify(OLD_DEFAULT_COLS)) return [...SPLIT_DEFAULT_COLS];
  const known = new Set(SPLIT_CHOICES.map((c) => c.key));
  const out: string[] = [];
  for (const k of cols) {
    if (typeof k === "string" && known.has(k) && !out.includes(k)) out.push(k);
    if (out.length === SPLIT_MAX_COLS) break;
  }
  return out.length > 0 ? out : [...SPLIT_DEFAULT_COLS];
}

/** Ticking a column adds it at the end (refused at the limit); unticking
 *  removes it (refused for the last one, so a pane always shows something). */
export function toggleCol(cols: string[], key: string): string[] {
  if (cols.includes(key)) return cols.length > 1 ? cols.filter((k) => k !== key) : cols;
  return cols.length < SPLIT_MAX_COLS ? [...cols, key] : cols;
}

/** Drop the dragged column at the target column's position. */
export function moveCol(cols: string[], from: string | null, to: string): string[] {
  if (!from || from === to || !cols.includes(from)) return cols;
  const next = cols.filter((k) => k !== from);
  const at = next.indexOf(to);
  if (at < 0) return cols;
  next.splice(at, 0, from);
  return next;
}

/** asc -> desc -> the backend's default order, like the main table. */
export function cycleSort(cur: SortState | null, key: string): SortState | null {
  if (cur?.key !== key) return { key, desc: false };
  if (!cur.desc) return { key, desc: true };
  return null;
}

/** Scroll offset that brings row `index` to the middle of the viewport. */
export function centerOffset(index: number, rowHeight: number, header: number, viewport: number): number {
  return Math.max(0, header + index * rowHeight + rowHeight / 2 - viewport / 2);
}

function read<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    if (raw != null) return JSON.parse(raw) as T;
  } catch {
    // fall through to default
  }
  return fallback;
}

function write(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // a view preference; losing it is harmless
  }
}

export const loadSplitOn = () => read<unknown>("spindex.split.on", false) === true;
export const saveSplitOn = (v: boolean) => write("spindex.split.on", v);
export type SplitSide = "left" | "right" | "middle";
export const loadSplitCols = (side: SplitSide) =>
  cleanCols(read<unknown>(`spindex.split.${side}cols`, null));
export const saveSplitCols = (side: SplitSide, cols: string[]) =>
  write(`spindex.split.${side}cols`, cols);

export function loadSplitSort(side: "left" | "middle" = "left"): SortState | null {
  const v = read<unknown>(`spindex.split.${side}sort`, SPLIT_DEFAULT_SORT);
  if (v === null) return null;
  if (v && typeof v === "object" && typeof (v as SortState).key === "string") {
    const s = v as SortState;
    if (s.key !== "num") return { key: s.key, desc: !!s.desc };
  }
  return SPLIT_DEFAULT_SORT;
}
export const saveSplitSort = (s: SortState | null, side: "left" | "middle" = "left") =>
  write(`spindex.split.${side}sort`, s);

export function loadSplitFrac(): number {
  const v = Number(read<unknown>("spindex.split.frac", 0.5));
  return Number.isFinite(v) ? Math.min(SPLIT_MAX_FRAC, Math.max(SPLIT_MIN_FRAC, v)) : 0.5;
}
export const saveSplitFrac = (f: number) => write("spindex.split.frac", f);

// ---- Three panes ---------------------------------------------------------
// A second editable pane sits between the overview and the running order, so
// two crates can be built side by side. Its share of the width, and whether
// it is open, persist like the rest of the split view.

export const SPLIT_PANE_MIN = 0.15;
export type Fracs3 = [number, number, number];
const EVEN3: Fracs3 = [1 / 3, 1 / 3, 1 / 3];

/** Three shares that each meet the minimum and sum to 1; anything else is
 *  an even split. */
export function cleanFracs3(v: unknown): Fracs3 {
  if (!Array.isArray(v) || v.length !== 3) return [...EVEN3];
  const n = v.map(Number);
  const sum = n.reduce((a, b) => a + b, 0);
  if (n.some((x) => !Number.isFinite(x) || x < SPLIT_PANE_MIN) || Math.abs(sum - 1) > 1e-6) {
    return [...EVEN3];
  }
  return [n[0], n[1], n[2]];
}

/** Dragging divider 0 (between panes 1 and 2) or 1 (between 2 and 3) to
 *  `at`, a share of the full width: only the two panes it separates change,
 *  and neither goes below the minimum. */
export function resizeThree(f: Fracs3, divider: 0 | 1, at: number): Fracs3 {
  if (divider === 0) {
    const pair = f[0] + f[1];
    const a = Math.min(pair - SPLIT_PANE_MIN, Math.max(SPLIT_PANE_MIN, at));
    return [a, pair - a, f[2]];
  }
  const pair = f[1] + f[2];
  const edge = Math.min(1 - SPLIT_PANE_MIN, Math.max(f[0] + SPLIT_PANE_MIN, at));
  const b = edge - f[0];
  return [f[0], b, pair - b];
}

export const loadSplitFracs3 = () => cleanFracs3(read<unknown>("spindex.split.fracs3", null));
export const saveSplitFracs3 = (f: Fracs3) => write("spindex.split.fracs3", f);
export const loadSplitMiddleOn = () => read<unknown>("spindex.split.middle", false) === true;
export const saveSplitMiddleOn = (v: boolean) => write("spindex.split.middle", v);
export const loadSplitMiddleSource = () => read<unknown>("spindex.split.middlesource", "");
export const saveSplitMiddleSource = (key: string) => write("spindex.split.middlesource", key);

// ---- Source of the left pane -------------------------------------------
// The overview can show the open crate itself or any other place a track can
// come from; dragging a track from there into the running order adds it.

export type SplitSource =
  | { kind: "crate" }
  | { kind: "library"; libraryId: number }
  | { kind: "folder"; libraryId: number; path: string }
  | { kind: "serato"; letter: string; file: string }
  | { kind: "rekordbox"; letter: string; id: number };

export const THIS_CRATE: SplitSource = { kind: "crate" };

/** Stable identity of a source, for the picker and for storage. */
export function sourceKey(s: SplitSource): string {
  switch (s.kind) {
    case "crate":
      return "crate";
    case "library":
      return `lib:${s.libraryId}`;
    case "folder":
      return `folder:${s.libraryId}:${s.path}`;
    case "serato":
      return `serato:${s.letter}:${s.file}`;
    case "rekordbox":
      return `rbx:${s.letter}:${s.id}`;
  }
}

export interface SourceOption {
  key: string;
  label: string;
  group: string;
  source: SplitSource;
}

/** Every place the picker offers, grouped: the open crate first, then each
 *  library with its folder crates, then Serato crates and Rekordbox
 *  playlists per drive. Empty crates stay (filling one is the point of a
 *  merge); empty Rekordbox playlists and Rekordbox folders are left out. */
export function sourceOptions(
  libraries: { id: number; name: string }[],
  crates: Map<number, { path: string; track_count: number }[]>,
  serato: Map<string, { name: string; file: string; track_ids: number[] }[]>,
  rekordbox: Map<string, { id: number; name: string; is_folder: boolean; track_ids: number[] }[]>,
  // the running order IS the open crate, so it has no "This crate" entry
  withThisCrate = true,
): SourceOption[] {
  const out: SourceOption[] = withThisCrate
    ? [{ key: "crate", label: "This crate", group: "", source: THIS_CRATE }]
    : [];
  for (const lib of libraries) {
    const group = lib.name;
    const whole: SplitSource = { kind: "library", libraryId: lib.id };
    out.push({ key: sourceKey(whole), label: `${lib.name} (whole library)`, group, source: whole });
    for (const c of crates.get(lib.id) ?? []) {
      const s: SplitSource = { kind: "folder", libraryId: lib.id, path: c.path };
      out.push({ key: sourceKey(s), label: c.path.split("/").join(" › "), group, source: s });
    }
  }
  for (const [letter, list] of [...serato.entries()].sort(([a], [b]) => a.localeCompare(b))) {
    for (const c of list) {
      const s: SplitSource = { kind: "serato", letter, file: c.file };
      out.push({ key: sourceKey(s), label: c.name, group: `Serato crates (${letter})`, source: s });
    }
  }
  for (const [letter, list] of [...rekordbox.entries()].sort(([a], [b]) => a.localeCompare(b))) {
    for (const p of list) {
      if (p.is_folder || p.track_ids.length === 0) continue;
      const s: SplitSource = { kind: "rekordbox", letter, id: p.id };
      out.push({ key: sourceKey(s), label: p.name, group: `Rekordbox (${letter})`, source: s });
    }
  }
  return out;
}

/** A folder crate or Serato crate: a pane showing one keeps its own order
 *  and takes drops; libraries and Rekordbox playlists are read-only. */
export function isEditableSource(s: SplitSource): boolean {
  return s.kind === "folder" || s.kind === "serato";
}

/** The ids a drag from the overview carries: the overview's own selection
 *  when the grabbed row is part of it, else just that row — minus tracks
 *  already in the crate, which cannot be added twice. */
export function dragIds(grabbed: number, selected: Set<number>, inCrate: Set<number>): number[] {
  const ids = selected.has(grabbed) ? [...selected] : [grabbed];
  return ids.filter((id) => !inCrate.has(id));
}

export const loadSplitSource = () => read<unknown>("spindex.split.source", "crate");
export const saveSplitSource = (key: string) => write("spindex.split.source", key);
