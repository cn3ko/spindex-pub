// Track-table column definitions and their localStorage-persisted state.
// Split out of App.tsx so both App.tsx and TrackTable.tsx can share the
// same ColumnDef/SortState shapes without one importing from the other.

export interface ColumnDef {
  key: string;
  label: string;
  width: string;
  /** backend sort key; absent = not sortable */
  sortKey?: string;
  /** multiple independently-sortable fields in one header (e.g. Track / Artist);
   *  when present, takes precedence over sortKey for header sort controls */
  sorts?: { label: string; key: string }[];
  /** cannot be hidden */
  always?: boolean;
}

export const ALL_COLUMNS: ColumnDef[] = [
  { key: "num", label: "#", width: "44px", sortKey: "num" },
  { key: "cover", label: "Cover", width: "52px" },
  {
    key: "title",
    label: "Track / Artist",
    width: "minmax(220px, 2fr)",
    sorts: [
      { label: "Track", key: "title" },
      { label: "Artist", key: "artist" },
    ],
    always: true,
  },
  { key: "album", label: "Album", width: "minmax(140px, 1.2fr)", sortKey: "album" },
  { key: "genre", label: "Genre", width: "minmax(90px, 0.8fr)", sortKey: "genre" },
  { key: "library", label: "Library", width: "minmax(90px, 0.9fr)" },
  { key: "bpm", label: "BPM", width: "64px", sortKey: "bpm" },
  { key: "key", label: "Key", width: "56px", sortKey: "key" },
  { key: "rating", label: "Rating", width: "90px", sortKey: "rating" },
  { key: "time", label: "Time", width: "56px", sortKey: "time" },
  { key: "format", label: "Type", width: "64px", sortKey: "format" },
  { key: "bitrate", label: "Bitrate", width: "70px", sortKey: "bitrate" },
  { key: "quality", label: "Quality", width: "70px", sortKey: "quality" },
  { key: "added", label: "Added", width: "140px", sortKey: "added" },
  { key: "updated", label: "Updated", width: "140px", sortKey: "updated" },
  { key: "status", label: "Status", width: "70px" },
];

export interface SortState {
  key: string;
  desc: boolean;
}

// Stored as the *hidden* set so columns added in later versions default to
// visible instead of silently missing from an old saved visible-list.
export function loadHiddenCols(): Set<string> {
  try {
    const raw = localStorage.getItem("spindex.hiddencols");
    if (raw) return new Set(JSON.parse(raw) as string[]);
  } catch {
    // fall through to default
  }
  return new Set();
}

export function loadColOrder(): string[] {
  const defaults = ALL_COLUMNS.map((c) => c.key);
  try {
    const raw = localStorage.getItem("spindex.colorder");
    if (raw) {
      const saved = (JSON.parse(raw) as string[]).filter((k) => defaults.includes(k));
      // columns added in later versions slot in at their default position
      const merged = [...saved];
      for (const k of defaults) {
        if (!merged.includes(k)) merged.splice(defaults.indexOf(k), 0, k);
      }
      return merged;
    }
  } catch {
    // fall through to default
  }
  return defaults;
}

export const COL_MIN = 40;

// Per-column pixel width overrides; a column absent from the map keeps its
// default (possibly flexible) width from ALL_COLUMNS.
export function loadColWidths(): Record<string, number> {
  try {
    const raw = localStorage.getItem("spindex.colwidths");
    if (raw) {
      const parsed = JSON.parse(raw) as Record<string, number>;
      const clean: Record<string, number> = {};
      for (const [k, v] of Object.entries(parsed)) {
        if (Number.isFinite(v) && v >= COL_MIN) clean[k] = v;
      }
      return clean;
    }
  } catch {
    // fall through to default
  }
  return {};
}

export function loadSort(): SortState | null {
  try {
    const raw = localStorage.getItem("spindex.sort");
    if (raw) return JSON.parse(raw) as SortState;
  } catch {
    // fall through to default
  }
  return null;
}
