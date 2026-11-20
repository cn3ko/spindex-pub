import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import * as api from "./api";
import { hideSplash } from "./splash";
import DeckPlayer from "./components/DeckPlayer";
import Sidebar from "./components/Sidebar";
import Toolbar from "./components/Toolbar";
import TrackTable from "./components/TrackTable";
import { sortKeys } from "./components/KeyFilterPopup";
import { OverviewPane, PaneHeader, SourcePicker, colsFor } from "./components/SplitPane";
import {
  centerOffset,
  loadSplitCols,
  loadSplitFrac,
  loadSplitOn,
  loadSplitSort,
  loadSplitSource,
  moveCol,
  saveSplitCols,
  saveSplitFrac,
  saveSplitOn,
  saveSplitSort,
  saveSplitSource,
  sourceKey,
  sourceOptions,
  isEditableSource,
  loadSplitFracs3,
  saveSplitFracs3,
  resizeThree,
  loadSplitMiddleOn,
  saveSplitMiddleOn,
  loadSplitMiddleSource,
  saveSplitMiddleSource,
  type Fracs3,
  THIS_CRATE,
  type SplitSource,
  SPLIT_MAX_FRAC,
  SPLIT_MIN_FRAC,
} from "./splitView";
import LibraryCompare from "./components/LibraryCompare";
import DuplicatesModal from "./components/DuplicatesModal";
import MetadataEditor from "./components/MetadataEditor";
import BatchMetadataEditor from "./components/BatchMetadataEditor";
import QualityPanel from "./components/QualityPanel";
import EnhanceDialog from "./components/EnhanceDialog";
import RekordboxExportDialog from "./components/RekordboxExportDialog";
import RekordboxImportDialog from "./components/RekordboxImportDialog";
import TempoDialog from "./components/TempoDialog";
import RestoreDialog from "./components/RestoreDialog";
import HoverTip from "./components/HoverTip";
import Visualizer from "./components/Visualizer";
import ModalBackdrop from "./components/ModalBackdrop";
import TrackContextMenu, { CtxItem } from "./components/TrackContextMenu";
import SeratoSyncModal from "./components/SeratoSyncModal";
import DeleteConfirm, { trackDisplayName, type DeleteItem } from "./components/DeleteConfirm";
import {
  ALL_COLUMNS,
  COL_MIN,
  loadColOrder,
  loadColWidths,
  loadHiddenCols,
  loadSort,
} from "./columns";
import type { ColumnDef, SortState } from "./columns";
import "./App.css";

const PAGE_SIZE = 500;
// the running order of a folder-crate; a constant so fetchTracks keeps its identity
const CRATE_ORDER_SORT: SortState = { key: "num", desc: false };

const SIDEBAR_MIN = 170;
const SIDEBAR_MAX = 420;
const SIDEBAR_DEFAULT = 232;

function loadSidebarWidth(): number {
  const raw = Number(localStorage.getItem("spindex.sidewidth"));
  if (Number.isFinite(raw) && raw >= SIDEBAR_MIN && raw <= SIDEBAR_MAX) return raw;
  return SIDEBAR_DEFAULT;
}

function loadOpenCrates(): Set<number> {
  try {
    const raw = localStorage.getItem("spindex.crates.open");
    if (raw) return new Set(JSON.parse(raw) as number[]);
  } catch {
    // fall through to default
  }
  return new Set();
}

function loadCollapsedCrates(): Set<string> {
  try {
    const raw = localStorage.getItem("spindex.crates.collapsed");
    if (raw) return new Set(JSON.parse(raw) as string[]);
  } catch {
    // fall through to default
  }
  return new Set();
}

function loadHealthCollapsed(): boolean {
  try {
    return localStorage.getItem("spindex.health.collapsed") === "1";
  } catch {
    return false;
  }
}

function useDebounced<T>(value: T, ms: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return debounced;
}

export default function App() {
  const [libraries, setLibraries] = useState<api.LibraryInfo[]>([]);
  const [activeLibrary, setActiveLibrary] = useState<number | null>(null);
  const [query, setQuery] = useState("");
  const [tracks, setTracks] = useState<api.TrackRow[]>([]);
  const [total, setTotal] = useState(0);
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set());
  const [anchorIdx, setAnchorIdx] = useState<number | null>(null);
  const [scanning, setScanning] = useState<api.ScanProgress | null>(null);
  const [toast, setToast] = useState<{ text: string; error?: boolean } | null>(null);
  const [playing, setPlaying] = useState<api.TrackRow | null>(null);
  const [playToken, setPlayToken] = useState(0);
  const [shuffle, setShuffle] = useState<boolean>(
    () => localStorage.getItem("spindex.shuffle") === "1",
  );
  const [repeat, setRepeat] = useState<"off" | "all" | "one">(() => {
    const raw = localStorage.getItem("spindex.repeat");
    return raw === "all" || raw === "one" ? raw : "off";
  });
  // Tracks that actually played, for Prev under shuffle. Not persisted.
  const historyRef = useRef<number[]>([]);
  const [waveToken, setWaveToken] = useState(0);
  const [artToken, setArtToken] = useState(0);
  const [analyzerReady, setAnalyzerReady] = useState(false);
  const [analysis, setAnalysis] = useState<api.AnalysisProgress | null>(null);
  const [analyzeMenu, setAnalyzeMenu] = useState(false);
  const [vizOpen, setVizOpen] = useState(false);
  const [copyMenu, setCopyMenu] = useState(false);
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number } | null>(null);
  const [copying, setCopying] = useState<api.CopyProgress | null>(null);
  const copyTargetRef = useRef<number | null>(null);
  // "Copy crate to…" menu: which crate is asking, and where the menu opens
  const [crateCopyMenu, setCrateCopyMenu] = useState<
    { libraryId: number; path: string; x: number; y: number } | null
  >(null);
  const [addMenu, setAddMenu] = useState(false);
  // tracks currently being dragged toward a sidebar library
  const dragTracksRef = useRef<number[] | null>(null);
  // set while the drag comes from the split view's overview (its rows by id):
  // the running order then adds the tracks instead of reordering them
  const foreignDragRef = useRef<Map<number, api.TrackRow> | null>(null);
  const [dropLib, setDropLib] = useState<number | null>(null);
  // Row index the dragged selection would land before; tracks.length = at the end.
  const [dropRow, setDropRow] = useState<number | null>(null);
  const [newLibParent, setNewLibParent] = useState<string | null>(null);
  const [newLibName, setNewLibName] = useState("");
  const [editing, setEditing] = useState<api.TrackRow | null>(null);
  const [qualityTrack, setQualityTrack] = useState<api.TrackRow | null>(null);
  const [batchEditing, setBatchEditing] = useState<api.TrackRow[] | null>(null);
  const [batch, setBatch] = useState<api.BatchProgress | null>(null);
  // ids of the tracks the enhance dialog is open for (null = closed)
  const [enhanceDialog, setEnhanceDialog] = useState<number[] | null>(null);
  const [rbxExport, setRbxExport] = useState<{
    letter: string;
    label: string;
    crate?: { file: string; name: string };
  } | null>(null);
  const [syncing, setSyncing] = useState<api.SyncProgress | null>(null);
  // Rekordbox → Serato import: the dialog (stick letter + label) and its pill
  const [rbxImport, setRbxImport] = useState<{ letter: string; label: string } | null>(null);
  const [rbImporting, setRbImporting] = useState<api.RekordboxImportProgress | null>(null);
  const [enhancing, setEnhancing] = useState<api.EnhanceProgress | null>(null);
  // ids of the tracks the tempo dialog is open for (null = closed)
  const [tempoDialog, setTempoDialog] = useState<number[] | null>(null);
  const [stabilizing, setStabilizing] = useState<api.EnhanceProgress | null>(null);
  const [apolloOk, setApolloOk] = useState(false);
  // ids of the tracks the ML-restore dialog is open for (null = closed)
  const [mlRestoreDialog, setMlRestoreDialog] = useState<number[] | null>(null);
  const [mlRestoreRun, setMlRestoreRun] = useState<api.MlRestoreProgress | null>(null);
  const [comparing, setComparing] = useState<{ set: api.TrackSetRef; name: string } | null>(null);
  const [devices, setDevices] = useState<api.DeviceInfo[]>([]);
  const [devicesLoaded, setDevicesLoaded] = useState(false);
  const [health, setHealth] = useState<api.HealthCounts | null>(null);
  const [healthFilter, setHealthFilter] = useState<api.HealthFilter | null>(null);
  // Camelot codes picked in the key wheel; empty = every key.
  const [keyFilter, setKeyFilter] = useState<Set<string>>(() => new Set());
  const keyList = useMemo(() => sortKeys(keyFilter), [keyFilter]);
  const keyView = keyList.join(",");
  const [healthCollapsed, setHealthCollapsed] = useState<boolean>(loadHealthCollapsed);
  // folder-crates per library, derived server-side from rel_path
  const [crates, setCrates] = useState<Map<number, api.CrateFolder[]>>(new Map());
  // library ids whose crate tree is expanded in the sidebar
  const [openCrates, setOpenCrates] = useState<Set<number>>(loadOpenCrates);
  // collapsed crate folders within a tree, keyed `${libId}:${path}`
  const [collapsedCrates, setCollapsedCrates] = useState<Set<string>>(loadCollapsedCrates);
  // active crate: folder path within activeLibrary, null = whole library
  const [activeFolder, setActiveFolder] = useState<string | null>(null);
  // create/rename-crate modal; parentPath/path are crate rel paths
  const [crateModal, setCrateModal] = useState<
    { libraryId: number; mode: "create"; parentPath: string } |
    { libraryId: number; mode: "rename"; path: string } |
    null
  >(null);
  const [crateName, setCrateName] = useState("");
  // crate row currently hovered by a track drag, keyed `${libId}:${path}`
  const [dropCrate, setDropCrate] = useState<string | null>(null);
  // read-only Serato subcrates per connected drive letter
  const [seratoCrates, setSeratoCrates] = useState<Map<string, api.SeratoCrateInfo[]>>(
    new Map(),
  );
  // active Serato crate filter (explicit id set spanning the stick's libraries)
  const [seratoFilter, setSeratoFilter] = useState<
    { letter: string; file: string; name: string; ids: number[] } | null
  >(null);
  // bump to reload Serato crates after an edit
  const [seratoToken, setSeratoToken] = useState(0);
  // create/rename modal for Serato crates (separate from folder-crate modal)
  const [seratoModal, setSeratoModal] = useState<
    { letter: string; mode: "create" } | { letter: string; mode: "rename"; file: string } | null
  >(null);
  // reconcile modal: SpinDex-vs-Serato sync plan for one crate
  const [seratoSync, setSeratoSync] = useState<
    { letter: string; file: string; name: string; plan: api.SeratoSyncPlan } | null
  >(null);
  // `${letter}:${file}` of the Serato crate whose sync preview is loading,
  // so its Sync button icon can spin while the async check runs.
  const [syncingSerato, setSyncingSerato] = useState<string | null>(null);
  // read-only Rekordbox playlists/folders per connected drive letter,
  // mirroring seratoCrates — this is a display-only slice (SP1), no
  // editing, no intent tracking, no sync.
  const [rekordboxCrates, setRekordboxCrates] = useState<
    Map<string, api.RekordboxCrateInfo[]>
  >(new Map());
  // playlists of the XML SpinDex exported to each stick (SpinDex/rekordbox.xml),
  // shown as "Prepared for Rekordbox" until Rekordbox imports them. Their ids
  // are NEGATED on load so they share renderRekordboxNodes and rekordboxFilter
  // with the real export.pdb playlists without ever colliding with them.
  const [preparedRekordbox, setPreparedRekordbox] = useState<
    Map<string, api.RekordboxCrateInfo[]>
  >(new Map());
  // bumped after a successful export so the prepared list re-reads the file
  const [preparedToken, setPreparedToken] = useState(0);
  // active Rekordbox playlist filter (explicit id set, same mechanism as
  // seratoFilter — `id` is the playlist's numeric Rekordbox id, not a file).
  const [rekordboxFilter, setRekordboxFilter] = useState<
    { letter: string; id: number; name: string; ids: number[] } | null
  >(null);
  const [dupesFor, setDupesFor] = useState<{
    set: api.TrackSetRef;
    name: string;
    readOnly?: boolean;
    seratoCrate?: { letter: string; file: string };
  } | null>(null);
  // The open delete confirm, resolved with the user's answer.
  const [deleteAsk, setDeleteAsk] = useState<{
    items: DeleteItem[];
    resolve: (ok: boolean) => void;
  } | null>(null);
  const askDelete = useCallback(
    (items: DeleteItem[]) =>
      new Promise<boolean>((resolve) => setDeleteAsk({ items, resolve })),
    [],
  );
  const answerDelete = useCallback(
    (ok: boolean) => {
      setDeleteAsk((cur) => {
        cur?.resolve(ok);
        return null;
      });
    },
    [],
  );
  const [hiddenCols, setHiddenCols] = useState<Set<string>>(loadHiddenCols);
  const [colOrder, setColOrder] = useState<string[]>(loadColOrder);
  const [sort, setSort] = useState<SortState | null>(loadSort);
  const [colWidths, setColWidths] = useState<Record<string, number>>(loadColWidths);
  const [sideWidth, setSideWidth] = useState<number>(loadSidebarWidth);
  const [colMenu, setColMenu] = useState(false);
  // Split view (crates only): overview on the left, running order on the right.
  const [splitOn, setSplitOn] = useState<boolean>(loadSplitOn);
  const [splitLeftCols, setSplitLeftCols] = useState<string[]>(() => loadSplitCols("left"));
  const [splitRightCols, setSplitRightCols] = useState<string[]>(() => loadSplitCols("right"));
  const [splitSort, setSplitSort] = useState<SortState | null>(loadSplitSort);
  const [splitFrac, setSplitFrac] = useState<number>(loadSplitFrac);
  // the optional middle pane, for a second crate beside the running order
  const [middleOn, setMiddleOn] = useState<boolean>(loadSplitMiddleOn);
  const [middleCols, setMiddleCols] = useState<string[]>(() => loadSplitCols("middle"));
  const [middleSort, setMiddleSort] = useState<SortState | null>(() => loadSplitSort("middle"));
  const [middleSourceKey, setMiddleSourceKey] = useState<string>(() => {
    const v = loadSplitMiddleSource();
    return typeof v === "string" ? v : "";
  });
  const [fracs3, setFracs3] = useState<Fracs3>(loadSplitFracs3);
  // the pane a track drag started in (its source key; null = the running order)
  const dragFromRef = useRef<string | null>(null);
  // bumped after any pane edits a crate, so every pane reloads
  const [paneToken, setPaneToken] = useState(0);
  const splitRef = useRef<HTMLDivElement>(null);
  const dragCol = useRef<string | null>(null);
  const [dragOver, setDragOver] = useState<string | null>(null);
  // OS files hovering over the track table (imports into the active library/crate)
  const [fileDropTable, setFileDropTable] = useState(false);
  const debouncedQuery = useDebounced(query, 150);
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    localStorage.setItem("spindex.hiddencols", JSON.stringify(Array.from(hiddenCols)));
  }, [hiddenCols]);
  useEffect(() => {
    localStorage.setItem("spindex.colorder", JSON.stringify(colOrder));
  }, [colOrder]);
  useEffect(() => saveSplitOn(splitOn), [splitOn]);
  useEffect(() => saveSplitCols("left", splitLeftCols), [splitLeftCols]);
  useEffect(() => saveSplitCols("right", splitRightCols), [splitRightCols]);
  useEffect(() => saveSplitSort(splitSort), [splitSort]);
  useEffect(() => saveSplitMiddleOn(middleOn), [middleOn]);
  useEffect(() => saveSplitCols("middle", middleCols), [middleCols]);
  useEffect(() => saveSplitSort(middleSort, "middle"), [middleSort]);
  useEffect(() => saveSplitMiddleSource(middleSourceKey), [middleSourceKey]);
  // what the overview shows, by key; kept across crate switches and restarts
  const [splitSourceKey, setSplitSourceKey] = useState<string>(() => {
    const v = loadSplitSource();
    return typeof v === "string" ? v : "crate";
  });
  useEffect(() => saveSplitSource(splitSourceKey), [splitSourceKey]);
  useEffect(() => {
    if (sort) localStorage.setItem("spindex.sort", JSON.stringify(sort));
    else localStorage.removeItem("spindex.sort");
  }, [sort]);
  useEffect(() => {
    localStorage.setItem("spindex.shuffle", shuffle ? "1" : "0");
  }, [shuffle]);
  useEffect(() => {
    localStorage.setItem("spindex.repeat", repeat);
  }, [repeat]);
  useEffect(() => {
    localStorage.setItem("spindex.crates.open", JSON.stringify(Array.from(openCrates)));
  }, [openCrates]);
  useEffect(() => {
    localStorage.setItem(
      "spindex.crates.collapsed",
      JSON.stringify(Array.from(collapsedCrates)),
    );
  }, [collapsedCrates]);
  useEffect(() => {
    localStorage.setItem("spindex.health.collapsed", healthCollapsed ? "1" : "0");
  }, [healthCollapsed]);

  // A new view context invalidates the selection — otherwise Delete could act
  // on tracks that are no longer visible (and the USB-permanence check in
  // deleteSelected can only see the currently loaded page).
  // The Serato context is WHICH crate, not its current contents: a reorder
  // rewrites `ids` (same tracks, new order) and must keep the selection, or
  // nudging a block of tracks would need a re-select per press. The edits that
  // do change membership (add / remove) clear the selection themselves.
  const seratoView = seratoFilter && `${seratoFilter.letter}:${seratoFilter.file}`;
  // Rekordbox has no edit path (SP1, read-only), so unlike seratoView there
  // is no reorder that needs the selection preserved — the playlist's ids
  // never change under the user here.
  const rekordboxView = rekordboxFilter && `${rekordboxFilter.letter}:${rekordboxFilter.id}`;
  useEffect(() => {
    setSelectedIds(new Set());
    setAnchorIdx(null);
  }, [debouncedQuery, activeLibrary, activeFolder, seratoView, rekordboxView, healthFilter, keyView]);

  // In "All tracks" with several libraries the Library column is offered;
  // in a single-library view it disappears regardless of the setting.
  const showLibraryColumn = activeLibrary === null && libraries.length > 1;
  // True when the library currently shown is the one being scanned right now.
  const activeScanning =
    scanning != null &&
    (activeLibrary === null || scanning.library_id === activeLibrary);
  const columns = colOrder
    .map((k) => ALL_COLUMNS.find((c) => c.key === k))
    .filter((c): c is ColumnDef => c !== undefined)
    .filter(
      (c) =>
        (c.always || !hiddenCols.has(c.key)) &&
        (c.key !== "library" || showLibraryColumn),
    );
  // A folder-crate is showing, so each row has a meaningful fixed number.
  // Serato crates are excluded: they carry their own order inside the .crate.
  const crateSelected =
    activeLibrary !== null && activeFolder !== null && seratoFilter === null;
  // Reordering additionally requires the full crate to be on screen in its
  // manual order — a search, a health filter or another sort hides or reorders
  // rows, and an insertion index computed against a partial list would be wrong.
  // A "num" sort only means anything inside a folder-crate: outside one it
  // orders the whole library by stale per-crate positions, interleaving every
  // crate's position-1 tracks. The # header is deliberately not sortable there,
  // so the user could never click it back off — and `sort` is persisted, so a
  // restart would resurrect it. Neutralise it at the point of use instead of
  // trusting every setter: everything downstream (the request, the header
  // indicator, manual mode) reads effectiveSort, never sort.
  // The split view works in any view; when its right pane shows a crate that
  // can be hand-ordered, it is always that crate's running order, whatever
  // `sort` says. A library or playlist there sorts like the plain table.
  const splitActive = splitOn;
  const splitCrateView = splitOn && (crateSelected || seratoFilter !== null);
  const effectiveSort = splitCrateView
    ? crateSelected
      ? CRATE_ORDER_SORT
      : null
    : sort && sort.key === "num" && !crateSelected
      ? null
      : sort;
  const manualMode =
    crateSelected &&
    effectiveSort?.key === "num" &&
    !effectiveSort.desc &&
    debouncedQuery === "" &&
    healthFilter === null &&
    keyList.length === 0;
  // A Serato crate orders itself: after list_tracks learned to honour an id
  // set's own sequence, "no explicit sort" IS the crate's order. There is no
  // crate_pos here and no "num" sort key — the .crate file is the authority.
  const seratoManualMode =
    seratoFilter !== null &&
    effectiveSort === null &&
    debouncedQuery === "" &&
    healthFilter === null &&
    keyList.length === 0;
  // Either kind of crate can be hand-ordered; the gestures are identical and
  // only the command they call differs.
  const anyManualMode = manualMode || seratoManualMode;
  // In the split view the right pane shows "#" plus its own chosen columns.
  const tableColumns = splitActive
    ? [ALL_COLUMNS.find((c) => c.key === "num")!, ...colsFor(splitRightCols)]
    : columns;
  const gridTemplate = tableColumns
    .map((c) =>
      colWidths[c.key] != null
        ? `${colWidths[c.key]}px`
        : c.key === "num" && anyManualMode
          ? "64px"
          : c.width,
    )
    .join(" ");

  // Column resize: drag the handle on a header cell's right edge. A resized
  // column becomes a fixed pixel width; double-click the handle resets it to
  // its default (possibly flexible) width.
  const startColResize = useCallback((e: React.MouseEvent, key: string) => {
    // preventDefault alone suppresses the draggable ancestor's native drag;
    // no stopPropagation, so document-level listeners (tooltip hide) still fire.
    e.preventDefault();
    const cell = (e.currentTarget as HTMLElement).parentElement;
    if (!cell) return;
    const startX = e.clientX;
    const startWidth = cell.getBoundingClientRect().width;
    const move = (ev: MouseEvent) => {
      const w = Math.max(COL_MIN, Math.round(startWidth + ev.clientX - startX));
      setColWidths((prev) => ({ ...prev, [key]: w }));
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      // persist once per drag, like the sidebar resize
      setColWidths((w) => {
        localStorage.setItem("spindex.colwidths", JSON.stringify(w));
        return w;
      });
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  }, []);

  // Drop the dragged column at the target column's position.
  const moveColumn = useCallback((from: string | null, to: string) => {
    if (!from || from === to) return;
    setColOrder((prev) => {
      const next = prev.filter((k) => k !== from);
      const at = next.indexOf(to);
      if (at < 0) return prev;
      next.splice(at, 0, from);
      return next;
    });
  }, []);

  // Sidebar resize: drag the handle on its right edge; width persists.
  const startSidebarResize = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    const move = (ev: MouseEvent) => {
      setSideWidth(Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, ev.clientX)));
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      setSideWidth((w) => {
        localStorage.setItem("spindex.sidewidth", String(w));
        return w;
      });
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  }, []);

  // header click cycles: ascending -> descending -> default order
  const toggleSort = useCallback((sortKey: string) => {
    setSort((cur) => {
      if (cur?.key !== sortKey) return { key: sortKey, desc: false };
      if (!cur.desc) return { key: sortKey, desc: true };
      return null;
    });
  }, []);

  const showToast = useCallback((text: string, error = false) => {
    setToast({ text, error });
    setTimeout(() => setToast(null), 4500);
  }, []);

  const refreshLibraries = useCallback(async () => {
    const libs = await api.listLibraries();
    setLibraries(libs);
    // crate trees follow the library list (counts change on every rescan)
    const pairs = await Promise.all(
      libs.map(async (l) => [l.id, await api.listCrates(l.id)] as const),
    );
    setCrates(new Map(pairs));
  }, []);

  // Set right before a Serato reorder bumps `seratoToken`: the crate's fresh
  // id list arrives asynchronously and changes `seratoFilter`, which re-runs
  // the automatic refresh below — that one must keep the scroll position too,
  // and only the code that asked for the reorder knows it. Consumed (and
  // cleared) by the first refresh that follows.
  const keepScrollRef = useRef(false);

  // Listings run in the background, so an older one can finish after a newer
  // one (fast typing, a reorder during a scan): every refresh takes a number
  // and only the latest may write the table. A page load checks it too.
  const listSeqRef = useRef(0);
  const loadingMoreRef = useRef(false);
  // How many rows are loaded, read by a keep-scroll refresh so a reorder at
  // row 900 reloads down to row 900 instead of collapsing to the first page.
  const loadedCountRef = useRef(0);
  loadedCountRef.current = tracks.length;

  const fetchTracks = useCallback(
    (offset: number, limit: number) =>
      api.listTracks(
        debouncedQuery,
        activeLibrary,
        effectiveSort?.key ?? null,
        effectiveSort?.desc ?? false,
        healthFilter,
        activeFolder,
        seratoFilter?.ids ?? rekordboxFilter?.ids ?? null,
        limit,
        offset,
        keyList,
      ),
    [
      debouncedQuery,
      activeLibrary,
      effectiveSort,
      healthFilter,
      keyList,
      activeFolder,
      seratoFilter,
      rekordboxFilter,
    ],
  );

  // The split overview lists the open crate, or another source, in its own
  // order with its own search.
  const fetchSource = useCallback(
    (src: SplitSource, q: string, order: SortState | null, offset: number, limit: number) => {
      const list = (lib: number | null, folder: string | null, ids: number[] | null) =>
        api.listTracks(q, lib, order?.key ?? null, order?.desc ?? false, null, folder, ids, limit, offset);
      switch (src.kind) {
        case "crate":
          return list(activeLibrary, activeFolder, seratoFilter?.ids ?? rekordboxFilter?.ids ?? null);
        case "library":
          return list(src.libraryId, null, null);
        case "folder":
          return list(src.libraryId, src.path, null);
        case "serato": {
          const c = seratoCrates.get(src.letter)?.find((x) => x.file === src.file);
          return list(null, null, c?.track_ids ?? []);
        }
        case "rekordbox": {
          const p = rekordboxCrates.get(src.letter)?.find((x) => x.id === src.id);
          return list(null, null, p?.track_ids ?? []);
        }
      }
    },
    [activeLibrary, activeFolder, seratoFilter, rekordboxFilter, seratoCrates, rekordboxCrates],
  );

  const splitOptions = useMemo(
    () => sourceOptions(libraries, crates, seratoCrates, rekordboxCrates),
    [libraries, crates, seratoCrates, rekordboxCrates],
  );
  // the running order's menu: the same places, minus "This crate"
  const splitTargets = useMemo(
    () => sourceOptions(libraries, crates, seratoCrates, rekordboxCrates, false),
    [libraries, crates, seratoCrates, rekordboxCrates],
  );
  // what the running order shows now, as a menu key ("" = All tracks)
  const splitTargetKey = seratoFilter
    ? sourceKey({ kind: "serato", letter: seratoFilter.letter, file: seratoFilter.file })
    : rekordboxFilter
      ? sourceKey({ kind: "rekordbox", letter: rekordboxFilter.letter, id: rekordboxFilter.id })
      : activeLibrary !== null && activeFolder !== null
        ? sourceKey({ kind: "folder", libraryId: activeLibrary, path: activeFolder })
        : activeLibrary !== null
          ? sourceKey({ kind: "library", libraryId: activeLibrary })
          : "";
  // opens a place in the running order, exactly like clicking it in the sidebar
  const openSplitTarget = (s: SplitSource) => {
    if (s.kind === "library") {
      setActiveLibrary(s.libraryId);
      setActiveFolder(null);
      setSeratoFilter(null);
      setRekordboxFilter(null);
    } else if (s.kind === "rekordbox") {
      const p = rekordboxCrates.get(s.letter)?.find((x) => x.id === s.id);
      if (!p) return;
      setActiveLibrary(null);
      setActiveFolder(null);
      setSeratoFilter(null);
      setSort(null);
      setRekordboxFilter({ letter: s.letter, id: p.id, name: p.name, ids: p.track_ids });
    } else if (s.kind === "folder") {
      setActiveLibrary(s.libraryId);
      setActiveFolder(s.path);
      setSeratoFilter(null);
      setRekordboxFilter(null);
      setSort({ key: "num", desc: false });
    } else if (s.kind === "serato") {
      const c = seratoCrates.get(s.letter)?.find((x) => x.file === s.file);
      if (!c) return;
      setActiveLibrary(null);
      setActiveFolder(null);
      setSort(null);
      setSeratoFilter({ letter: s.letter, file: c.file, name: c.name, ids: c.track_ids });
      setRekordboxFilter(null);
    }
  };
  // a stored source that no longer exists (crate deleted, stick unplugged)
  // falls back to the open crate
  const splitSource: SplitSource =
    splitOptions.find((o) => o.key === splitSourceKey)?.source ?? THIS_CRATE;
  // the middle pane's place; a vanished one falls back to the first crate
  const middleSource: SplitSource =
    splitOptions.find((o) => o.key === middleSourceKey)?.source ??
    splitOptions.find((o) => isEditableSource(o.source))?.source ??
    THIS_CRATE;
  // opening the middle pane suggests a crate the other panes do not show
  const openMiddle = () => {
    const taken = new Set([splitTargetKey, sourceKey(splitSource)]);
    const pick =
      splitOptions.find((o) => o.key === middleSourceKey && !taken.has(o.key)) ??
      splitOptions.find((o) => isEditableSource(o.source) && !taken.has(o.key));
    if (pick) setMiddleSourceKey(pick.key);
    setMiddleOn(true);
  };
  // every pane reloads when the running order changes or a pane edits a crate
  const paneRefresh = useMemo(() => ({}), [tracks, paneToken]);
  // tracks the running order already holds (Serato: the whole crate; a
  // folder crate: the rows loaded so far)
  // (nothing when the right side is a library or playlist, not a crate)
  const inCrate = useMemo(
    () =>
      new Set(
        !splitCrateView ? [] : seratoFilter ? seratoFilter.ids : tracks.map((t) => t.id),
      ),
    [splitCrateView, seratoFilter, tracks],
  );

  // A click in the overview selects the track and brings it to the middle of
  // the running order, where it can be moved.
  const pickFromOverview = useCallback(
    (t: api.TrackRow) => {
      setSelectedIds(new Set([t.id]));
      const idx = tracks.findIndex((r) => r.id === t.id);
      setAnchorIdx(idx >= 0 ? idx : null);
      const el = scrollRef.current;
      if (idx < 0 || !el) return;
      const head = (el.querySelector(".thead") as HTMLElement | null)?.offsetHeight ?? 0;
      el.scrollTo({ top: centerOffset(idx, 42, head, el.clientHeight), behavior: "smooth" });
    },
    [tracks],
  );

  // Drag the divider between the two panes; the share persists.
  const startSplitResize = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    const box = splitRef.current?.getBoundingClientRect();
    if (!box || box.width <= 0) return;
    const move = (ev: MouseEvent) => {
      const f = (ev.clientX - box.left) / box.width;
      setSplitFrac(Math.min(SPLIT_MAX_FRAC, Math.max(SPLIT_MIN_FRAC, f)));
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      setSplitFrac((f) => {
        saveSplitFrac(f);
        return f;
      });
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  }, []);

  // With three panes each divider moves only the two panes beside it.
  const startSplitResize3 = useCallback((divider: 0 | 1) => (e: React.MouseEvent) => {
    e.preventDefault();
    const box = splitRef.current?.getBoundingClientRect();
    if (!box || box.width <= 0) return;
    const move = (ev: MouseEvent) => {
      const at = (ev.clientX - box.left) / box.width;
      setFracs3((f) => resizeThree(f, divider, at));
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      setFracs3((f) => {
        saveSplitFracs3(f);
        return f;
      });
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  }, []);

  // `keepScroll` is for a reorder: nudging a track at row 200 must not throw
  // the view back to row 0. Every other caller keeps the scroll-to-top reset.
  const refreshTracks = useCallback(
    async (opts?: { keepScroll?: boolean }) => {
      const seq = ++listSeqRef.current;
      const want = opts?.keepScroll ? Math.max(PAGE_SIZE, loadedCountRef.current) : PAGE_SIZE;
      let page = await fetchTracks(0, PAGE_SIZE);
      const rows = [...page.rows];
      while (rows.length < want && rows.length < page.total && page.rows.length > 0) {
        if (seq !== listSeqRef.current) return;
        page = await fetchTracks(rows.length, PAGE_SIZE);
        rows.push(...page.rows);
      }
      if (seq !== listSeqRef.current) return;
      setTracks(rows);
      setTotal(page.total);
      if (!opts?.keepScroll) scrollRef.current?.scrollTo({ top: 0 });
    },
    [fetchTracks],
  );

  // The table asks for the next page when its last rows come into view.
  const loadMoreTracks = useCallback(async () => {
    if (loadingMoreRef.current) return;
    const have = loadedCountRef.current;
    if (have === 0 || have >= total) return;
    loadingMoreRef.current = true;
    const seq = listSeqRef.current;
    try {
      const page = await fetchTracks(have, PAGE_SIZE);
      // a refresh in the meantime replaced the list; its rows win
      if (seq !== listSeqRef.current) return;
      setTracks((cur) => (cur.length === have ? [...cur, ...page.rows] : cur));
      setTotal(page.total);
    } catch {
      // the next scroll asks again
    } finally {
      loadingMoreRef.current = false;
    }
  }, [fetchTracks, total]);

  // Serato subcrates follow the connected devices, the index state, and any
  // Serato edit (seratoToken). An active crate filter tracks its fresh ids.
  useEffect(() => {
    let stale = false;
    (async () => {
      const pairs = await Promise.all(
        devices.map(async (d) => {
          try {
            return [d.letter, await api.listSeratoCrates(d.letter)] as const;
          } catch {
            return [d.letter, [] as api.SeratoCrateInfo[]] as const;
          }
        }),
      );
      if (stale) return;
      const map = new Map(pairs);
      setSeratoCrates(map);
      setSeratoFilter((cur) => {
        if (!cur) return null;
        const fresh = map.get(cur.letter)?.find((c) => c.file === cur.file);
        if (!fresh) return null;
        // Identity matters: this effect also runs on every device/library
        // refresh, and a new object would clear the selection and scroll the
        // table back to row 0 for no reason at all. Keep the same object when
        // nothing actually changed.
        const same =
          fresh.name === cur.name &&
          fresh.track_ids.length === cur.ids.length &&
          fresh.track_ids.every((id, i) => id === cur.ids[i]);
        return same ? cur : { ...cur, name: fresh.name, ids: fresh.track_ids };
      });
    })();
    return () => {
      stale = true;
    };
  }, [devices, libraries, seratoToken]);

  // Rekordbox playlists follow the connected devices and the index state,
  // same as Serato subcrates above — but read-only, so there is no edit
  // token to depend on and no identity-preservation dance: a playlist's
  // ids never change out from under the user in this SP1 slice.
  useEffect(() => {
    let stale = false;
    (async () => {
      const pairs = await Promise.all(
        devices.map(async (d) => {
          try {
            return [d.letter, await api.listRekordboxCrates(d.letter)] as const;
          } catch {
            return [d.letter, [] as api.RekordboxCrateInfo[]] as const;
          }
        }),
      );
      const preparedPairs = await Promise.all(
        devices.map(async (d) => {
          try {
            const list = await api.listPreparedRekordbox(d.letter);
            // negate so a prepared id can never equal a real Rekordbox id;
            // parent 0 (top level) stays 0
            const negated = list.map((c) => ({ ...c, id: -c.id, parent_id: -c.parent_id }));
            return [d.letter, negated] as const;
          } catch {
            return [d.letter, [] as api.RekordboxCrateInfo[]] as const;
          }
        }),
      );
      if (stale) return;
      const map = new Map(pairs);
      const prepared = new Map(preparedPairs);
      setRekordboxCrates(map);
      setPreparedRekordbox(prepared);
      setRekordboxFilter((cur) => {
        if (!cur) return null;
        const fresh = (cur.id < 0 ? prepared : map)
          .get(cur.letter)
          ?.find((c) => c.id === cur.id);
        if (!fresh) return null;
        // Identity matters here exactly like the Serato effect above: this
        // effect also runs on every unrelated device/library refresh (e.g.
        // a scan finishing somewhere else in the app), and a new object
        // would churn `refreshTracks`'s identity, re-firing the fetch below
        // and resetting the scroll position for a view the user never
        // touched. Keep the same object when nothing actually changed.
        const same =
          fresh.name === cur.name &&
          fresh.track_ids.length === cur.ids.length &&
          fresh.track_ids.every((id, i) => id === cur.ids[i]);
        return same ? cur : { ...cur, name: fresh.name, ids: fresh.track_ids };
      });
    })();
    return () => {
      stale = true;
    };
  }, [devices, libraries, preparedToken]);

  const refreshDevices = useCallback(async () => {
    try {
      setDevices(await api.listDevices());
    } finally {
      setDevicesLoaded(true);
    }
  }, []);

  const refreshHealth = useCallback(async () => {
    setHealth(await api.libraryHealth(activeLibrary));
  }, [activeLibrary]);

  useEffect(() => {
    refreshDevices().catch(() => {});
  }, [refreshDevices]);

  // Health is cheap to recompute and depends on scans/analysis/edits, so it
  // simply follows every track refresh.
  useEffect(() => {
    refreshHealth().catch(() => {});
  }, [refreshHealth, tracks]);

  // A crate that vanished (rescan, stick unplugged) must not leave a
  // permanently empty table behind its stale filter.
  useEffect(() => {
    if (activeFolder === null || activeFolder === "" || activeLibrary === null) return;
    const list = crates.get(activeLibrary);
    if (list && !list.some((c) => c.path === activeFolder)) setActiveFolder(null);
  }, [crates, activeLibrary, activeFolder]);

  // Device plug/unplug: refresh sidebar state; reconnected known libraries
  // get an automatic incremental rescan.
  useEffect(() => {
    const unsubs = [
      api.onDeviceArrived((d) => {
        showToast(`Device connected: ${d.label} (${d.letter})`);
        refreshDevices().catch(() => {});
        refreshLibraries().catch(() => {});
        refreshTracks().catch(() => {});
        d.known_library_ids.forEach((id) => {
          api.scanLibrary(id).catch(() => {});
        });
      }),
      api.onDeviceRemoved((d) => {
        if (d.library_ids.length > 0) {
          showToast(`Device ${d.letter} removed — its tracks are marked missing`);
        }
        refreshDevices().catch(() => {});
        refreshLibraries().catch(() => {});
        refreshTracks().catch(() => {});
      }),
    ];
    return () => {
      unsubs.forEach((u) => u.then((f) => f()).catch(() => {}));
    };
  }, [refreshDevices, refreshLibraries, refreshTracks, showToast]);

  useEffect(() => {
    refreshLibraries()
      .catch((e) => showToast(String(e), true))
      .finally(() => hideSplash());
    api.apolloReady().then(setApolloOk).catch(() => setApolloOk(false));
  }, [refreshLibraries, showToast]);

  useEffect(() => {
    const keep = keepScrollRef.current;
    keepScrollRef.current = false;
    refreshTracks({ keepScroll: keep }).catch((e) => showToast(String(e), true));
  }, [refreshTracks, showToast]);

  useEffect(() => {
    const unsubs = [
      api.onScanProgress((p) => setScanning(p)),
      api.onScanDone((r) => {
        setScanning(null);
        showToast(
          `Scan done: ${r.found} files, ${r.new} new, ${r.updated} updated, ` +
            `${r.unchanged} unchanged in ${r.elapsed_secs.toFixed(1)}s`,
        );
        refreshLibraries().catch(() => {});
        refreshTracks().catch(() => {});
      }),
      api.onAnalysisProgress((p) => setAnalysis(p)),
      api.onAnalysisDone((p) => {
        setAnalysis(null);
        if (p.total > 0) {
          showToast(`Analysis done: ${p.done} analyzed, ${p.failed} failed`, p.failed > 0);
        }
        setWaveToken((n) => n + 1);
        refreshTracks().catch(() => {});
      }),
      api.onCopyProgress((p) => setCopying(p)),
      api.onCopyDone((p) => {
        setCopying(null);
        if (p.total > 0) {
          showToast(
            `Copied ${p.done} · skipped ${p.skipped} · ${p.failed} failed`,
            p.failed > 0,
          );
        }
        const target = copyTargetRef.current;
        copyTargetRef.current = null;
        if (target != null && p.done > 0) {
          setScanning({ library_id: target, found: 0 });
          api.scanLibrary(target).catch((e) => {
            setScanning(null);
            showToast(String(e), true);
          });
        }
      }),
      api.onBatchProgress((p) => setBatch(p)),
      api.onSyncProgress((p) => setSyncing(p)),
      api.onSyncDone((p) => {
        setSyncing(null);
        if (p.cancelled) return showToast("Rekordbox export cancelled");
        if (!p.ok) return showToast(`Rekordbox export failed: ${p.error ?? "unknown error"}`, true);
        setPreparedToken((t) => t + 1);
        const unread = p.unreadable.length ? ` · ${p.unreadable.length} files unreadable` : "";
        const dangling = p.dangling ? ` · ${p.dangling} dead crate links` : "";
        showToast(
          `Rekordbox XML: ${p.tracks} tracks · ${p.playlists} playlists · ${p.grids} grids · ${p.cues} cues · ${p.loops} loops${unread}${dangling}`,
          p.unreadable.length > 0,
        );
      }),
      api.onRekordboxImportProgress((p) => setRbImporting(p)),
      api.onRekordboxImportDone((p) => {
        setRbImporting(null);
        // crates and files may have changed even when the run stopped early
        setSeratoToken((t) => t + 1);
        refreshTracks().catch(() => {});
        if (p.cancelled) return showToast(`Rekordbox import cancelled after ${p.files_written} files`);
        if (p.error) return showToast(`Rekordbox import stopped: ${p.error}`, true);
        const failed = p.failed_names.length ? ` · failed: ${p.failed_names.join(", ")}` : "";
        showToast(
          `Rekordbox import: ${p.files_written} files updated · ${p.crates_created} crates created · ${p.crates_merged} merged · ${p.links_added} links added${failed}. In Serato: Rescan ID3 Tags.`,
          p.failed_names.length > 0,
        );
      }),
      api.onBatchDone((p) => {
        setBatch(null);
        const extra = p.failed_names.length ? ` · failed: ${p.failed_names.join(", ")}` : "";
        // a repair sweep reports what it deliberately left alone; a batch edit
        // writes every track it is given, so skipped is 0 and stays unsaid
        const fine = p.skipped ? ` · ${p.skipped} already fine` : "";
        showToast(`Updated ${p.done}${fine}${extra}`, p.failed > 0);
        refreshTracks().catch(() => {});
      }),
      api.onEnhanceProgress((p) => setEnhancing(p)),
      api.onEnhanceDone((p) => {
        setEnhancing(null);
        const extra = p.failed_names.length ? ` · failed: ${p.failed_names.join(", ")}` : "";
        const why = p.skipped_names.length ? ` · ${p.skipped_names.join("; ")}` : "";
        showToast(
          `Enhanced ${p.done} · skipped ${p.skipped} · ${p.failed} failed${why}${extra}`,
          p.failed > 0,
        );
        p.library_ids.forEach((id) => {
          api.scanLibrary(id).catch(() => {});
        });
        // New-file mode: reveal the first created file in Explorer so the
        // user sees where it landed (one window even for multi-track runs).
        if (p.new_files.length > 0) {
          api.revealPath(p.new_files[0]).catch(() => {});
        }
        if (p.enhanced_ids.length > 0) {
          api
            .startAnalysis(p.enhanced_ids, null, true)
            .then((n) => {
              if (n > 0) setAnalysis({ done: 0, failed: 0, total: n });
            })
            .catch(() => {});
        }
        refreshTracks().catch(() => {});
        refreshLibraries().catch(() => {});
      }),
      api.onStabilizeProgress((p) => setStabilizing(p)),
      api.onStabilizeDone((p) => {
        setStabilizing(null);
        const extra = p.failed_names.length ? ` · failed: ${p.failed_names.join(", ")}` : "";
        const why = p.skipped_names.length ? ` · ${p.skipped_names.join("; ")}` : "";
        showToast(
          `Stabilized ${p.done} · skipped ${p.skipped} · ${p.failed} failed${why}${extra}`,
          p.failed > 0,
        );
        p.library_ids.forEach((id) => {
          api.scanLibrary(id).catch(() => {});
        });
        if (p.new_files.length > 0) {
          api.revealPath(p.new_files[0]).catch(() => {});
        }
        // The samples changed, so the stored grid and drift are stale —
        // re-analyze so the panel shows the flattened figure.
        if (p.enhanced_ids.length > 0) {
          api
            .startAnalysis(p.enhanced_ids, null, true)
            .then((n) => {
              if (n > 0) setAnalysis({ done: 0, failed: 0, total: n });
            })
            .catch(() => {});
        }
        refreshTracks().catch(() => {});
        refreshLibraries().catch(() => {});
      }),
      api.onMlRestoreProgress((p) => setMlRestoreRun(p)),
      api.onMlRestoreDone((p) => {
        setMlRestoreRun(null);
        const extra = p.failed_names.length ? ` · failed: ${p.failed_names.join(", ")}` : "";
        const why = p.skipped_names.length ? ` · ${p.skipped_names.join("; ")}` : "";
        showToast(
          `Restored ${p.done} · skipped ${p.skipped} · ${p.failed} failed${why}${extra}`,
          p.failed > 0,
        );
        p.library_ids.forEach((id) => {
          api.scanLibrary(id).catch(() => {});
        });
        // New-file mode: reveal the first created file in Explorer so the
        // user sees where it landed (one window even for multi-track runs).
        if (p.new_files.length > 0) {
          api.revealPath(p.new_files[0]).catch(() => {});
        }
        if (p.enhanced_ids.length > 0) {
          api
            .startAnalysis(p.enhanced_ids, null, true)
            .then((n) => {
              if (n > 0) setAnalysis({ done: 0, failed: 0, total: n });
            })
            .catch(() => {});
        }
        refreshTracks().catch(() => {});
        refreshLibraries().catch(() => {});
      }),
    ];
    return () => {
      unsubs.forEach((u) => u.then((f) => f()).catch(() => {}));
    };
  }, [refreshLibraries, refreshTracks, showToast]);

  useEffect(() => {
    api.analyzerAvailable().then(setAnalyzerReady).catch(() => setAnalyzerReady(false));
  }, []);

  // Keep the deck's copy of the playing row fresh after rescans and
  // analysis runs (bpm/key readout updates in place).
  useEffect(() => {
    setPlaying((cur) => {
      if (!cur) return cur;
      const fresh = tracks.find((t) => t.id === cur.id);
      return fresh ?? cur;
    });
  }, [tracks]);

  // Ctrl+A selects every track in the current view; Escape clears the
  // selection. Both stay inactive while typing or while a modal is open.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")) return;
      if (
        editing ||
        comparing ||
        batchEditing ||
        qualityTrack != null ||
        enhanceDialog != null ||
        tempoDialog != null ||
        mlRestoreDialog != null ||
        dupesFor != null
      )
        return;
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
        e.preventDefault();
        setSelectedIds(new Set(tracks.map((t) => t.id)));
      } else if (e.key === "Escape") {
        setSelectedIds(new Set());
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [
    tracks, editing, comparing, batchEditing, qualityTrack, enhanceDialog, tempoDialog,
    mlRestoreDialog, dupesFor,
  ]);

  // Click selects; ctrl toggles; shift extends from the last plain click.
  const selectRow = useCallback(
    (index: number, e: React.MouseEvent) => {
      const id = tracks[index].id;
      if (e.ctrlKey || e.metaKey) {
        setSelectedIds((prev) => {
          const next = new Set(prev);
          if (next.has(id)) next.delete(id);
          else next.add(id);
          return next;
        });
        setAnchorIdx(index);
      } else if (e.shiftKey && anchorIdx !== null) {
        const [lo, hi] = anchorIdx < index ? [anchorIdx, index] : [index, anchorIdx];
        setSelectedIds(new Set(tracks.slice(lo, hi + 1).map((t) => t.id)));
      } else {
        setSelectedIds(new Set([id]));
        setAnchorIdx(index);
      }
    },
    [tracks, anchorIdx],
  );

  // Grid mode in the deck (the beat-grid editor). The editor reports whether
  // it holds unsaved edits so a track change can ask before dropping them.
  const [gridMode, setGridMode] = useState(false);
  const gridDirtyRef = useRef(false);
  const onGridDirtyChange = useCallback((d: boolean) => {
    gridDirtyRef.current = d;
  }, []);
  const leaveGridEdits = useCallback((): boolean => {
    if (!gridDirtyRef.current) return true;
    const ok = window.confirm("The grid has unsaved changes. Discard them?");
    if (ok) gridDirtyRef.current = false;
    return ok;
  }, []);

  const playTrack = useCallback(
    (t: api.TrackRow) => {
      if (!leaveGridEdits()) return;
      // Manual start = fresh listening context.
      historyRef.current = [];
      setPlaying(t);
      setPlayToken((n) => n + 1);
    },
    [leaveGridEdits],
  );

  const playStep = useCallback(
    (dir: 1 | -1) => {
      if (!leaveGridEdits()) return;
      setPlaying((current) => {
        if (!current) return current;
        if (shuffle) {
          if (dir === 1) {
            if (tracks.length === 0) return current;
            const pool = tracks.filter((t) => t.id !== current.id);
            // pool empty = the visible list is just the current track: replay it
            const next =
              pool.length === 0 ? tracks[0] : pool[Math.floor(Math.random() * pool.length)];
            historyRef.current.push(current.id);
            setPlayToken((n) => n + 1);
            return next;
          }
          // Prev: retrace history, skipping tracks no longer visible.
          while (historyRef.current.length > 0) {
            const id = historyRef.current.pop()!;
            const prev = tracks.find((t) => t.id === id);
            if (prev) {
              setPlayToken((n) => n + 1);
              return prev;
            }
          }
          return current;
        }
        const idx = tracks.findIndex((t) => t.id === current.id);
        let next = tracks[idx + dir];
        if (!next && repeat === "all" && tracks.length > 0) {
          next = dir === 1 ? tracks[0] : tracks[tracks.length - 1];
        }
        if (!next) return current;
        setPlayToken((n) => n + 1);
        return next;
      });
    },
    [tracks, shuffle, repeat, leaveGridEdits],
  );

  // Natural end of a track — the only place repeat-one applies.
  const advance = useCallback(() => {
    // the grid editor keeps its track loaded: the end of it just stops
    if (gridMode) return;
    if (repeat === "one") {
      setPlayToken((n) => n + 1);
      return;
    }
    playStep(1);
  }, [repeat, playStep, gridMode]);

  // no track, or one whose format holds no Serato grid: no grid mode
  useEffect(() => {
    if (!playing || !api.GRID_FORMATS.includes((playing.format ?? "").toLowerCase())) {
      setGridMode(false);
    }
  }, [playing]);

  const onPlayerError = useCallback(
    (message: string, trackId: number) => {
      showToast(message, true);
      api.markMissing(trackId).catch(() => {});
      refreshTracks().catch(() => {});
    },
    [refreshTracks, showToast],
  );

  const runAnalysis = useCallback(
    (mode: "selected" | "missing" | "overwrite") => {
      setAnalyzeMenu(false);
      const trackIds =
        mode === "selected" && selectedIds.size > 0 ? Array.from(selectedIds) : null;
      // analyzing an explicit selection always recomputes, even if bpm/key exist
      const overwrite = mode === "overwrite" || mode === "selected";
      api
        .startAnalysis(trackIds, mode === "selected" ? null : activeLibrary, overwrite)
        .then((n) => {
          if (n === 0) showToast("Nothing to analyze — all tracks have BPM and key");
          else setAnalysis({ done: 0, failed: 0, total: n });
        })
        .catch((e) => showToast(String(e), true));
    },
    [activeLibrary, selectedIds, showToast],
  );

  const addLibrary = useCallback(async () => {
    try {
      const dir = await open({ directory: true, title: "Choose a music folder" });
      if (!dir || Array.isArray(dir)) return;
      const lib = await api.addLibrary(dir);
      await refreshLibraries();
      setActiveLibrary(lib.id);
      setScanning({ library_id: lib.id, found: 0 });
      api.scanLibrary(lib.id).catch((e) => {
        setScanning(null);
        showToast(String(e), true);
      });
    } catch (e) {
      showToast(String(e), true);
    }
  }, [refreshLibraries, showToast]);

  // "Create new folder…": pick the parent location, then ask for a name.
  const pickNewLibraryParent = useCallback(async () => {
    try {
      const dir = await open({ directory: true, title: "Where should the new folder live?" });
      if (!dir || Array.isArray(dir)) return;
      setNewLibName("");
      setNewLibParent(dir);
    } catch (e) {
      showToast(String(e), true);
    }
  }, [showToast]);

  const createNewLibrary = useCallback(async () => {
    if (!newLibParent) return;
    try {
      const lib = await api.createLibrary(newLibParent, newLibName);
      setNewLibParent(null);
      await refreshLibraries();
      setActiveLibrary(lib.id);
      showToast(`Created empty library "${lib.name}" — copy tracks into it via Copy to`);
    } catch (e) {
      showToast(String(e), true);
    }
  }, [newLibParent, newLibName, refreshLibraries, showToast]);

  const startCopy = useCallback(
    (targetId: number, trackIds?: number[], destFolder?: string) => {
      setCopyMenu(false);
      copyTargetRef.current = targetId;
      api
        .copyTracks(trackIds ?? Array.from(selectedIds), targetId, destFolder)
        .then((n) => {
          if (n === 0) showToast("Nothing to copy");
          else setCopying({ done: 0, skipped: 0, failed: 0, total: n });
        })
        .catch((e) => showToast(String(e), true));
    },
    [selectedIds, showToast],
  );

  const copyCrateTo = useCallback(
    (sourceLibraryId: number, folder: string, target: api.LibraryInfo) => {
      const name = folder.split("/").pop() ?? folder;
      const ok = window.confirm(
        `Copy crate "${name}" (incl. subcrates) to "${target.name}"?\n\n` +
          "Existing files are skipped — nothing is overwritten.",
      );
      if (!ok) return;
      copyTargetRef.current = target.id;
      api
        .copyCrate(sourceLibraryId, folder, target.id)
        .then((n) => {
          if (n === 0) showToast("Nothing to copy");
          else setCopying({ done: 0, skipped: 0, failed: 0, total: n });
        })
        .catch((e) => showToast(String(e), true));
    },
    [showToast],
  );

  const startBatch = useCallback(
    (update: api.TagUpdate, apply: string[]) => {
      const ids = (batchEditing ?? []).map((t) => t.id);
      api
        .updateTracksMetadata(ids, update, apply)
        .then((total) => {
          if (total === 0) showToast("Nothing to update");
          else setBatch({ done: 0, failed: 0, total });
        })
        .catch((e) => showToast(String(e), true));
    },
    [batchEditing, showToast],
  );

  // True if any of the given tracks lives on a connected device that has
  // Serato crates (same check as MetadataEditor's seratoHint).
  const anyOnSeratoDevice = useCallback(
    (ids: number[]) => {
      const idSet = new Set(ids);
      return tracks.some((t) => {
        if (!idSet.has(t.id)) return false;
        const lib = libraries.find((l) => l.id === t.library_id);
        if (!lib?.volume_serial) return false;
        const dev = devices.find((d) => d.serial === lib.volume_serial);
        return dev != null && (seratoCrates.get(dev.letter) ?? []).length > 0;
      });
    },
    [tracks, libraries, devices, seratoCrates],
  );

  const startEnhance = useCallback(
    (mode: api.EnhanceMode, format: api.EnhanceFormat, destDir: string | null) => {
      const ids = enhanceDialog ?? [];
      api
        .enhanceTracks(ids, mode, format, destDir)
        .then((n) => {
          if (n === 0) showToast("Nothing to enhance — already at target loudness");
          else setEnhancing({ done: 0, failed: 0, skipped: 0, total: n });
        })
        .catch((e) => showToast(String(e), true));
    },
    [enhanceDialog, showToast],
  );

  const startStabilize = useCallback(
    (
      targetBpm: number | null,
      mode: api.EnhanceMode,
      format: api.EnhanceFormat,
      destDir: string | null,
    ) => {
      const ids = tempoDialog ?? [];
      api
        .stabilizeTracks(ids, targetBpm, mode, format, destDir)
        .then((n) => {
          if (n === 0) showToast("Nothing to stabilize");
          else setStabilizing({ done: 0, failed: 0, skipped: 0, total: n });
        })
        .catch((e) => showToast(String(e), true));
    },
    [tempoDialog, showToast],
  );

  const startMlRestore = useCallback(
    (mode: api.EnhanceMode, format: api.EnhanceFormat, destDir: string | null) => {
      const ids = mlRestoreDialog ?? [];
      api
        .mlRestoreTracks(ids, mode, format, destDir)
        .then((n) => {
          if (n === 0) showToast("Nothing to restore");
          else setMlRestoreRun({ done: 0, failed: 0, skipped: 0, total: n, chunk_done: 0, chunk_total: 0, current: "" });
        })
        .catch((e) => showToast(String(e), true));
    },
    [mlRestoreDialog, showToast],
  );
  // Soft-gate eligibility notes for the restore dialog: not-yet-analyzed
  // tracks can't be judged, and full-band tracks are unlikely to benefit.
  // loudness_lufs is the "has been analyzed" signal (mirrors QualityPanel's
  // own verdict()) — cutoff_hz alone can't tell "not analyzed" apart from
  // "analyzed, no band-limiting found".
  const restoreHints = useCallback((rows: api.TrackRow[]): string[] => {
    const out: string[] = [];
    const unanalyzed = rows.filter((r) => r.loudness_lufs == null).length;
    const fullBand = rows.filter((r) => r.loudness_lufs != null && r.cutoff_hz == null).length;
    if (unanalyzed > 0) {
      out.push(
        rows.length === 1
          ? "This track is not analyzed yet — analyze first to see if restoration is needed."
          : `${unanalyzed} of ${rows.length} not analyzed yet — analyze first to see if restoration is needed.`,
      );
    }
    if (fullBand > 0) {
      out.push(
        rows.length === 1
          ? "This track shows no band-limiting — restoration is unlikely to help."
          : `${fullBand} of ${rows.length} show no band-limiting — restoration is unlikely to help those.`,
      );
    }
    return out;
  }, []);

  // "Restore original" for tracks enhanced with mode "replace": pulls the
  // pre-enhance backup back over the current file.
  const restoreSelected = useCallback(async () => {
    if (selectedIds.size === 0) return;
    const ids = Array.from(selectedIds);
    const n = ids.length;
    const ok = window.confirm(
      `Restore the original file for ${n} track${n === 1 ? "" : "s"} from backup?\n\n` +
        "The enhanced version is replaced by the pre-enhance original.",
    );
    if (!ok) return;
    try {
      const failed = await api.restoreTracks(ids);
      showToast(
        `Restored ${n - failed.length}` +
          (failed.length > 0 ? ` · failed: ${failed.join(", ")}` : ""),
        failed.length > 0,
      );
      // A restore is a fresh file on disk (the pre-enhance original) —
      // re-analyze it, mirroring onEnhanceDone, so bpm/key/loudness reflect
      // what's actually there instead of the enhanced version's stale values.
      // failed entries are named "<filename>: <reason>" (see restore_tracks);
      // match that prefix against each id's own filename to find which ids
      // actually came back restored.
      const failedNames = new Set(failed.map((f) => f.split(": ")[0]));
      const restoredIds = ids.filter((id) => {
        const t = tracks.find((tt) => tt.id === id);
        const filename = t?.rel_path.split("/").pop() ?? "";
        return !failedNames.has(filename);
      });
      if (restoredIds.length > 0) {
        api
          .startAnalysis(restoredIds, null, true)
          .then((n) => {
            if (n > 0) setAnalysis({ done: 0, failed: 0, total: n });
          })
          .catch(() => {});
      }
      refreshTracks().catch(() => {});
    } catch (e) {
      showToast(String(e), true);
    }
  }, [selectedIds, tracks, refreshTracks, showToast]);

  const rescan = useCallback(
    (id: number) => {
      setScanning({ library_id: id, found: 0 });
      api.scanLibrary(id).catch((e) => {
        setScanning(null);
        showToast(String(e), true);
      });
    },
    [showToast],
  );

  // Files dragged in from Explorer. The webview only exposes their bytes
  // (dragDropEnabled is false, so no native paths), so each file is read and
  // shipped to the import_file command; the target rescans once at the end.
  const importOsFiles = useCallback(
    async (libraryId: number, folder: string, files: FileList) => {
      const all = Array.from(files);
      const audio = all.filter((f) => /\.(mp3|flac|wav|aiff?|aac|m4a)$/i.test(f.name));
      const notAudio = all.length - audio.length;
      if (audio.length === 0) {
        showToast("No audio files in that drop", true);
        return;
      }
      let copied = 0;
      let skipped = 0;
      let failed = 0;
      // First failure's message, shown in the toast — "N failed" alone left
      // the actual reason buried in the dev console.
      let firstError = "";
      for (const f of audio) {
        try {
          const data = new Uint8Array(await f.arrayBuffer());
          const r = await api.importFile(libraryId, folder, f.name, data);
          if (r === "copied") copied += 1;
          else skipped += 1;
        } catch (e) {
          console.error(`import ${f.name}:`, e);
          if (!firstError) firstError = `${f.name}: ${String(e)}`.slice(0, 160);
          failed += 1;
        }
      }
      const parts = [`${copied} imported`];
      if (skipped > 0) parts.push(`${skipped} already there`);
      if (failed > 0) parts.push(`${failed} failed — ${firstError}`);
      if (notAudio > 0) parts.push(`${notAudio} not audio`);
      showToast(parts.join(" · "), copied === 0 && failed > 0);
      if (copied > 0) rescan(libraryId);
    },
    [rescan, showToast],
  );

  // A drop that misses every target must never navigate the webview to the
  // dropped file. Row handlers run first and preventDefault; anything still
  // unhandled here is swallowed.
  useEffect(() => {
    const over = (e: DragEvent) => {
      if (!e.defaultPrevented) {
        e.preventDefault();
        if (e.dataTransfer) e.dataTransfer.dropEffect = "none";
      }
    };
    const drop = (e: DragEvent) => {
      if (!e.defaultPrevented) e.preventDefault();
    };
    window.addEventListener("dragover", over);
    window.addEventListener("drop", drop);
    return () => {
      window.removeEventListener("dragover", over);
      window.removeEventListener("drop", drop);
    };
  }, []);

  // Safe-removal: locks + dismounts + ejects the volume; fails with a clear
  // message while something (e.g. playback) still holds a file open.
  const ejectStick = useCallback(
    async (letter: string, label: string) => {
      try {
        await api.ejectDevice(letter);
        showToast(`${label} ejected — safe to remove`);
      } catch (e) {
        showToast(String(e), true);
      }
    },
    [showToast],
  );

  // Drops the stick's libraries + index. No files are touched; on next
  // connect it presents like a brand-new USB.
  const forgetStick = useCallback(
    async (serial: string, label: string, libIds: number[]) => {
      const ok = window.confirm(
        `Remove ${label} from SpinDex?\n\n` +
          "Only the index entry is removed — no files on the stick are " +
          "touched. When reconnected it appears as a new device.",
      );
      if (!ok) return;
      try {
        await api.forgetDevice(serial);
        if (activeLibrary !== null && libIds.includes(activeLibrary)) {
          setActiveLibrary(null);
          setActiveFolder(null);
        }
        await refreshLibraries();
        await refreshDevices();
        showToast(`${label} removed from the library list`);
      } catch (e) {
        showToast(String(e), true);
      }
    },
    [activeLibrary, refreshLibraries, refreshDevices, showToast],
  );

  // Create/rename a crate = mkdir / rename the real folder.
  const submitCrateModal = useCallback(async () => {
    if (!crateModal || crateName.trim() === "") return;
    try {
      if (crateModal.mode === "create") {
        await api.createCrate(crateModal.libraryId, crateModal.parentPath, crateName);
        setOpenCrates((prev) => new Set(prev).add(crateModal.libraryId));
      } else {
        const newPath = await api.renameCrate(
          crateModal.libraryId,
          crateModal.path,
          crateName,
        );
        // keep the filter on the renamed crate (also when a parent renamed)
        setActiveFolder((cur) => {
          if (cur === null || activeLibrary !== crateModal.libraryId) return cur;
          if (cur === crateModal.path) return newPath;
          if (cur.startsWith(crateModal.path + "/"))
            return newPath + cur.slice(crateModal.path.length);
          return cur;
        });
      }
      setCrateModal(null);
      await refreshLibraries();
      refreshTracks().catch(() => {});
    } catch (e) {
      showToast(String(e), true);
    }
  }, [crateModal, crateName, activeLibrary, refreshLibraries, refreshTracks, showToast]);

  // Deleting a crate only ever removes an empty folder (the backend refuses
  // anything else), so a light confirm is enough.
  const deleteCrate = useCallback(
    async (libraryId: number, path: string) => {
      const name = path.split("/").pop() ?? path;
      if (!window.confirm(`Delete the empty crate "${name}"?`)) return;
      try {
        await api.deleteCrate(libraryId, path);
        if (activeLibrary === libraryId && activeFolder === path) setActiveFolder(null);
        await refreshLibraries();
      } catch (e) {
        showToast(String(e), true);
      }
    },
    [activeLibrary, activeFolder, refreshLibraries, showToast],
  );

  // Drop of dragged tracks on a crate: same library moves the files into the
  // folder, other libraries copy them (via the copy engine, into the folder).
  const dropTracksOnCrate = useCallback(
    async (lib: api.LibraryInfo, folderPath: string, ids: number[]) => {
      // a drag from the split overview carries rows the table may not hold
      const own = ids.filter(
        (id) =>
          (tracks.find((t) => t.id === id) ?? foreignDragRef.current?.get(id))?.library_id ===
          lib.id,
      );
      const crateName = folderPath.split("/").pop() ?? folderPath;
      try {
        if (own.length === ids.length) {
          const ok = window.confirm(
            `Move ${ids.length} track${ids.length === 1 ? "" : "s"} into "${crateName}"?\n\n` +
              "The files move into that folder on disk.",
          );
          if (!ok) return;
          const res = await api.moveTracksToFolder(ids, lib.id, folderPath);
          showToast(
            `Moved ${res.moved} · skipped ${res.skipped}` +
              (res.failed > 0 ? ` · ${res.failed} failed` : ""),
            res.failed > 0,
          );
          await refreshLibraries();
          refreshTracks().catch(() => {});
        } else {
          const ok = window.confirm(
            `Copy ${ids.length} track${ids.length === 1 ? "" : "s"} into "${crateName}" (${lib.name})?\n\n` +
              "The files are copied into that folder; sources stay untouched.",
          );
          if (!ok) return;
          startCopy(lib.id, ids, folderPath);
        }
      } catch (e) {
        showToast(String(e), true);
      }
    },
    [tracks, refreshLibraries, refreshTracks, showToast, startCopy],
  );

  // "Remove from crate": the files move up to the library root, confirmed.
  const removeFromCrate = useCallback(async () => {
    if (activeLibrary === null || !activeFolder || selectedIds.size === 0) return;
    const n = selectedIds.size;
    const ok = window.confirm(
      `Remove ${n} track${n === 1 ? "" : "s"} from this crate?\n\n` +
        "The files move up to the library's root folder — nothing is deleted.",
    );
    if (!ok) return;
    try {
      const res = await api.moveTracksToFolder(Array.from(selectedIds), activeLibrary, "");
      showToast(
        `Moved ${res.moved} to the library root · skipped ${res.skipped}` +
          (res.failed > 0 ? ` · ${res.failed} failed` : ""),
        res.failed > 0,
      );
      setSelectedIds(new Set());
      await refreshLibraries();
      refreshTracks().catch(() => {});
    } catch (e) {
      showToast(String(e), true);
    }
  }, [activeLibrary, activeFolder, selectedIds, refreshLibraries, refreshTracks, showToast]);

  // The toolbar's Sort menu: re-numbers the whole open crate once, then shows
  // it in its own (new) order.
  const sortCrate = useCallback(
    async (mode: api.CrateSortMode) => {
      const name = seratoFilter
        ? seratoFilter.name
        : activeFolder !== null
          ? activeFolder.split("/").pop() || "(root)"
          : null;
      if (name === null) return;
      const ok = window.confirm(
        `Re-number every track in "${name}" — ${api.CRATE_SORT_LABELS[mode].toLowerCase()}?\n\n` +
          "This replaces the crate's current order." +
          (seratoFilter ? "\nClose Serato first — it overwrites crate edits when it exits." : ""),
      );
      if (!ok) return;
      try {
        if (seratoFilter) {
          await api.seratoSortTracks(seratoFilter.letter, seratoFilter.file, mode);
          setSort(null);
          setSeratoToken((n) => n + 1);
        } else if (activeLibrary !== null && activeFolder !== null) {
          await api.sortCrateTracks(activeLibrary, activeFolder, mode);
          setSort(CRATE_ORDER_SORT);
          await refreshTracks();
        }
        setPaneToken((n) => n + 1);
        showToast(`"${name}" re-numbered — ${api.CRATE_SORT_LABELS[mode].toLowerCase()}`);
      } catch (e) {
        showToast(`Could not sort: ${e}`, true);
      }
    },
    [seratoFilter, activeLibrary, activeFolder, refreshTracks, showToast],
  );

  // Both the ▲▼ buttons and the context-menu entries land here. The backend
  // rewrites the order file first, so a failure means nothing changed at all.
  const moveSelection = useCallback(
    async (delta: number, ids?: number[]) => {
      const targets = ids ?? Array.from(selectedIds);
      if (targets.length === 0) return;
      try {
        if (seratoManualMode && seratoFilter) {
          await api.seratoMoveTracks(seratoFilter.letter, seratoFilter.file, targets, delta);
          // re-read the .crate (it is the authority) without losing the user's
          // place or their selection: the fresh ids arrive through seratoToken
          keepScrollRef.current = true;
          setSeratoToken((t) => t + 1);
        } else if (manualMode && activeLibrary !== null && activeFolder !== null) {
          await api.moveCrateTracks(targets, activeLibrary, activeFolder, delta);
          await refreshTracks({ keepScroll: true });
        } else {
          return;
        }
      } catch (e) {
        showToast(`Could not reorder: ${e}`, true);
      }
    },
    [
      manualMode,
      seratoManualMode,
      seratoFilter,
      activeLibrary,
      activeFolder,
      selectedIds,
      refreshTracks,
      showToast,
    ],
  );

  // Split view: tracks dragged from the overview land in the open crate
  // before row `before`. No confirm: nothing is deleted or overwritten, and a
  // box per drag would get in the way of building a crate.
  // Adds dropped tracks to any folder or Serato crate before row `before`,
  // whichever pane shows it; every pane and the sidebar counts then reload.
  const addToCrate = useCallback(
    async (target: SplitSource, ids: number[], before: number) => {
      if (ids.length === 0) return;
      const plural = (n: number) => `${n} track${n === 1 ? "" : "s"}`;
      try {
        if (target.kind === "serato") {
          const crate = seratoCrates.get(target.letter)?.find((c) => c.file === target.file);
          const res = await api.seratoAddTracks(target.letter, target.file, ids, before);
          showToast(
            `Added ${plural(res.added)} to "${crate?.name ?? target.file}"` +
              (res.copied > 0 ? ` · copied ${res.copied} to the stick` : "") +
              (res.failed > 0 ? ` · ${res.failed} failed` : ""),
            res.failed > 0,
          );
          if (res.rescan_library !== null) {
            setScanning({ library_id: res.rescan_library, found: 0 });
            api.scanLibrary(res.rescan_library).catch(() => setScanning(null));
          }
          keepScrollRef.current = true;
          setSeratoToken((n) => n + 1);
          setPaneToken((n) => n + 1);
        } else if (target.kind === "folder") {
          const folder = target.path;
          const name = folder.split("/").pop() || folder;
          if (ids.length > 3) showToast(`Adding ${plural(ids.length)} to "${name}"…`);
          const res = await api.addTracksToCrate(ids, target.libraryId, folder, before);
          const parts = [
            res.moved > 0 ? `Moved ${res.moved}` : "",
            res.copied > 0 ? `${res.moved > 0 ? "copied" : "Copied"} ${res.copied}` : "",
          ].filter(Boolean);
          showToast(
            `${parts.length > 0 ? parts.join(" · ") : "Nothing added"} into "${name}"` +
              (res.skipped > 0 ? ` · ${res.skipped} already there` : "") +
              (res.failed > 0 ? ` · ${res.failed} failed` : ""),
            res.failed > 0,
          );
          // copied files are indexed by the rescan; their place in the order
          // is already written, so they land where they were dropped
          if (res.copied > 0) {
            setScanning({ library_id: target.libraryId, found: 0 });
            api.scanLibrary(target.libraryId).catch(() => setScanning(null));
          }
          await refreshLibraries();
          // a move out of the crate on the right changes it too
          await refreshTracks({ keepScroll: true });
          setPaneToken((n) => n + 1);
        }
      } catch (e) {
        showToast(String(e), true);
      }
    },
    [seratoCrates, refreshLibraries, refreshTracks, showToast],
  );

  // the running order's own crate, as a split source (null: not a crate)
  const mainCrate = useMemo<SplitSource | null>(
    () =>
      seratoManualMode && seratoFilter
        ? { kind: "serato", letter: seratoFilter.letter, file: seratoFilter.file }
        : manualMode && activeLibrary !== null && activeFolder !== null
          ? { kind: "folder", libraryId: activeLibrary, path: activeFolder }
          : null,
    [seratoManualMode, seratoFilter, manualMode, activeLibrary, activeFolder],
  );
  // tracks dragged from a pane land in the running order before row `before`
  const addFromOverview = useCallback(
    (ids: number[], before: number) => {
      if (mainCrate) void addToCrate(mainCrate, ids, before);
    },
    [addToCrate, mainCrate],
  );

  // a pane's own crate rows dropped before row `before`
  const reorderInPane = useCallback(
    async (target: SplitSource, ids: number[], before: number) => {
      try {
        if (target.kind === "serato") {
          await api.seratoReorderTracks(target.letter, target.file, ids, before);
          keepScrollRef.current = true;
          setSeratoToken((n) => n + 1);
        } else if (target.kind === "folder") {
          await api.reorderCrateTracks(ids, target.libraryId, target.path, before);
          await refreshTracks({ keepScroll: true });
        }
        setPaneToken((n) => n + 1);
      } catch (e) {
        showToast(`Could not reorder: ${e}`, true);
      }
    },
    [refreshTracks, showToast],
  );

  // Drop of dragged tracks on a Serato crate: stick tracks are linked,
  // foreign tracks are copied to the stick first, then linked.
  const dropTracksOnSerato = useCallback(
    async (letter: string, crate: api.SeratoCrateInfo, ids: number[]) => {
      const foreign = ids.filter((id) => {
        const t = tracks.find((r) => r.id === id) ?? foreignDragRef.current?.get(id);
        const lib = t && libraries.find((l) => l.id === t.library_id);
        return !lib || !lib.root_path.startsWith(`${letter}\\`);
      }).length;
      const ok = window.confirm(
        `Add ${ids.length} track${ids.length === 1 ? "" : "s"} to the Serato crate "${crate.name}"?\n\n` +
          (foreign > 0
            ? `${foreign} of them are not on the stick yet and will be copied there first.\n`
            : "") +
          "Close Serato first — it overwrites crate edits when it exits.\n" +
          "The crate file is backed up before its first change.",
      );
      if (!ok) return;
      try {
        const res = await api.seratoAddTracks(letter, crate.file, ids);
        showToast(
          `Added ${res.added} to "${crate.name}"` +
            (res.copied > 0 ? ` · copied ${res.copied} to the stick` : "") +
            (res.failed > 0 ? ` · ${res.failed} failed` : ""),
          res.failed > 0,
        );
        if (res.rescan_library !== null) {
          setScanning({ library_id: res.rescan_library, found: 0 });
          api.scanLibrary(res.rescan_library).catch(() => setScanning(null));
        }
        setSeratoToken((n) => n + 1);
      } catch (e) {
        showToast(String(e), true);
      }
    },
    [tracks, libraries, showToast],
  );

  // Removes the selection's link entries from the active Serato crate.
  const removeFromSerato = useCallback(async () => {
    if (!seratoFilter || selectedIds.size === 0) return;
    const n = selectedIds.size;
    const ok = window.confirm(
      `Remove ${n} track${n === 1 ? "" : "s"} from the Serato crate "${seratoFilter.name}"?\n\n` +
        "Only the crate's link entries go — the audio files stay where they are.",
    );
    if (!ok) return;
    try {
      const removed = await api.seratoRemoveTracks(
        seratoFilter.letter,
        seratoFilter.file,
        Array.from(selectedIds),
      );
      showToast(`Removed ${removed} from "${seratoFilter.name}"`);
      setSelectedIds(new Set());
      setSeratoToken((t) => t + 1);
    } catch (e) {
      showToast(String(e), true);
    }
  }, [seratoFilter, selectedIds, showToast]);

  // Recycle-Bin delete of the selection: the one action that removes audio
  // files, always behind this confirm. USB sticks have no Recycle Bin.
  const deleteSelected = useCallback(async () => {
    if (selectedIds.size === 0) return;
    const ids = Array.from(selectedIds);
    const n = ids.length;
    const removable = new Set(libraries.filter((l) => l.is_removable).map((l) => l.id));
    const selected = tracks.filter((t) => selectedIds.has(t.id));
    const items: DeleteItem[] = selected.map((t) => ({
      name: trackDisplayName(t),
      onStick: removable.has(t.library_id),
      missing: t.is_missing,
    }));
    // Selected rows outside the loaded page are deleted too; list them as
    // unnamed so the count in the confirm is always the real one.
    for (let i = selected.length; i < n; i++) {
      items.push({ name: "(track not loaded in this view)", onStick: false, missing: false });
    }
    const ok = await askDelete(items);
    if (!ok) return;
    try {
      const res = await api.deleteTracks(ids);
      showToast(
        `Deleted ${res.deleted}` +
          (res.failed.length > 0 ? ` · failed: ${res.failed.join(", ")}` : ""),
        res.failed.length > 0,
      );
      setSelectedIds(new Set());
      setPlaying((cur) => (cur && ids.includes(cur.id) ? null : cur));
      await refreshLibraries();
      refreshTracks().catch(() => {});
    } catch (e) {
      showToast(String(e), true);
    }
  }, [selectedIds, tracks, libraries, refreshLibraries, refreshTracks, showToast, askDelete]);

  // Delete key: inside a crate view it means "remove from this crate",
  // otherwise Recycle-Bin delete. Both confirm first. Inactive while typing
  // or while a modal is open (same guards as Ctrl+A/Escape).
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key !== "Delete") return;
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")) return;
      if (
        editing ||
        comparing ||
        batchEditing ||
        qualityTrack != null ||
        enhanceDialog != null ||
        tempoDialog != null ||
        mlRestoreDialog != null ||
        dupesFor != null ||
        selectedIds.size === 0
      )
        return;
      e.preventDefault();
      if (seratoFilter) removeFromSerato();
      else if (activeLibrary !== null && activeFolder !== null && activeFolder !== "") removeFromCrate();
      else deleteSelected();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [
    editing,
    comparing,
    batchEditing,
    qualityTrack,
    enhanceDialog,
    tempoDialog,
    mlRestoreDialog,
    dupesFor,
    selectedIds,
    seratoFilter,
    activeLibrary,
    activeFolder,
    removeFromSerato,
    removeFromCrate,
    deleteSelected,
  ]);

  const submitSeratoModal = useCallback(async () => {
    if (!seratoModal || crateName.trim() === "") return;
    try {
      if (seratoModal.mode === "create") {
        await api.seratoCreateCrate(seratoModal.letter, crateName);
      } else {
        const newFile = await api.seratoRenameCrate(
          seratoModal.letter,
          seratoModal.file,
          crateName,
        );
        setSeratoFilter((cur) =>
          cur && cur.letter === seratoModal.letter && cur.file === seratoModal.file
            ? { ...cur, file: newFile }
            : cur,
        );
      }
      setSeratoModal(null);
      setSeratoToken((t) => t + 1);
    } catch (e) {
      showToast(String(e), true);
    }
  }, [seratoModal, crateName, showToast]);

  const deleteSeratoCrate = useCallback(
    async (letter: string, crate: api.SeratoCrateInfo) => {
      const ok = window.confirm(
        `Delete the Serato crate "${crate.name}"?\n\n` +
          "Only the crate file is removed (a backup is kept in " +
          "_Serato_\\SpinDexBackups) — no audio file is touched.",
      );
      if (!ok) return;
      try {
        await api.seratoDeleteCrate(letter, crate.file);
        setSeratoFilter((cur) =>
          cur && cur.letter === letter && cur.file === crate.file ? null : cur,
        );
        setSeratoToken((t) => t + 1);
      } catch (e) {
        showToast(String(e), true);
      }
    },
    [showToast],
  );

  // Writes SpinDex's stored metadata into the crate's files, so Serato and
  // other players see what SpinDex shows. Only files that would actually
  // change are rewritten — the backend compares first — so this is safe to
  // run on a whole crate.
  const runTagRepair = useCallback(
    async (label: string, start: () => Promise<number>) => {
      const ok = window.confirm(
        `Write SpinDex's tags into the files of "${label}"?\n\n` +
          "Title, artist, album, genre, year, BPM, key, comment and label are " +
          "written into each file, replacing what the file holds. BPM and key " +
          "live only in SpinDex until now, so this is how Serato finally sees " +
          "them.\n\n" +
          "Fields SpinDex has no value for are left alone, files already " +
          "matching are skipped, and no audio is changed.\n\n" +
          "Close Serato first, or it will not see the result.",
      );
      if (!ok) return;
      try {
        const total = await start();
        if (total === 0) showToast("No tracks to check in that crate");
      } catch (e) {
        showToast(`Could not start: ${e}`, true);
      }
    },
    [showToast],
  );

  const regenGrid = useCallback(
    async (label: string, start: (rewriteAll: boolean) => Promise<number>) => {
      const ok = window.confirm(
        `Write a Serato beat grid for "${label}" from SpinDex's analysis?\n\n` +
          "Each file is modified in place (a one-time backup is made in " +
          "_SpinDexBackups_). Tracks not yet analyzed are skipped. Constant grid " +
          "for steady tracks, a moving grid for tracks that drift.\n\n" +
          "Close Serato first, then use its 'Rescan ID3 Tags' afterwards.\n\n" +
          "OK = only tracks without a SpinDex grid. Cancel to abort.",
      );
      if (!ok) return;
      try {
        const total = await start(false);
        if (total === 0) showToast("No analyzed tracks needing a grid");
        else setBatch({ done: 0, failed: 0, total });
      } catch (e) {
        showToast(`Could not start: ${e}`, true);
      }
    },
    [showToast],
  );

  const openSeratoSync = useCallback(
    async (letter: string, crate: api.SeratoCrateInfo) => {
      setSyncingSerato(`${letter}:${crate.file}`);
      try {
        const plan = await api.seratoSyncPreview(letter, crate.file);
        setSeratoSync({ letter, file: crate.file, name: crate.name, plan });
      } catch (e) {
        showToast(String(e), true);
      } finally {
        setSyncingSerato(null);
      }
    },
    [showToast],
  );

  // Unlinks one library from the app. Files on disk are never touched.
  const removeLib = useCallback(
    async (lib: api.LibraryInfo) => {
      const ok = window.confirm(
        `Remove "${lib.name}" from SpinDex?\n\n` +
          "Only the library entry and its index are removed — the folder " +
          "and all files on disk stay untouched.",
      );
      if (!ok) return;
      try {
        await api.removeLibrary(lib.id);
        if (activeLibrary === lib.id) {
          setActiveLibrary(null);
          setActiveFolder(null);
        }
        await refreshLibraries();
        await refreshDevices();
        showToast(`"${lib.name}" removed — files untouched`);
      } catch (e) {
        showToast(String(e), true);
      }
    },
    [activeLibrary, refreshLibraries, refreshDevices, showToast],
  );

  const totalTracks = libraries.reduce((n, l) => n + l.track_count, 0);
  const libraryName = useCallback(
    (id: number) => libraries.find((l) => l.id === id)?.name ?? "—",
    [libraries],
  );

  return (
    <div
      className={"app" + (gridMode ? " grid-mode" : "")}
      style={{ gridTemplateColumns: `${sideWidth}px 1fr` }}
    >
      <div className="titlebar">
        <div className="logo">
          SPIN<span>DEX</span>
        </div>
        <div className="stats">
          <div>
            <b>{totalTracks.toLocaleString()}</b> tracks
          </div>
          <div>
            <b>{libraries.length}</b> {libraries.length === 1 ? "library" : "libraries"}
          </div>
          <div>
            scan:{" "}
            {scanning ? (
              <b className="scan-live">{scanning.found.toLocaleString()} files…</b>
            ) : (
              <b style={{ color: "var(--green)" }}>idle</b>
            )}
          </div>
        </div>
      </div>

      <div
        className="side-resizer"
        style={{ left: sideWidth - 3 }}
        onMouseDown={startSidebarResize}
      />
      <Sidebar
        libraries={libraries}
        totalTracks={totalTracks}
        activeLibrary={activeLibrary}
        setActiveLibrary={setActiveLibrary}
        activeFolder={activeFolder}
        setActiveFolder={setActiveFolder}
        scanning={scanning}
        rescan={rescan}
        devices={devices}
        devicesLoaded={devicesLoaded}
        crates={crates}
        openCrates={openCrates}
        setOpenCrates={setOpenCrates}
        collapsedCrates={collapsedCrates}
        setCollapsedCrates={setCollapsedCrates}
        dropLib={dropLib}
        setDropLib={setDropLib}
        dropCrate={dropCrate}
        setDropCrate={setDropCrate}
        dragTracksRef={dragTracksRef}
        startCopy={startCopy}
        importOsFiles={importOsFiles}
        setComparing={setComparing}
        removeLib={removeLib}
        setCrateName={setCrateName}
        setCrateModal={setCrateModal}
        setCrateCopyMenu={setCrateCopyMenu}
        runTagRepair={runTagRepair}
        regenGrid={regenGrid}
        dropTracksOnCrate={dropTracksOnCrate}
        deleteCrate={deleteCrate}
        setSort={setSort}
        ejectStick={ejectStick}
        exportToRekordbox={(letter, label, crate) => setRbxExport({ letter, label, crate })}
        importFromRekordbox={(letter, label) => setRbxImport({ letter, label })}
        rbImporting={rbImporting}
        syncing={syncing}
        forgetStick={forgetStick}
        health={health}
        healthFilter={healthFilter}
        setHealthFilter={setHealthFilter}
        healthCollapsed={healthCollapsed}
        setHealthCollapsed={setHealthCollapsed}
        setDupesFor={setDupesFor}
        showToast={showToast}
        analysis={analysis}
        copying={copying}
        batch={batch}
        enhancing={enhancing}
        stabilizing={stabilizing}
        mlRestoreRun={mlRestoreRun}
        addMenu={addMenu}
        setAddMenu={setAddMenu}
        addLibrary={addLibrary}
        pickNewLibraryParent={pickNewLibraryParent}
        refreshLibraries={refreshLibraries}
        refreshDevices={refreshDevices}
        refreshTracks={refreshTracks}
        setScanning={setScanning}
        seratoCrates={seratoCrates}
        seratoFilter={seratoFilter}
        setSeratoFilter={setSeratoFilter}
        syncingSerato={syncingSerato}
        openSeratoSync={openSeratoSync}
        deleteSeratoCrate={deleteSeratoCrate}
        setSeratoModal={setSeratoModal}
        dropTracksOnSerato={dropTracksOnSerato}
        rekordboxCrates={rekordboxCrates}
        preparedRekordbox={preparedRekordbox}
        rekordboxFilter={rekordboxFilter}
        setRekordboxFilter={setRekordboxFilter}
      />

      <div className="main">
        <Toolbar
          query={query}
          setQuery={setQuery}
          selectedIds={selectedIds}
          tracks={tracks}
          setEditing={setEditing}
          analyzerReady={analyzerReady}
          analysis={analysis}
          analyzeMenu={analyzeMenu}
          setAnalyzeMenu={setAnalyzeMenu}
          runAnalysis={runAnalysis}
          copying={copying}
          copyMenu={copyMenu}
          setCopyMenu={setCopyMenu}
          libraries={libraries}
          startCopy={startCopy}
          playing={playing}
          setVizOpen={setVizOpen}
          splitOn={splitOn}
          setSplitOn={setSplitOn}
          splitAvailable={true}
          crateOpen={crateSelected || seratoFilter !== null}
          sortCrate={sortCrate}
          colMenu={colMenu}
          setColMenu={setColMenu}
          hiddenCols={hiddenCols}
          setHiddenCols={setHiddenCols}
          showLibraryColumn={showLibraryColumn}
          seratoFilter={seratoFilter}
          setSeratoFilter={setSeratoFilter}
          rekordboxFilter={rekordboxFilter}
          setRekordboxFilter={setRekordboxFilter}
          removeFromSerato={removeFromSerato}
          activeFolder={activeFolder}
          setActiveFolder={setActiveFolder}
          removeFromCrate={removeFromCrate}
          healthFilter={healthFilter}
          setHealthFilter={setHealthFilter}
          keyFilter={keyFilter}
          setKeyFilter={setKeyFilter}
          total={total}
        />

        {(() => {
          const table = (
        <TrackTable
          activeScanning={activeScanning}
          scanning={scanning}
          fileDropTable={fileDropTable}
          setFileDropTable={setFileDropTable}
          scrollRef={scrollRef}
          dragTracksRef={dragTracksRef}
          foreignDragRef={foreignDragRef}
          onAddAt={addFromOverview}
          dragCol={dragCol}
          activeLibrary={activeLibrary}
          showToast={showToast}
          importOsFiles={importOsFiles}
          activeFolder={activeFolder}
          gridTemplate={gridTemplate}
          columns={tableColumns}
          sortable={!splitCrateView}
          crateSelected={crateSelected}
          dragOver={dragOver}
          setDragOver={setDragOver}
          moveColumn={
            splitActive
              ? (from: string | null, to: string) =>
                  setSplitRightCols((c) => moveCol(c, from, to))
              : moveColumn
          }
          effectiveSort={effectiveSort}
          toggleSort={toggleSort}
          startColResize={startColResize}
          setColWidths={setColWidths}
          tracks={tracks}
          libraries={libraries}
          query={query}
          selectedIds={selectedIds}
          setSelectedIds={setSelectedIds}
          setAnchorIdx={setAnchorIdx}
          playing={playing}
          dropRow={dropRow}
          setDropRow={setDropRow}
          selectRow={selectRow}
          playTrack={playTrack}
          setCtxMenu={setCtxMenu}
          anyManualMode={anyManualMode}
          seratoManualMode={seratoManualMode}
          seratoFilter={seratoFilter}
          keepScrollRef={keepScrollRef}
          setSeratoToken={setSeratoToken}
          refreshTracks={refreshTracks}
          loadMoreTracks={loadMoreTracks}
          moveSelection={moveSelection}
          artToken={artToken}
          libraryName={libraryName}
          setQualityTrack={setQualityTrack}
          setDropLib={setDropLib}
        />
          );
          if (!splitActive) return table;
          const paneProps = {
            fetchSource,
            options: splitOptions,
            inCrate,
            dragTracksRef,
            foreignDragRef,
            dragFromRef,
            refreshKey: paneRefresh,
            colWidths,
            selectedIds,
            playing,
            onPick: pickFromOverview,
            playTrack,
            artToken,
            libraryName,
            setQualityTrack,
            onAdd: (t: SplitSource, ids: number[], before: number) => void addToCrate(t, ids, before),
            onReorder: (t: SplitSource, ids: number[], before: number) =>
              void reorderInPane(t, ids, before),
          };
          return (
            <div
              className="split"
              ref={splitRef}
              style={{
                gridTemplateColumns: middleOn
                  ? `${fracs3[0]}fr 6px ${fracs3[1]}fr 6px ${fracs3[2]}fr`
                  : `${splitFrac}fr 6px ${1 - splitFrac}fr`,
              }}
            >
              <OverviewPane
                {...paneProps}
                source={splitSource}
                setSource={(s) => setSplitSourceKey(sourceKey(s))}
                sort={splitSort}
                setSort={setSplitSort}
                cols={splitLeftCols}
                setCols={setSplitLeftCols}
              />
              <div
                className="split-divider"
                data-tip="Drag to resize"
                onMouseDown={middleOn ? startSplitResize3(0) : startSplitResize}
              />
              {middleOn && (
                <>
                  <OverviewPane
                    {...paneProps}
                    source={middleSource}
                    setSource={(s) => setMiddleSourceKey(sourceKey(s))}
                    sort={middleSort}
                    setSort={setMiddleSort}
                    cols={middleCols}
                    setCols={setMiddleCols}
                    onClose={() => setMiddleOn(false)}
                  />
                  <div
                    className="split-divider"
                    data-tip="Drag to resize"
                    onMouseDown={startSplitResize3(1)}
                  />
                </>
              )}
              <div className="split-pane">
                <PaneHeader
                  title={splitCrateView ? "Running order" : "Tracks"}
                  hint={
                    !splitCrateView
                      ? "the view the sidebar opens"
                      : anyManualMode
                        ? "drag, ▲▼ or Move up/down to reorder"
                        : "clear the search to reorder"
                  }
                  cols={splitRightCols}
                  setCols={setSplitRightCols}
                >
                  <SourcePicker
                    options={splitTargets}
                    value={splitTargetKey}
                    placeholder="All tracks"
                    tip="Choose what this pane shows — the sidebar opens here too"
                    onPick={openSplitTarget}
                  />
                  {!middleOn && (
                    <button
                      className="btn icon pane-add"
                      data-tip="Open a third pane beside this one — e.g. a second crate"
                      onClick={openMiddle}
                    >
                      +
                    </button>
                  )}
                </PaneHeader>
                {table}
              </div>
            </div>
          );
        })()}
      </div>

      <DeckPlayer
        track={playing}
        libraryName={playing ? libraryName(playing.library_id) : null}
        playToken={playToken}
        waveToken={waveToken}
        artToken={artToken}
        onPrev={() => playStep(-1)}
        onNext={() => playStep(1)}
        onEnded={advance}
        shuffle={shuffle}
        repeat={repeat}
        onToggleShuffle={() => setShuffle((s) => !s)}
        onCycleRepeat={() =>
          setRepeat((r) => (r === "off" ? "all" : r === "all" ? "one" : "off"))
        }
        onError={onPlayerError}
        onEdit={() => playing && setEditing(playing)}
        gridMode={gridMode}
        onToggleGrid={() => {
          if (gridMode && !leaveGridEdits()) return;
          setGridMode((m) => !m);
        }}
        onGridDirtyChange={onGridDirtyChange}
        onGridSaved={() => refreshTracks().catch(() => {})}
        onGridError={(m) => showToast(m, true)}
        onNotify={(m) => showToast(m)}
      />

      {deleteAsk && (
        <DeleteConfirm
          items={deleteAsk.items}
          onConfirm={() => answerDelete(true)}
          onCancel={() => answerDelete(false)}
        />
      )}

      {dupesFor && (
        <DuplicatesModal
          askDelete={askDelete}
          libraries={libraries}
          set={dupesFor.set}
          setName={dupesFor.name}
          readOnly={dupesFor.readOnly}
          onRemoveLink={
            dupesFor.seratoCrate
              ? (ids) => {
                  const c = dupesFor.seratoCrate!;
                  return api.seratoRemoveTracks(c.letter, c.file, ids).then(() => {
                    setSeratoToken((t) => t + 1);
                  });
                }
              : undefined
          }
          onPlay={(id) => {
            const t = tracks.find((x) => x.id === id);
            if (t) playTrack(t);
          }}
          onDeleted={(ids) => {
            // Same deck handling as App's own delete path: if the loaded track
            // was submitted for deletion, unload it. refreshLibraries reloads
            // library rows and per-library crate trees, where the sidebar's
            // track counts live (health re-runs via the effect watching `tracks`).
            setPlaying((cur) => (cur && ids.includes(cur.id) ? null : cur));
            refreshLibraries().catch(() => {});
            refreshTracks().catch(() => {});
          }}
          onClose={() => {
            setDupesFor(null);
            refreshHealth().catch(() => {});
          }}
          onError={(m) => showToast(m, true)}
          onNotify={(m) => showToast(m)}
        />
      )}

      {newLibParent && (
        <ModalBackdrop onClose={() => setNewLibParent(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-title">Create new library folder</div>
            <div className="modal-sub">in {newLibParent}</div>
            <label className="field">
              <span>Folder name</span>
              <input
                autoFocus
                value={newLibName}
                onChange={(e) => setNewLibName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") createNewLibrary();
                  if (e.key === "Escape") setNewLibParent(null);
                }}
                placeholder="e.g. Warmup Set"
                spellCheck={false}
              />
            </label>
            <div className="modal-actions">
              <button className="btn" onClick={() => setNewLibParent(null)}>
                Cancel
              </button>
              <button
                className="btn primary"
                disabled={newLibName.trim() === ""}
                onClick={createNewLibrary}
              >
                Create
              </button>
            </div>
          </div>
        </ModalBackdrop>
      )}

      {crateModal && (
        <ModalBackdrop onClose={() => setCrateModal(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-title">
              {crateModal.mode === "create" ? "New crate" : "Rename crate"}
            </div>
            <div className="modal-sub">
              {crateModal.mode === "create"
                ? crateModal.parentPath === ""
                  ? `in ${libraryName(crateModal.libraryId)}`
                  : `inside ${crateModal.parentPath}`
                : crateModal.path}
              {" — a real folder "}
              {crateModal.mode === "create" ? "is created" : "is renamed"}
            </div>
            <label className="field">
              <span>Crate name</span>
              <input
                autoFocus
                value={crateName}
                onChange={(e) => setCrateName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") submitCrateModal();
                  if (e.key === "Escape") setCrateModal(null);
                }}
                placeholder="e.g. Peak Time"
                spellCheck={false}
              />
            </label>
            <div className="modal-actions">
              <button className="btn" onClick={() => setCrateModal(null)}>
                Cancel
              </button>
              <button
                className="btn primary"
                disabled={crateName.trim() === ""}
                onClick={submitCrateModal}
              >
                {crateModal.mode === "create" ? "Create" : "Rename"}
              </button>
            </div>
          </div>
        </ModalBackdrop>
      )}

      {seratoModal && (
        <ModalBackdrop onClose={() => setSeratoModal(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-title">
              {seratoModal.mode === "create" ? "New Serato crate" : "Rename Serato crate"}
            </div>
            <div className="modal-sub">
              on {seratoModal.letter} — writes only the .crate file, backed up first. Close Serato before editing!
            </div>
            <label className="field">
              <span>Crate name</span>
              <input
                autoFocus
                value={crateName}
                onChange={(e) => setCrateName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") submitSeratoModal();
                  if (e.key === "Escape") setSeratoModal(null);
                }}
                placeholder="e.g. Warmup"
                spellCheck={false}
              />
            </label>
            <div className="modal-actions">
              <button className="btn" onClick={() => setSeratoModal(null)}>
                Cancel
              </button>
              <button
                className="btn primary"
                disabled={crateName.trim() === ""}
                onClick={submitSeratoModal}
              >
                {seratoModal.mode === "create" ? "Create" : "Rename"}
              </button>
            </div>
          </div>
        </ModalBackdrop>
      )}

      {seratoSync && (
        <SeratoSyncModal
          letter={seratoSync.letter}
          file={seratoSync.file}
          name={seratoSync.name}
          plan={seratoSync.plan}
          onClose={() => setSeratoSync(null)}
          onApplied={(res) => {
            setSeratoSync(null);
            setSeratoToken((t) => t + 1);
            if (res.added > 0 || res.removed > 0) {
              showToast(`Synced “${seratoSync.name}” — added ${res.added}, removed ${res.removed}`);
            }
          }}
        />
      )}

      {editing && (
        <MetadataEditor
          track={editing}
          seratoHint={(() => {
            // Track lives on a connected stick that has Serato crates →
            // remind that Serato shows its own cached tags, not the file's.
            const lib = libraries.find((l) => l.id === editing.library_id);
            if (!lib?.volume_serial) return false;
            const dev = devices.find((d) => d.serial === lib.volume_serial);
            return dev != null && (seratoCrates.get(dev.letter) ?? []).length > 0;
          })()}
          onClose={() => setEditing(null)}
          onSaved={() => {
            showToast("Tags saved to file");
            refreshTracks().catch(() => {});
          }}
          onArtworkChanged={() => {
            showToast("Artwork saved to file");
            setArtToken((n) => n + 1);
            refreshTracks().catch(() => {});
          }}
          onError={(m) => showToast(m, true)}
        />
      )}

      {qualityTrack && (
        <QualityPanel
          track={qualityTrack}
          apolloOk={apolloOk}
          onClose={() => setQualityTrack(null)}
          onEnhance={(trackId) => {
            setQualityTrack(null);
            setEnhanceDialog([trackId]);
          }}
          onRestore={(trackId) => {
            setQualityTrack(null);
            setMlRestoreDialog([trackId]);
          }}
          onStabilize={(trackId) => {
            setQualityTrack(null);
            setTempoDialog([trackId]);
          }}
        />
      )}

      {tempoDialog && (
        <TempoDialog
          trackIds={tempoDialog}
          suggestedBpm={tracks.find((t) => t.id === tempoDialog[0])?.bpm ?? null}
          drift={tracks.find((t) => t.id === tempoDialog[0])?.tempo_drift_pct ?? null}
          seratoDevice={anyOnSeratoDevice(tempoDialog)}
          onStart={startStabilize}
          onClose={() => setTempoDialog(null)}
        />
      )}
      {rbxExport && (
        <RekordboxExportDialog
          letter={rbxExport.letter}
          label={rbxExport.label}
          crate={rbxExport.crate}
          onClose={() => setRbxExport(null)}
        />
      )}
      {rbxImport && (
        <RekordboxImportDialog
          letter={rbxImport.letter}
          label={rbxImport.label}
          onStarted={() => setRbImporting({ done: 0, total: 0 })}
          onStartFailed={() => setRbImporting(null)}
          onClose={() => setRbxImport(null)}
        />
      )}
      {enhanceDialog && (
        <EnhanceDialog
          trackIds={enhanceDialog}
          seratoDevice={anyOnSeratoDevice(enhanceDialog)}
          onStart={startEnhance}
          onClose={() => setEnhanceDialog(null)}
        />
      )}

      {mlRestoreDialog && (
        <RestoreDialog
          trackIds={mlRestoreDialog}
          hints={restoreHints(tracks.filter((t) => mlRestoreDialog.includes(t.id)))}
          seratoDevice={anyOnSeratoDevice(mlRestoreDialog)}
          onStart={startMlRestore}
          onClose={() => setMlRestoreDialog(null)}
        />
      )}

      {batchEditing && (
        <BatchMetadataEditor
          tracks={batchEditing}
          onClose={() => setBatchEditing(null)}
          onApply={startBatch}
          onError={(m) => showToast(m, true)}
        />
      )}

      {comparing && (
        <LibraryCompare
          source={comparing.set}
          sourceName={comparing.name}
          onClose={() => setComparing(null)}
          onError={(m) => showToast(m, true)}
        />
      )}

      {vizOpen && <Visualizer track={playing} onClose={() => setVizOpen(false)} />}

      {ctxMenu && (() => {
        const one =
          selectedIds.size === 1
            ? tracks.find((t) => selectedIds.has(t.id))
            : undefined;
        const inCrate = seratoFilter !== null || (activeLibrary !== null && activeFolder !== null && activeFolder !== "");
        const selectedTracks = tracks.filter((t) => selectedIds.has(t.id));
        const items: CtxItem[] = [
          { label: "Play", disabled: !one, action: one ? () => playTrack(one) : undefined },
          ...(anyManualMode
            ? [
                {
                  label: "Move up",
                  disabled: selectedTracks.length === 0,
                  action: () => moveSelection(-1),
                } as CtxItem,
                {
                  label: "Move down",
                  disabled: selectedTracks.length === 0,
                  action: () => moveSelection(1),
                } as CtxItem,
              ]
            : []),
          {
            label: selectedTracks.length > 1 ? `Edit metadata (${selectedTracks.length})` : "Edit metadata",
            disabled: selectedTracks.length === 0,
            action:
              selectedTracks.length === 0
                ? undefined
                : () =>
                    selectedTracks.length === 1
                      ? setEditing(selectedTracks[0])
                      : setBatchEditing(selectedTracks),
          },
          {
            label: "Quality details",
            disabled: !one,
            action: one ? () => setQualityTrack(one) : undefined,
          },
          {
            label: selectedTracks.length > 1 ? `Enhance… (${selectedTracks.length})` : "Enhance…",
            disabled: selectedTracks.length === 0,
            action:
              selectedTracks.length === 0
                ? undefined
                : () => setEnhanceDialog(selectedTracks.map((t) => t.id)),
          },
          {
            label:
              selectedTracks.length > 1
                ? `Steady tempo… (${selectedTracks.length})`
                : "Steady tempo…",
            disabled: selectedTracks.length === 0,
            action:
              selectedTracks.length === 0
                ? undefined
                : () => setTempoDialog(selectedTracks.map((t) => t.id)),
          },
          {
            label:
              selectedTracks.length > 1
                ? `Regenerate Serato grid (${selectedTracks.length})`
                : "Regenerate Serato grid",
            disabled: selectedTracks.length === 0,
            action:
              selectedTracks.length === 0
                ? undefined
                : () => {
                    // a grid set by hand in the deck is only replaced on request
                    const manual = selectedTracks.filter((t) => t.serato_grid_source === "manual");
                    let ids = selectedTracks.map((t) => t.id);
                    let rewrite = false;
                    if (manual.length > 0) {
                      if (
                        window.confirm(
                          `${manual.length} track${manual.length === 1 ? " has" : "s have"} a grid ` +
                            "you set by hand. Replace with SpinDex's automatic grid?\n\n" +
                            "OK = replace · Cancel = keep the hand-set grids",
                        )
                      ) {
                        rewrite = true;
                      } else {
                        const keep = new Set(manual.map((t) => t.id));
                        ids = ids.filter((id) => !keep.has(id));
                        if (ids.length === 0) return;
                      }
                    }
                    regenGrid(
                      ids.length === 1
                        ? (selectedTracks.find((t) => t.id === ids[0])?.title ?? "track")
                        : `${ids.length} tracks`,
                      (rw) => api.regenerateSeratoGrid(ids, rw || rewrite),
                    );
                  },
          },
          ...(apolloOk
            ? [
                {
                  label:
                    selectedTracks.length > 1
                      ? `Restore quality (ML)… (${selectedTracks.length})`
                      : "Restore quality (ML)…",
                  disabled: selectedTracks.length === 0,
                  action:
                    selectedTracks.length === 0
                      ? undefined
                      : () => setMlRestoreDialog(selectedTracks.map((t) => t.id)),
                } as CtxItem,
              ]
            : []),
          ...(selectedTracks.length > 0 && selectedTracks.every((t) => t.enhanced_at != null)
            ? [
                {
                  label:
                    selectedTracks.length > 1
                      ? `Restore original (${selectedTracks.length})`
                      : "Restore original",
                  action: restoreSelected,
                } as CtxItem,
              ]
            : []),
          {
            label: "Copy to",
            disabled: libraries.length === 0,
            submenu: libraries.map((lib) => ({
              key: lib.id,
              label: lib.name,
              hint: lib.track_count.toLocaleString(),
              action: () => startCopy(lib.id),
            })),
          },
          {
            label: "Show in Explorer",
            disabled: !one,
            action: one
              ? () => api.showInExplorer(one.id).catch((e) => showToast(String(e), true))
              : undefined,
          },
          {
            label: "Remove from crate",
            disabled: !inCrate,
            action: inCrate
              ? () => (seratoFilter ? removeFromSerato() : removeFromCrate())
              : undefined,
          },
          { label: "Delete (Recycle Bin)", danger: true, action: deleteSelected },
        ];
        return (
          <TrackContextMenu
            x={ctxMenu.x}
            y={ctxMenu.y}
            items={items}
            onClose={() => setCtxMenu(null)}
          />
        );
      })()}

      {crateCopyMenu && (() => {
        const others = libraries.filter((l) => l.id !== crateCopyMenu.libraryId);
        const items: CtxItem[] =
          others.length > 0
            ? others.map((l) => ({
                label: l.name,
                action: () => {
                  const src = libraries.find((s) => s.id === crateCopyMenu.libraryId);
                  if (src) copyCrateTo(src.id, crateCopyMenu.path, l);
                },
              }))
            : [{ label: "No other library", disabled: true }];
        return (
          <TrackContextMenu
            x={crateCopyMenu.x}
            y={crateCopyMenu.y}
            items={items}
            onClose={() => setCrateCopyMenu(null)}
          />
        );
      })()}

      {toast && <div className={`toast ${toast.error ? "error" : ""}`}>{toast.text}</div>}
      <HoverTip />
    </div>
  );
}
