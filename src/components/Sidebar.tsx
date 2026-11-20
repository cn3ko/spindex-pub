import { Fragment, type CSSProperties, type ReactNode } from "react";
import * as api from "../api";

// Depth cap for renderRekordboxNodes — see the comment at its definition.
// A real Rekordbox playlist tree is a handful of levels deep at most; this
// is insurance against a malformed export.pdb, not a real limit.
const REKORDBOX_MAX_DEPTH = 20;

interface SidebarProps {
  libraries: api.LibraryInfo[];
  totalTracks: number;
  activeLibrary: number | null;
  setActiveLibrary: (id: number | null) => void;
  activeFolder: string | null;
  setActiveFolder: (path: string | null) => void;
  scanning: api.ScanProgress | null;
  rescan: (id: number) => void;
  devices: api.DeviceInfo[];
  devicesLoaded: boolean;
  crates: Map<number, api.CrateFolder[]>;
  openCrates: Set<number>;
  setOpenCrates: React.Dispatch<React.SetStateAction<Set<number>>>;
  collapsedCrates: Set<string>;
  setCollapsedCrates: React.Dispatch<React.SetStateAction<Set<string>>>;
  dropLib: number | null;
  setDropLib: React.Dispatch<React.SetStateAction<number | null>>;
  dropCrate: string | null;
  setDropCrate: React.Dispatch<React.SetStateAction<string | null>>;
  dragTracksRef: React.MutableRefObject<number[] | null>;
  startCopy: (targetId: number, trackIds?: number[], destFolder?: string) => void;
  importOsFiles: (libraryId: number, folder: string, files: FileList) => void;
  setComparing: (v: { set: api.TrackSetRef; name: string } | null) => void;
  removeLib: (lib: api.LibraryInfo) => void;
  setCrateName: (name: string) => void;
  setCrateModal: React.Dispatch<
    React.SetStateAction<
      | { libraryId: number; mode: "create"; parentPath: string }
      | { libraryId: number; mode: "rename"; path: string }
      | null
    >
  >;
  setCrateCopyMenu: (
    v: { libraryId: number; path: string; x: number; y: number } | null,
  ) => void;
  runTagRepair: (label: string, start: () => Promise<number>) => void;
  regenGrid: (
    label: string,
    start: (rewriteAll: boolean) => Promise<number>,
  ) => void;
  dropTracksOnCrate: (lib: api.LibraryInfo, folderPath: string, ids: number[]) => void;
  deleteCrate: (libraryId: number, path: string) => void;
  setSort: (s: { key: string; desc: boolean } | null) => void;
  ejectStick: (letter: string, label: string) => void;
  exportToRekordbox: (letter: string, label: string, crate?: { file: string; name: string }) => void;
  importFromRekordbox: (letter: string, label: string) => void;
  rbImporting: api.RekordboxImportProgress | null;
  forgetStick: (serial: string, label: string, libIds: number[]) => void;
  health: api.HealthCounts | null;
  healthFilter: api.HealthFilter | null;
  setHealthFilter: React.Dispatch<React.SetStateAction<api.HealthFilter | null>>;
  healthCollapsed: boolean;
  setHealthCollapsed: React.Dispatch<React.SetStateAction<boolean>>;
  setDupesFor: (v: {
    set: api.TrackSetRef;
    name: string;
    readOnly?: boolean;
    seratoCrate?: { letter: string; file: string };
  }) => void;
  showToast: (text: string, error?: boolean) => void;
  analysis: api.AnalysisProgress | null;
  copying: api.CopyProgress | null;
  batch: api.BatchProgress | null;
  syncing: api.SyncProgress | null;
  enhancing: api.EnhanceProgress | null;
  stabilizing: api.EnhanceProgress | null;
  mlRestoreRun: api.MlRestoreProgress | null;
  addMenu: boolean;
  setAddMenu: React.Dispatch<React.SetStateAction<boolean>>;
  addLibrary: () => void;
  pickNewLibraryParent: () => void;
  refreshLibraries: () => Promise<void>;
  refreshDevices: () => Promise<void>;
  refreshTracks: () => Promise<void>;
  setScanning: (s: api.ScanProgress | null) => void;
  seratoCrates: Map<string, api.SeratoCrateInfo[]>;
  seratoFilter: { letter: string; file: string; name: string; ids: number[] } | null;
  setSeratoFilter: (
    v: { letter: string; file: string; name: string; ids: number[] } | null,
  ) => void;
  syncingSerato: string | null;
  openSeratoSync: (letter: string, crate: api.SeratoCrateInfo) => void;
  deleteSeratoCrate: (letter: string, crate: api.SeratoCrateInfo) => void;
  setSeratoModal: (
    v: { letter: string; mode: "create" } | { letter: string; mode: "rename"; file: string } | null,
  ) => void;
  dropTracksOnSerato: (letter: string, crate: api.SeratoCrateInfo, ids: number[]) => void;
  rekordboxCrates: Map<string, api.RekordboxCrateInfo[]>;
  /** playlists of the XML exported to the stick, ids negated (see App.tsx) */
  preparedRekordbox: Map<string, api.RekordboxCrateInfo[]>;
  rekordboxFilter: { letter: string; id: number; name: string; ids: number[] } | null;
  setRekordboxFilter: (
    v: { letter: string; id: number; name: string; ids: number[] } | null,
  ) => void;
}

// Sidebar: library/device tree with folder-crates, Serato crates,
// Rekordbox crates (read-only), library health counts and the
// analysis/copy/batch/enhance/stabilize/restore progress pills.
//
// This is presentational — every mutation (create/rename/delete crate,
// copy, drop handling, etc.) is a callback prop owned by App.tsx, which is
// also where all the corresponding state lives. Keeping the handlers in
// App.tsx (rather than duplicating them here) is deliberate: several of
// them are shared with the toolbar/table (e.g. startCopy, runTagRepair).
export default function Sidebar(props: SidebarProps) {
  const {
    libraries,
    totalTracks,
    activeLibrary,
    setActiveLibrary,
    activeFolder,
    setActiveFolder,
    scanning,
    rescan,
    devices,
    devicesLoaded,
    crates,
    openCrates,
    setOpenCrates,
    collapsedCrates,
    setCollapsedCrates,
    dropLib,
    setDropLib,
    dropCrate,
    setDropCrate,
    dragTracksRef,
    startCopy,
    importOsFiles,
    setComparing,
    removeLib,
    setCrateName,
    setCrateModal,
    setCrateCopyMenu,
    runTagRepair,
    regenGrid,
    dropTracksOnCrate,
    deleteCrate,
    setSort,
    ejectStick,
    exportToRekordbox,
    importFromRekordbox,
    rbImporting,
    forgetStick,
    health,
    healthFilter,
    setHealthFilter,
    healthCollapsed,
    setHealthCollapsed,
    setDupesFor,
    showToast,
    analysis,
    copying,
    batch,
    syncing,
    enhancing,
    stabilizing,
    mlRestoreRun,
    addMenu,
    setAddMenu,
    addLibrary,
    pickNewLibraryParent,
    refreshLibraries,
    refreshDevices,
    refreshTracks,
    setScanning,
    seratoCrates,
    seratoFilter,
    setSeratoFilter,
    syncingSerato,
    openSeratoSync,
    deleteSeratoCrate,
    setSeratoModal,
    dropTracksOnSerato,
    rekordboxCrates,
    preparedRekordbox,
    rekordboxFilter,
    setRekordboxFilter,
  } = props;

  // Read-only Rekordbox playlist/folder tree for one connected drive letter.
  // The command does not sort its output (per api.ts's listRekordboxCrates
  // doc comment) — the tree comes purely from grouping by parent_id, id 0
  // being the export's root. Folder nodes are unclickable headers; playlist
  // nodes filter the table via the same explicit-id-set mechanism as
  // seratoFilter. No editing, no drag targets — SP1 is display-only.
  //
  // `parse_playlist_tree` (Task 4/5) reads id/parent_id/is_folder at byte
  // offsets inferred from one fixture and skips short/malformed ROWS rather
  // than failing the whole table — it does not validate the resulting GRAPH.
  // A differently-versioned or corrupted export.pdb could hand this function
  // a parent_id chain that cycles back on itself (e.g. two folders each
  // listing the other as parent), which would recurse forever and crash the
  // render with no error boundary. `ancestors` carries the folder ids on the
  // current path from the root; a node whose id is already an ancestor is a
  // cycle and is skipped rather than recursed into. `REKORDBOX_MAX_DEPTH` is
  // a second, independent cap against a long-but-acyclic malformed chain.
  const renderRekordboxNodes = (
    letter: string,
    nodes: api.RekordboxCrateInfo[],
    parentId: number,
    depth: number,
    ancestors: ReadonlySet<number> = new Set(),
  ): ReactNode[] => {
    if (depth > REKORDBOX_MAX_DEPTH) return [];
    return nodes
      .filter((n) => n.parent_id === parentId)
      .flatMap((n) => {
        if (ancestors.has(n.id)) return []; // cycle guard — bail, don't recurse
        // prepared (exported-XML) playlists carry negated ids, see App.tsx;
        // compare/dupes resolve against export.pdb, so they get no actions
        const prepared = n.id < 0;
        const children = n.is_folder
          ? renderRekordboxNodes(letter, nodes, n.id, depth + 1, new Set(ancestors).add(n.id))
          : [];
        if (n.is_folder) {
          return [
            <div
              key={n.id}
              className="side-item crate rekordbox folder"
              style={{ paddingLeft: 30 + depth * 14 }}
              data-tip={n.name}
            >
              <svg viewBox="0 0 24 24">
                <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
              </svg>
              {n.name}
            </div>,
            ...children,
          ];
        }
        return [
          <div
            key={n.id}
            className={`side-item crate rekordbox${prepared ? " prepared" : ""} ${
              rekordboxFilter?.letter === letter && rekordboxFilter?.id === n.id
                ? "active"
                : ""
            }`}
            style={{ paddingLeft: 30 + depth * 14 }}
            onClick={() => {
              setActiveLibrary(null);
              setActiveFolder(null);
              setSeratoFilter(null);
              setSort(null);
              setRekordboxFilter({
                letter,
                id: n.id,
                name: n.name,
                ids: n.track_ids,
              });
            }}
            data-tip={
              (prepared
                ? "Prepared for Rekordbox: in SpinDex\\rekordbox.xml on this stick, not yet in Rekordbox.\nImport it in Rekordbox, then export to the stick."
                : "Rekordbox playlist (read-only)") +
              (n.missing > 0
                ? `\n${n.missing} entr${n.missing === 1 ? "y" : "ies"} not in the index`
                : "")
            }
          >
            <svg viewBox="0 0 24 24">
              <path d="M4 6h10M4 12h10M4 18h6" />
              <circle cx="18" cy="17" r="3" />
              <path d="M21 17V6l-3 1" />
            </svg>
            {n.name} <span className="count">{n.track_ids.length.toLocaleString()}</span>
            {!prepared && (
            <span className="side-actions">
              <button
                className="side-compare"
                data-tip="Compare with libraries, crates and playlists"
                onClick={(e) => {
                  e.stopPropagation();
                  setComparing({ set: { kind: "rekordbox", letter, playlist_id: n.id }, name: n.name });
                }}
              >
                <svg viewBox="0 0 24 24">
                  <circle cx="9" cy="12" r="5.5" />
                  <circle cx="15" cy="12" r="5.5" />
                </svg>
              </button>
              <button
                className="side-compare"
                data-tip="Find duplicates in here"
                onClick={(e) => {
                  e.stopPropagation();
                  setDupesFor({ set: { kind: "rekordbox", letter, playlist_id: n.id }, name: n.name, readOnly: true });
                }}
              >
                <svg viewBox="0 0 24 24">
                  <path d="M8 8h10v10H8z" />
                  <path d="M5 5h10v2H7v8H5z" />
                </svg>
              </button>
            </span>
            )}
          </div>,
        ];
      });
  };

  const renderLibraryRow = (lib: api.LibraryInfo) => {
    const crateList = crates.get(lib.id) ?? [];
    const open = openCrates.has(lib.id);
    return (
      <Fragment key={lib.id}>
        <div
          className={`side-item ${
            activeLibrary === lib.id && activeFolder === null ? "active" : ""
          } ${dropLib === lib.id ? "drop-target" : ""}`}
          onClick={() => {
            setActiveLibrary(lib.id);
            setActiveFolder(null);
            setSeratoFilter(null);
            setRekordboxFilter(null);
          }}
          onDoubleClick={() => rescan(lib.id)}
          data-tip={`${lib.root_path}\nDouble-click to rescan · drop tracks or files to copy`}
          onDragOver={(e) => {
            if (!dragTracksRef.current && !e.dataTransfer.types.includes("Files")) return;
            e.preventDefault();
            e.dataTransfer.dropEffect = "copy";
            setDropLib(lib.id);
          }}
          onDragLeave={() => setDropLib((cur) => (cur === lib.id ? null : cur))}
          onDrop={(e) => {
            e.preventDefault();
            const ids = dragTracksRef.current;
            dragTracksRef.current = null;
            setDropLib(null);
            if (ids && ids.length > 0) startCopy(lib.id, ids);
            else if (e.dataTransfer.files.length > 0)
              importOsFiles(lib.id, "", e.dataTransfer.files);
          }}
        >
          {crateList.length > 0 ? (
            <button
              className={`crate-chevron ${open ? "open" : ""}`}
              data-tip={open ? "Collapse crates" : "Expand crates"}
              onClick={(e) => {
                e.stopPropagation();
                setOpenCrates((prev) => {
                  const next = new Set(prev);
                  if (next.has(lib.id)) next.delete(lib.id);
                  else next.add(lib.id);
                  return next;
                });
              }}
            >
              <svg viewBox="0 0 24 24">
                <path d="M9 6l6 6-6 6" />
              </svg>
            </button>
          ) : (
            <span className="crate-chevron spacer" />
          )}
          <svg viewBox="0 0 24 24">
            <rect x="3" y="5" width="18" height="14" rx="2" />
          </svg>
          {lib.is_removable && (
            <span
              className={`usb-dot ${
                devices.some((d) => d.known_library_id === lib.id) ? "on" : "off"
              }`}
              data-tip={
                devices.some((d) => d.known_library_id === lib.id)
                  ? "USB device connected"
                  : "USB device not connected — tracks marked missing"
              }
            />
          )}
          {lib.name} <span className="count">{lib.track_count.toLocaleString()}</span>
          <span className="side-actions">
            <button
              className="side-compare"
              data-tip="New crate (creates a folder)"
              onClick={(e) => {
                e.stopPropagation();
                setCrateName("");
                setCrateModal({ libraryId: lib.id, mode: "create", parentPath: "" });
              }}
            >
              <svg viewBox="0 0 24 24">
                <path d="M12 6v12M6 12h12" />
              </svg>
            </button>
            <button
              className={`side-compare${scanning?.library_id === lib.id ? " syncing" : ""}`}
              data-tip="Rescan folder"
              onClick={(e) => {
                e.stopPropagation();
                rescan(lib.id);
              }}
            >
              <svg viewBox="0 0 24 24">
                <path d="M20 12a8 8 0 1 1-2.3-5.6" />
                <path d="M20 4v4h-4" />
              </svg>
            </button>
            {(
              <button
                className="side-compare"
                data-tip="Compare with other libraries and crates"
                onClick={(e) => {
                  e.stopPropagation();
                  setComparing({ set: { kind: "library", id: lib.id }, name: lib.name });
                }}
              >
                <svg viewBox="0 0 24 24">
                  <circle cx="9" cy="12" r="5.5" />
                  <circle cx="15" cy="12" r="5.5" />
                </svg>
              </button>
            )}
            <button
              className="side-compare"
              data-tip={"Remove from SpinDex\nOnly the index — files stay untouched"}
              onClick={(e) => {
                e.stopPropagation();
                removeLib(lib);
              }}
            >
              <svg viewBox="0 0 24 24">
                <path d="M6 6l12 12M18 6L6 18" />
              </svg>
            </button>
          </span>
        </div>
        {open &&
          crateList.map((c) => {
            const dropKey = `${lib.id}:${c.path}`;
            const hasChildren = crateList.some((o) => o.path.startsWith(c.path + "/"));
            const isEmpty = c.track_count === 0 && !hasChildren;
            const collapsed = collapsedCrates.has(dropKey);
            // hidden while any ancestor folder is collapsed
            let ancestor = c.path;
            while (ancestor.includes("/")) {
              ancestor = ancestor.slice(0, ancestor.lastIndexOf("/"));
              if (collapsedCrates.has(`${lib.id}:${ancestor}`)) return null;
            }
            return (
              <div
                key={c.path}
                className={`side-item crate ${
                  activeLibrary === lib.id && activeFolder === c.path ? "active" : ""
                } ${dropCrate === dropKey ? "drop-target" : ""}`}
                style={{ paddingLeft: 30 + c.depth * 14 }}
                onClick={() => {
                  setActiveLibrary(lib.id);
                  setActiveFolder(c.path);
                  setSeratoFilter(null);
                  setRekordboxFilter(null);
                  // show the crate in its hand-made order, not whatever sort was
                  // active in the previous view
                  setSort({ key: "num", desc: false });
                }}
                data-tip={
                  (c.path.includes("/") ? c.path + "\n" : "") +
                  "Drop tracks or files to file them here"
                }
                onDragOver={(e) => {
                  if (!dragTracksRef.current && !e.dataTransfer.types.includes("Files")) return;
                  e.preventDefault();
                  e.dataTransfer.dropEffect = "copy";
                  setDropCrate(dropKey);
                }}
                onDragLeave={() => setDropCrate((cur) => (cur === dropKey ? null : cur))}
                onDrop={(e) => {
                  e.preventDefault();
                  const ids = dragTracksRef.current;
                  dragTracksRef.current = null;
                  setDropCrate(null);
                  if (ids && ids.length > 0) dropTracksOnCrate(lib, c.path, ids);
                  else if (e.dataTransfer.files.length > 0)
                    importOsFiles(lib.id, c.path, e.dataTransfer.files);
                }}
              >
                {hasChildren ? (
                  <button
                    className={`crate-chevron ${collapsed ? "" : "open"}`}
                    data-tip={collapsed ? "Expand" : "Collapse"}
                    onClick={(e) => {
                      e.stopPropagation();
                      setCollapsedCrates((prev) => {
                        const next = new Set(prev);
                        if (next.has(dropKey)) next.delete(dropKey);
                        else next.add(dropKey);
                        return next;
                      });
                    }}
                  >
                    <svg viewBox="0 0 24 24">
                      <path d="M9 6l6 6-6 6" />
                    </svg>
                  </button>
                ) : (
                  <span className="crate-chevron spacer" />
                )}
                <svg viewBox="0 0 24 24">
                  <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
                </svg>
                {c.name}{" "}
                <span className="count">
                  {c.track_count > 0 ? c.track_count.toLocaleString() : ""}
                </span>
                <span className="side-actions">
                  <button
                    className="side-compare"
                    data-tip="Compare with libraries, crates and playlists"
                    onClick={(e) => {
                      e.stopPropagation();
                      setComparing({ set: { kind: "folder", library_id: lib.id, path: c.path }, name: `${lib.name} / ${c.path}` });
                    }}
                  >
                    <svg viewBox="0 0 24 24">
                      <circle cx="9" cy="12" r="5.5" />
                      <circle cx="15" cy="12" r="5.5" />
                    </svg>
                  </button>
                  <button
                    className="side-compare"
                    data-tip="Find duplicates in here"
                    onClick={(e) => {
                      e.stopPropagation();
                      setDupesFor({ set: { kind: "folder", library_id: lib.id, path: c.path }, name: `${lib.name} / ${c.path}` });
                    }}
                  >
                    <svg viewBox="0 0 24 24">
                      <path d="M8 8h10v10H8z" />
                      <path d="M5 5h10v2H7v8H5z" />
                    </svg>
                  </button>
                  <button
                    className="side-compare"
                    data-tip="New crate inside"
                    onClick={(e) => {
                      e.stopPropagation();
                      setCrateName("");
                      setCrateModal({ libraryId: lib.id, mode: "create", parentPath: c.path });
                    }}
                  >
                    <svg viewBox="0 0 24 24">
                      <path d="M12 6v12M6 12h12" />
                    </svg>
                  </button>
                  <button
                    className="side-compare"
                    data-tip={"Copy crate to another library\nIncl. subcrates · never overwrites"}
                    onClick={(e) => {
                      e.stopPropagation();
                      setCrateCopyMenu({
                        libraryId: lib.id,
                        path: c.path,
                        x: e.clientX,
                        y: e.clientY,
                      });
                    }}
                  >
                    <svg viewBox="0 0 24 24">
                      <rect x="8" y="8" width="12" height="12" rx="2" />
                      <path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" />
                    </svg>
                  </button>
                  <button
                    className="side-compare"
                    data-tip={"Update tags from SpinDex\nWrites SpinDex's metadata into the files"}
                    onClick={(e) => {
                      e.stopPropagation();
                      runTagRepair(c.name, () => api.repairFolderCrateTags(lib.id, c.path));
                    }}
                  >
                    <svg viewBox="0 0 24 24">
                      <path d="M20 12a8 8 0 1 1-2.3-5.6" />
                      <path d="M20 4v5h-5" />
                    </svg>
                  </button>
                  <button
                    className="side-compare"
                    data-tip={"Regenerate Serato grid\nWrites a beat grid from SpinDex's analysis"}
                    onClick={(e) => {
                      e.stopPropagation();
                      regenGrid(c.name, (rw) =>
                        api.regenerateFolderCrateGrids(lib.id, c.path, rw),
                      );
                    }}
                  >
                    <svg viewBox="0 0 24 24">
                      <path d="M4 6h16M4 12h16M4 18h16M8 3v18M16 3v18" />
                    </svg>
                  </button>
                  <button
                    className="side-compare"
                    data-tip="Rename crate (renames the folder)"
                    onClick={(e) => {
                      e.stopPropagation();
                      setCrateName(c.name);
                      setCrateModal({ libraryId: lib.id, mode: "rename", path: c.path });
                    }}
                  >
                    <svg viewBox="0 0 24 24">
                      <path d="M4 20l4-1L19 8l-3-3L5 16zM14 7l3 3" />
                    </svg>
                  </button>
                  {isEmpty && (
                    <button
                      className="side-compare"
                      data-tip="Delete empty crate"
                      onClick={(e) => {
                        e.stopPropagation();
                        deleteCrate(lib.id, c.path);
                      }}
                    >
                      <svg viewBox="0 0 24 24">
                        <path d="M6 6l12 12M18 6L6 18" />
                      </svg>
                    </button>
                  )}
                </span>
              </div>
            );
          })}
      </Fragment>
    );
  };

  return (
    <div className="sidebar">
      <div className="side-label">Libraries</div>
      <div
        className={`side-item ${activeLibrary === null && !seratoFilter && !rekordboxFilter ? "active" : ""}`}
        onClick={() => {
          setActiveLibrary(null);
          setActiveFolder(null);
          setSeratoFilter(null);
          setRekordboxFilter(null);
        }}
      >
        <svg viewBox="0 0 24 24">
          <path d="M4 6h16M4 12h16M4 18h16" />
        </svg>
        All tracks <span className="count">{totalTracks.toLocaleString()}</span>
      </div>
      {libraries.filter((l) => l.volume_serial === null).map(renderLibraryRow)}
      {/* devices: libraries grouped by stick */}
      {Array.from(
        libraries
          .filter((l) => l.volume_serial !== null)
          .reduce((m, l) => {
            const list = m.get(l.volume_serial!) ?? [];
            list.push(l);
            m.set(l.volume_serial!, list);
            return m;
          }, new Map<string, api.LibraryInfo[]>()),
      ).map(([serial, libs]) => {
        const dev = devices.find(
          (d) => d.known_library_id !== null && libs.some((l) => l.id === d.known_library_id),
        );
        const connected = dev !== undefined;
        // Header shows the stick's volume label; an unlabeled stick would
        // otherwise surface its drive letter ("D:") here — show "USB" instead.
        const derived = libs[0].name.replace(/^.*\(|\)$/g, "").trim();
        const generic = (s: string) => !s || /^[A-Za-z]:$/.test(s) || s === "Removable drive";
        const label = dev && !generic(dev.label) ? dev.label : generic(derived) ? "USB" : derived;
        return (
          <div key={serial} className="device-group">
            <div className="device-group-head">
              <span className={`usb-dot ${connected ? "on" : "off"}`} />
              {label}
              <span className="side-actions">
                {dev && (
                  <button
                    className="side-compare"
                    data-tip="Export to Rekordbox (XML)"
                    onClick={() => exportToRekordbox(dev.letter, label)}
                  >
                    <svg viewBox="0 0 24 24">
                      <path d="M12 4v11M7 10l5 5 5-5" />
                      <path d="M5 19h14" />
                    </svg>
                  </button>
                )}
                {dev && (
                  <button
                    className="side-compare"
                    data-tip={"Import from Rekordbox (XML)\nGrids, cues, loops and playlists into Serato"}
                    onClick={() => importFromRekordbox(dev.letter, label)}
                  >
                    <svg viewBox="0 0 24 24">
                      <path d="M12 15V4M7 9l5-5 5 5" />
                      <path d="M5 19h14" />
                    </svg>
                  </button>
                )}
                {dev && (
                  <button
                    className="side-compare"
                    data-tip="Eject — safe removal"
                    onClick={() => ejectStick(dev.letter, label)}
                  >
                    <svg viewBox="0 0 24 24">
                      <path d="M12 5l6 8H6z" />
                      <path d="M6 17h12" />
                    </svg>
                  </button>
                )}
                <button
                  className="side-compare"
                  data-tip={"Forget this device\nRemoves only the index — files stay untouched"}
                  onClick={() =>
                    forgetStick(serial, label, libs.map((l) => l.id))
                  }
                >
                  <svg viewBox="0 0 24 24">
                    <path d="M6 6l12 12M18 6L6 18" />
                  </svg>
                </button>
              </span>
            </div>
            {libs.map(renderLibraryRow)}
            {dev && (
              <>
                <div className="serato-label">
                  Serato crates
                  <button
                    className="side-compare"
                    data-tip="New Serato crate"
                    onClick={() => {
                      setCrateName("");
                      setSeratoModal({ letter: dev.letter, mode: "create" });
                    }}
                  >
                    <svg viewBox="0 0 24 24">
                      <path d="M12 6v12M6 12h12" />
                    </svg>
                  </button>
                  <button
                    className="side-compare"
                    data-tip={"Export all Serato crates to Rekordbox\nWrites a rekordbox.xml — the stick is not changed"}
                    onClick={() => exportToRekordbox(dev.letter, label)}
                  >
                    <svg viewBox="0 0 24 24">
                      <path d="M12 4v11M7 10l5 5 5-5" />
                      <path d="M5 19h14" />
                    </svg>
                  </button>
                </div>
                {(seratoCrates.get(dev.letter) ?? []).map((sc) => (
                  <div
                    key={sc.file}
                    className={`side-item crate serato ${
                      seratoFilter?.letter === dev.letter && seratoFilter?.file === sc.file
                        ? "active"
                        : ""
                    } ${dropCrate === `serato:${dev.letter}:${sc.file}` ? "drop-target" : ""}`}
                    onClick={() => {
                      setActiveLibrary(null);
                      setActiveFolder(null);
                      // no sort IS the crate's own order here; a sort left
                      // over from the previous view (and `sort` is persisted
                      // across restarts) would silently show the crate
                      // alphabetically with no reorder arrows and no clue why
                      setSort(null);
                      setSeratoFilter({
                        letter: dev.letter,
                        file: sc.file,
                        name: sc.name,
                        ids: sc.track_ids,
                      });
                      setRekordboxFilter(null);
                    }}
                    data-tip={
                      "Serato crate — drop tracks to add them" +
                      (sc.missing > 0
                        ? `\n${sc.missing} entr${sc.missing === 1 ? "y" : "ies"} not in the index`
                        : "")
                    }
                    onDragOver={(e) => {
                      if (!dragTracksRef.current) return;
                      e.preventDefault();
                      e.dataTransfer.dropEffect = "copy";
                      setDropCrate(`serato:${dev.letter}:${sc.file}`);
                    }}
                    onDragLeave={() =>
                      setDropCrate((cur) =>
                        cur === `serato:${dev.letter}:${sc.file}` ? null : cur,
                      )
                    }
                    onDrop={(e) => {
                      e.preventDefault();
                      const ids = dragTracksRef.current;
                      dragTracksRef.current = null;
                      setDropCrate(null);
                      if (ids && ids.length > 0) dropTracksOnSerato(dev.letter, sc, ids);
                    }}
                  >
                    <svg viewBox="0 0 24 24">
                      <circle cx="12" cy="12" r="8" />
                      <circle cx="12" cy="12" r="2.5" />
                    </svg>
                    {sc.name}{" "}
                    <span className="count">{sc.track_ids.length.toLocaleString()}</span>
                    <span className="side-actions">
                      <button
                        className="side-compare"
                        data-tip="Compare with libraries, crates and playlists"
                        onClick={(e) => {
                          e.stopPropagation();
                          setComparing({ set: { kind: "serato", letter: dev.letter, file: sc.file }, name: sc.name });
                        }}
                      >
                        <svg viewBox="0 0 24 24">
                          <circle cx="9" cy="12" r="5.5" />
                          <circle cx="15" cy="12" r="5.5" />
                        </svg>
                      </button>
                      <button
                        className="side-compare"
                        data-tip="Find duplicates in here"
                        onClick={(e) => {
                          e.stopPropagation();
                          setDupesFor({ set: { kind: "serato", letter: dev.letter, file: sc.file }, name: sc.name, seratoCrate: { letter: dev.letter, file: sc.file } });
                        }}
                      >
                        <svg viewBox="0 0 24 24">
                          <path d="M8 8h10v10H8z" />
                          <path d="M5 5h10v2H7v8H5z" />
                        </svg>
                      </button>
                      <button
                        className="side-compare"
                        data-tip={"Export this crate to Rekordbox\nWrites a rekordbox.xml — the stick is not changed"}
                        onClick={(e) => {
                          e.stopPropagation();
                          exportToRekordbox(dev.letter, label, { file: sc.file, name: sc.name });
                        }}
                      >
                        <svg viewBox="0 0 24 24">
                          <path d="M12 4v11M7 10l5 5 5-5" />
                          <path d="M5 19h14" />
                        </svg>
                      </button>
                      <button
                        className={`side-compare${
                          syncingSerato === `${dev.letter}:${sc.file}` ? " syncing" : ""
                        }`}
                        data-tip={"Sync with SpinDex\nReconcile links — close Serato first"}
                        onClick={(e) => {
                          e.stopPropagation();
                          openSeratoSync(dev.letter, sc);
                        }}
                      >
                        <svg viewBox="0 0 24 24">
                          <path d="M4 12a8 8 0 0 1 13-6l2-2M20 12a8 8 0 0 1-13 6l-2 2M17 4v3h-3M7 20v-3h3" />
                        </svg>
                      </button>
                      <button
                        className="side-compare"
                        data-tip={"Update tags from SpinDex\nWrites SpinDex's metadata into the files"}
                        onClick={(e) => {
                          e.stopPropagation();
                          runTagRepair(sc.name, () =>
                            api.repairSeratoCrateTags(dev.letter, sc.file),
                          );
                        }}
                      >
                        <svg viewBox="0 0 24 24">
                          <path d="M20 12a8 8 0 1 1-2.3-5.6" />
                          <path d="M20 4v5h-5" />
                        </svg>
                      </button>
                      <button
                        className="side-compare"
                        data-tip={"Regenerate Serato grid\nWrites a beat grid from SpinDex's analysis"}
                        onClick={(e) => {
                          e.stopPropagation();
                          regenGrid(sc.name, (rw) =>
                            api.regenerateSeratoCrateGrids(dev.letter, sc.file, rw),
                          );
                        }}
                      >
                        <svg viewBox="0 0 24 24">
                          <path d="M4 6h16M4 12h16M4 18h16M8 3v18M16 3v18" />
                        </svg>
                      </button>
                      <button
                        className="side-compare"
                        data-tip="Rename Serato crate"
                        onClick={(e) => {
                          e.stopPropagation();
                          setCrateName(sc.name.split(" / ").pop() ?? sc.name);
                          setSeratoModal({
                            letter: dev.letter,
                            mode: "rename",
                            file: sc.file,
                          });
                        }}
                      >
                        <svg viewBox="0 0 24 24">
                          <path d="M4 20l4-1L19 8l-3-3L5 16zM14 7l3 3" />
                        </svg>
                      </button>
                      <button
                        className="side-compare"
                        data-tip={"Delete Serato crate\nOnly the crate file — audio stays"}
                        onClick={(e) => {
                          e.stopPropagation();
                          deleteSeratoCrate(dev.letter, sc);
                        }}
                      >
                        <svg viewBox="0 0 24 24">
                          <path d="M6 6l12 12M18 6L6 18" />
                        </svg>
                      </button>
                    </span>
                  </div>
                ))}
              </>
            )}
            {dev && (rekordboxCrates.get(dev.letter) ?? []).length > 0 && (
              <>
                <div className="rekordbox-label">Rekordbox crates</div>
                {renderRekordboxNodes(dev.letter, rekordboxCrates.get(dev.letter) ?? [], 0, 0)}
              </>
            )}
            {dev && (preparedRekordbox.get(dev.letter) ?? []).length > 0 && (
              <>
                <div
                  className="rekordbox-label"
                  data-tip={`Playlists SpinDex exported to ${dev.letter}\\SpinDex\\rekordbox.xml.\nNot in Rekordbox yet: import the XML in Rekordbox and export to this stick.`}
                >
                  Prepared for Rekordbox <span className="prepared-note">not yet in Rekordbox</span>
                </div>
                {renderRekordboxNodes(dev.letter, preparedRekordbox.get(dev.letter) ?? [], 0, 0)}
              </>
            )}
          </div>
        );
      })}
      <div className="add-lib-wrap">
        <div className="side-item add" onClick={() => setAddMenu((v) => !v)}>
          <svg viewBox="0 0 24 24">
            <path d="M12 5v14M5 12h14" />
          </svg>
          Add library
        </div>
        {addMenu && (
          <div className="menu" onMouseLeave={() => setAddMenu(false)}>
            <div
              className="menu-item"
              onClick={() => {
                setAddMenu(false);
                addLibrary();
              }}
            >
              Add existing folder…
            </div>
            <div
              className="menu-item"
              onClick={() => {
                setAddMenu(false);
                pickNewLibraryParent();
              }}
            >
              Create new folder…
            </div>
          </div>
        )}
      </div>
      {libraries.length === 0 && (
        <div className="side-empty">
          Pick your music folder to build the index. Files are never modified.
        </div>
      )}
      {!devicesLoaded && (
        <>
          <div className="side-label">Devices</div>
          <div className="side-item device-loading">Scanning devices…</div>
        </>
      )}
      {devices.filter((d) => d.known_library_id === null).length > 0 && (
        <>
          <div className="side-label">Devices</div>
          {devices
            .filter((d) => d.known_library_id === null)
            .map((d) => (
              <div className="side-item device" key={d.letter}>
                <svg viewBox="0 0 24 24">
                  <path d="M7 8V5a2 2 0 0 1 2-2h6a2 2 0 0 1 2 2v3M5 8h14v11a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2z" />
                </svg>
                <span className="device-name">
                  {d.label} ({d.letter})
                </span>
                <button
                  className="side-add-device"
                  data-tip="Index this drive as a library"
                  onClick={() => {
                    api
                      .addDeviceLibraries(d.letter)
                      .then(async (libs) => {
                        refreshLibraries().catch(() => {});
                        refreshDevices().catch(() => {});
                        if (libs.length === 0) return;
                        setActiveLibrary(libs[0].id);
                        for (const lib of libs) {
                          setScanning({ library_id: lib.id, found: 0 });
                          try {
                            await api.scanLibrary(lib.id);
                          } catch (e) {
                            showToast(String(e), true);
                          }
                        }
                        setScanning(null);
                        refreshLibraries().catch(() => {});
                        refreshTracks().catch(() => {});
                      })
                      .catch((e) => showToast(String(e), true));
                  }}
                >
                  + Add
                </button>
                <button
                  className="side-compare device-eject"
                  data-tip="Eject — safe removal"
                  onClick={() => ejectStick(d.letter, d.label)}
                >
                  <svg viewBox="0 0 24 24">
                    <path d="M12 5l6 8H6z" />
                    <path d="M6 17h12" />
                  </svg>
                </button>
              </div>
            ))}
        </>
      )}

      {health && libraries.length > 0 && (
        <>
          <div
            className="side-label collapsible"
            onClick={() => setHealthCollapsed((cur) => !cur)}
            data-tip={healthCollapsed ? "Expand" : "Collapse"}
          >
            <button className={`crate-chevron ${healthCollapsed ? "" : "open"}`}>
              <svg viewBox="0 0 24 24">
                <path d="M9 6l6 6-6 6" />
              </svg>
            </button>
            Library health
          </div>
          {!healthCollapsed && (
            <>
              <div
                className={`health-item ${health.duplicates > 0 ? "warn-red" : ""}`}
                onClick={() => {
                  const lib =
                    libraries.find((l) => l.id === activeLibrary) ??
                    (libraries.length === 1 ? libraries[0] : null);
                  if (lib) setDupesFor({ set: { kind: "library", id: lib.id }, name: lib.name });
                  else showToast("Select a library to inspect its duplicates");
                }}
                data-tip="Show duplicate groups"
              >
                Duplicates <span className="count">{health.duplicates.toLocaleString()}</span>
              </div>
              {(
                [
                  ["missing_bpm", "Missing BPM", health.missing_bpm, "warn-amber"],
                  ["missing_key", "Missing key", health.missing_key, "warn-amber"],
                  ["missing_artwork", "Missing artwork", health.missing_artwork, "warn-amber"],
                  ["low_quality", "Low quality", health.low_quality, "warn-amber"],
                  ["unsteady_tempo", "Unsteady tempo", health.unsteady_tempo, "warn-amber"],
                  ["missing_file", "Missing files", health.missing_file, "warn-red"],
                  ["analysis_error", "Analysis errors", health.analysis_error, "warn-red"],
                ] as [api.HealthFilter, string, number, string][]
              ).map(([key, label, count, cls]) => (
                <div
                  key={key}
                  className={`health-item ${count > 0 ? cls : ""} ${
                    healthFilter === key ? "active" : ""
                  }`}
                  onClick={() => setHealthFilter((cur) => (cur === key ? null : key))}
                  data-tip="Filter the track list"
                >
                  {label} <span className="count">{count.toLocaleString()}</span>
                </div>
              ))}
            </>
          )}
        </>
      )}

      {analysis && (
        <>
          <div className="side-label">Analysis</div>
          <div
            className="analysis-pill"
            style={
              {
                "--pct": `${
                  ((analysis.done + analysis.failed) /
                    Math.max(1, analysis.total)) *
                  100
                }%`,
              } as CSSProperties
            }
          >
            <span className="mono">
              {analysis.done + analysis.failed}/{analysis.total}
            </span>
            {analysis.failed > 0 && (
              <span className="mono failed">{analysis.failed} failed</span>
            )}
            <button
              className="pill-cancel"
              onClick={() => api.cancelAnalysis().catch(() => {})}
              data-tip="Cancel analysis"
            >
              ✕
            </button>
          </div>
        </>
      )}
      {copying && (
        <>
          <div className="side-label">Copying</div>
          <div
            className="analysis-pill"
            style={
              {
                "--pct": `${
                  ((copying.done + copying.skipped + copying.failed) /
                    Math.max(1, copying.total)) *
                  100
                }%`,
              } as CSSProperties
            }
          >
            <span className="mono">
              {copying.done + copying.skipped + copying.failed}/{copying.total}
            </span>
            {copying.failed > 0 && (
              <span className="mono failed">{copying.failed} failed</span>
            )}
            <button
              className="pill-cancel"
              onClick={() => api.cancelCopy().catch(() => {})}
              data-tip="Cancel copy"
            >
              ✕
            </button>
          </div>
        </>
      )}
      {batch && (
        <>
          <div className="side-label">Metadata</div>
          <div
            className="analysis-pill"
            style={
              { "--pct": `${((batch.done + batch.failed) / Math.max(1, batch.total)) * 100}%` } as CSSProperties
            }
          >
            <span className="mono">{batch.done + batch.failed}/{batch.total}</span>
            {batch.failed > 0 && <span className="mono failed">{batch.failed} failed</span>}
            <button
              className="pill-cancel"
              onClick={() => api.cancelBatch().catch(() => {})}
              data-tip="Cancel metadata update"
            >
              ✕
            </button>
          </div>
        </>
      )}
      {syncing && (
        <>
          <div className="side-label">Rekordbox export</div>
          <div
            className="analysis-pill"
            style={{ "--pct": `${(syncing.done / Math.max(1, syncing.total)) * 100}%` } as CSSProperties}
          >
            <span className="mono">{syncing.done}/{syncing.total}</span>
            <button
              className="pill-cancel"
              onClick={() => api.cancelSync().catch(() => {})}
              data-tip="Cancel export"
            >
              ✕
            </button>
          </div>
        </>
      )}
      {rbImporting && (
        <>
          <div className="side-label">Rekordbox import</div>
          <div
            className="analysis-pill"
            style={
              { "--pct": `${(rbImporting.done / Math.max(1, rbImporting.total)) * 100}%` } as CSSProperties
            }
          >
            <span className="mono">
              {rbImporting.total > 0 ? `${rbImporting.done}/${rbImporting.total}` : "preparing…"}
            </span>
            <button
              className="pill-cancel"
              onClick={() => api.cancelRekordboxImport().catch(() => {})}
              data-tip="Cancel import"
            >
              ✕
            </button>
          </div>
        </>
      )}
      {enhancing && (
        <>
          <div className="side-label">Enhance</div>
          <div
            className="analysis-pill"
            style={
              {
                "--pct": `${
                  ((enhancing.done + enhancing.failed + enhancing.skipped) /
                    Math.max(1, enhancing.total)) *
                  100
                }%`,
              } as CSSProperties
            }
          >
            <span className="mono">
              {enhancing.done + enhancing.failed + enhancing.skipped}/{enhancing.total}
            </span>
            {enhancing.failed > 0 && (
              <span className="mono failed">{enhancing.failed} failed</span>
            )}
            <button
              className="pill-cancel"
              onClick={() => api.cancelEnhance().catch(() => {})}
              data-tip="Cancel enhance"
            >
              ✕
            </button>
          </div>
        </>
      )}
      {stabilizing && (
        <>
          <div className="side-label">Tempo</div>
          <div
            className="analysis-pill"
            style={
              {
                "--pct": `${
                  ((stabilizing.done + stabilizing.failed + stabilizing.skipped) /
                    Math.max(1, stabilizing.total)) *
                  100
                }%`,
              } as CSSProperties
            }
          >
            <span className="mono">
              {stabilizing.done + stabilizing.failed + stabilizing.skipped}/
              {stabilizing.total}
            </span>
            {stabilizing.failed > 0 && (
              <span className="mono failed">{stabilizing.failed} failed</span>
            )}
            <button
              className="pill-cancel"
              onClick={() => api.cancelStabilize().catch(() => {})}
              data-tip="Cancel tempo run"
            >
              ✕
            </button>
          </div>
        </>
      )}
      {mlRestoreRun && (
        <>
          <div className="side-label">Restoring</div>
          <div
            className="analysis-pill"
            style={
              {
                "--pct": `${
                  ((mlRestoreRun.done + mlRestoreRun.failed + mlRestoreRun.skipped) /
                    Math.max(1, mlRestoreRun.total)) *
                  100
                }%`,
              } as CSSProperties
            }
          >
            <span className="mono">
              {mlRestoreRun.done + mlRestoreRun.failed + mlRestoreRun.skipped}/{mlRestoreRun.total}
              {mlRestoreRun.current ? ` · ${mlRestoreRun.current}` : ""}
              {mlRestoreRun.chunk_total > 0
                ? ` (${Math.round((mlRestoreRun.chunk_done / mlRestoreRun.chunk_total) * 100)}%)`
                : ""}
            </span>
            {mlRestoreRun.failed > 0 && (
              <span className="mono failed">{mlRestoreRun.failed} failed</span>
            )}
            <button
              className="pill-cancel"
              onClick={() => api.cancelMlRestore().catch(() => {})}
              data-tip="Cancel ML restore"
            >
              ✕
            </button>
          </div>
        </>
      )}
    </div>
  );
}
