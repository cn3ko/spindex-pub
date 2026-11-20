import * as api from "../api";
import { ALL_COLUMNS } from "../columns";
import { useState } from "react";
import KeyFilterPopup, { sortKeys } from "./KeyFilterPopup";
import { keyBadgeColor } from "../theme";

interface ToolbarProps {
  query: string;
  setQuery: (q: string) => void;
  selectedIds: Set<number>;
  tracks: api.TrackRow[];
  setEditing: (t: api.TrackRow) => void;
  analyzerReady: boolean;
  analysis: api.AnalysisProgress | null;
  analyzeMenu: boolean;
  setAnalyzeMenu: React.Dispatch<React.SetStateAction<boolean>>;
  runAnalysis: (mode: "selected" | "missing" | "overwrite") => void;
  copying: api.CopyProgress | null;
  copyMenu: boolean;
  setCopyMenu: React.Dispatch<React.SetStateAction<boolean>>;
  libraries: api.LibraryInfo[];
  startCopy: (targetId: number, trackIds?: number[], destFolder?: string) => void;
  playing: api.TrackRow | null;
  setVizOpen: (v: boolean) => void;
  colMenu: boolean;
  splitOn: boolean;
  setSplitOn: React.Dispatch<React.SetStateAction<boolean>>;
  /** a folder or Serato crate is open, the only views that can be hand-ordered */
  splitAvailable: boolean;
  /** a folder or Serato crate is open, so the Sort menu can re-number it */
  crateOpen: boolean;
  sortCrate: (mode: api.CrateSortMode) => void;
  setColMenu: React.Dispatch<React.SetStateAction<boolean>>;
  hiddenCols: Set<string>;
  setHiddenCols: React.Dispatch<React.SetStateAction<Set<string>>>;
  showLibraryColumn: boolean;
  seratoFilter: { letter: string; file: string; name: string; ids: number[] } | null;
  setSeratoFilter: (
    v: { letter: string; file: string; name: string; ids: number[] } | null,
  ) => void;
  rekordboxFilter: { letter: string; id: number; name: string; ids: number[] } | null;
  setRekordboxFilter: (
    v: { letter: string; id: number; name: string; ids: number[] } | null,
  ) => void;
  removeFromSerato: () => void;
  activeFolder: string | null;
  setActiveFolder: (path: string | null) => void;
  removeFromCrate: () => void;
  healthFilter: api.HealthFilter | null;
  setHealthFilter: React.Dispatch<React.SetStateAction<api.HealthFilter | null>>;
  keyFilter: Set<string>;
  setKeyFilter: (keys: Set<string>) => void;
  total: number;
}

const SORT_TIPS: Record<api.CrateSortMode, string> = {
  key_asc: "1A, 1B, 2A … 12B; slower first within a key; no key last",
  key_desc: "12B, 12A … 1A; slower first within a key; no key last",
  harmonic:
    "Starts on a random track, then each next key mixes:\nsame key, relative A/B, or up to 2 steps on the wheel",
  harmonic_first:
    "Keeps your first track as the opener, then each next key mixes:\nsame key, relative A/B, or up to 2 steps on the wheel",
  bpm: "Slowest first; no BPM last",
  random: "Plain shuffle",
};

// Toolbar: search box (with the clear ✕ button), the Edit/Analyze/Copy
// to/Viz/columns buttons, the active filter chips (Serato crate,
// Rekordbox playlist, folder-crate, health) and the result count.
//
// Presentational only — every action is a callback prop owned by App.tsx.
export default function Toolbar({
  query,
  setQuery,
  selectedIds,
  tracks,
  setEditing,
  analyzerReady,
  analysis,
  analyzeMenu,
  setAnalyzeMenu,
  runAnalysis,
  copying,
  copyMenu,
  setCopyMenu,
  libraries,
  startCopy,
  playing,
  setVizOpen,
  splitOn,
  setSplitOn,
  splitAvailable,
  crateOpen,
  sortCrate,
  colMenu,
  setColMenu,
  hiddenCols,
  setHiddenCols,
  showLibraryColumn,
  seratoFilter,
  setSeratoFilter,
  rekordboxFilter,
  setRekordboxFilter,
  removeFromSerato,
  activeFolder,
  setActiveFolder,
  removeFromCrate,
  healthFilter,
  setHealthFilter,
  keyFilter,
  setKeyFilter,
  total,
}: ToolbarProps) {
  const [keyMenu, setKeyMenu] = useState(false);
  const [sortMenu, setSortMenu] = useState(false);
  const keys = sortKeys(keyFilter);
  return (
    <div className="toolbar">
      <div className="search">
        <svg viewBox="0 0 24 24">
          <circle cx="11" cy="11" r="7" />
          <path d="M21 21l-4.3-4.3" />
        </svg>
        <input
          placeholder="Search title, artist, album, genre…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          spellCheck={false}
        />
        {query && (
          <button
            className="search-clear"
            aria-label="Clear search"
            onClick={() => setQuery("")}
          >
            ✕
          </button>
        )}
      </div>
      {/* tip lives on the wrapper: a disabled button gets no mouse events */}
      <span
        className="tip-wrap"
        data-tip={
          selectedIds.size === 1
            ? "Edit tags of the selected track"
            : "Select exactly one track to edit"
        }
      >
        <button
          className="btn"
          disabled={selectedIds.size !== 1}
          onClick={() => {
            const id = Array.from(selectedIds)[0];
            const t = tracks.find((x) => x.id === id);
            if (t) setEditing(t);
          }}
        >
          Edit
        </button>
      </span>
      <div className="analyze-wrap">
        <span
          className="tip-wrap"
          data-tip={
            analyzerReady
              ? "Detect BPM, key and waveform"
              : "ffmpeg is not installed — run install-components.ps1 (next to SpinDex, or in scripts/) and restart. It is not bundled because it is GPL-licensed."
          }
        >
          <button
            className="btn"
            disabled={!analyzerReady || analysis !== null}
            onClick={() => setAnalyzeMenu((v) => !v)}
          >
            Analyze ▾
          </button>
        </span>
        {analyzeMenu && (
          <div className="menu" onMouseLeave={() => setAnalyzeMenu(false)}>
            <div
              className={`menu-item ${selectedIds.size > 0 ? "" : "disabled"}`}
              onClick={() => selectedIds.size > 0 && runAnalysis("selected")}
            >
              Analyze selected{" "}
              {selectedIds.size > 0 ? `(${selectedIds.size})` : ""}
            </div>
            <div className="menu-item" onClick={() => runAnalysis("missing")}>
              Analyze all missing BPM/key
            </div>
            <div className="menu-item" onClick={() => runAnalysis("overwrite")}>
              Re-analyze (overwrite)
            </div>
          </div>
        )}
      </div>
      <div className="analyze-wrap">
        <button
          className="btn"
          disabled={selectedIds.size === 0 || copying !== null}
          data-tip={
            selectedIds.size > 0
              ? "Copy the selected tracks into another library's folder"
              : "Select tracks to copy"
          }
          onClick={() => setCopyMenu((v) => !v)}
        >
          Copy to ▾
        </button>
        {copyMenu && (
          <div className="menu" onMouseLeave={() => setCopyMenu(false)}>
            <div className="menu-label">
              Copy {selectedIds.size} track{selectedIds.size === 1 ? "" : "s"} to
            </div>
            {libraries.map((lib) => (
              <div key={lib.id} className="menu-item" onClick={() => startCopy(lib.id)}>
                {lib.name}{" "}
                <span className="mono dim">{lib.track_count.toLocaleString()}</span>
              </div>
            ))}
          </div>
        )}
      </div>
      <button
        className="btn"
        disabled={!playing}
        data-tip={playing ? "Open visualizer" : "Load a track first"}
        onClick={() => setVizOpen(true)}
      >
        Viz
      </button>
      <div className="analyze-wrap">
        <button
          className={`btn${keys.length > 0 ? " active" : ""}`}
          data-tip="Show only tracks in the keys you pick on the Camelot wheel"
          // the popup closes itself on an outside mousedown; stop this one
          // reaching it so a click on the button toggles instead of reopening
          onMouseDown={(e) => keyMenu && e.stopPropagation()}
          onClick={() => setKeyMenu((v) => !v)}
        >
          Key{keys.length > 0 ? ` · ${keys.length}` : ""} ▾
        </button>
        {keyMenu && (
          <KeyFilterPopup
            selected={keyFilter}
            onChange={setKeyFilter}
            onClose={() => setKeyMenu(false)}
          />
        )}
      </div>
      <div className="analyze-wrap">
        <span
          className="tip-wrap"
          data-tip={
            crateOpen
              ? "Re-number the whole crate once: by key, BPM, a harmonic mix or shuffled"
              : "Open a crate to sort it"
          }
        >
          <button
            className="btn"
            disabled={!crateOpen}
            onClick={() => setSortMenu((v) => !v)}
          >
            Sort ▾
          </button>
        </span>
        {sortMenu && crateOpen && (
          <div className="menu" onMouseLeave={() => setSortMenu(false)}>
            <div className="menu-label">Re-number the crate</div>
            {(Object.keys(api.CRATE_SORT_LABELS) as api.CrateSortMode[]).map((m) => (
              <div
                key={m}
                className="menu-item"
                data-tip={SORT_TIPS[m]}
                onClick={() => {
                  setSortMenu(false);
                  sortCrate(m);
                }}
              >
                {api.CRATE_SORT_LABELS[m]}
              </div>
            ))}
          </div>
        )}
      </div>
      <span
        data-tip={
          !splitAvailable
            ? "Open a crate to split it into overview and running order"
            : splitOn
              ? "Back to a single table"
              : "Split: browse any library or crate beside the one you are building"
        }
      >
        <button
          className={`btn icon${splitOn && splitAvailable ? " active" : ""}`}
          disabled={!splitAvailable}
          aria-pressed={splitOn && splitAvailable}
          onClick={() => setSplitOn((v) => !v)}
        >
          <svg viewBox="0 0 24 24">
            <rect x="3" y="4" width="18" height="16" rx="2" />
            <path d="M12 4v16" />
          </svg>
        </button>
      </span>
      <div className="analyze-wrap">
        <button
          className="btn icon"
          data-tip="Choose table columns"
          onClick={() => setColMenu((v) => !v)}
        >
          <svg viewBox="0 0 24 24">
            <path d="M12 8.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7z" />
            <path d="M19.4 13.5a7.6 7.6 0 0 0 0-3l2-1.5-2-3.5-2.4 1a7.6 7.6 0 0 0-2.6-1.5L14 2.5h-4l-.4 2.5A7.6 7.6 0 0 0 7 6.5l-2.4-1-2 3.5 2 1.5a7.6 7.6 0 0 0 0 3l-2 1.5 2 3.5 2.4-1a7.6 7.6 0 0 0 2.6 1.5l.4 2.5h4l.4-2.5a7.6 7.6 0 0 0 2.6-1.5l2.4 1 2-3.5-2-1.5z" />
          </svg>
        </button>
        {colMenu && (
          <div className="menu" onMouseLeave={() => setColMenu(false)}>
            <div className="menu-label">Columns</div>
            {ALL_COLUMNS.filter(
              (c) => !c.always && (c.key !== "library" || showLibraryColumn),
            ).map((c) => (
              <label className="menu-check" key={c.key}>
                <input
                  type="checkbox"
                  checked={!hiddenCols.has(c.key)}
                  onChange={() =>
                    setHiddenCols((prev) => {
                      const next = new Set(prev);
                      if (next.has(c.key)) next.delete(c.key);
                      else next.add(c.key);
                      return next;
                    })
                  }
                />
                {c.label === "#" ? "Numbering" : c.label}
              </label>
            ))}
          </div>
        )}
      </div>
      {seratoFilter && (
        <span
          className="filter-chip"
          onClick={() => setSeratoFilter(null)}
          data-tip="Clear the Serato crate filter"
        >
          Serato: {seratoFilter.name} ✕
        </span>
      )}
      {rekordboxFilter && (
        <span
          className="filter-chip"
          onClick={() => setRekordboxFilter(null)}
          data-tip="Clear the Rekordbox playlist filter"
        >
          Rekordbox: {rekordboxFilter.name} ✕
        </span>
      )}
      {seratoFilter && selectedIds.size > 0 && (
        <span
          className="filter-chip"
          onClick={removeFromSerato}
          data-tip={"Remove the selection from this Serato crate\nOnly link entries — audio stays"}
        >
          Remove {selectedIds.size} from crate
        </span>
      )}
      {activeFolder !== null && (
        <span
          className="filter-chip"
          onClick={() => setActiveFolder(null)}
          data-tip="Show the whole library"
        >
          {activeFolder.split("/").pop() || "(root)"} ✕
        </span>
      )}
      {activeFolder !== null && activeFolder !== "" && selectedIds.size > 0 && (
        <span
          className="filter-chip"
          onClick={removeFromCrate}
          data-tip={"Move the selection out of this crate\nFiles go to the library root"}
        >
          Remove {selectedIds.size} from crate
        </span>
      )}
      {healthFilter && (
        <span className="filter-chip" onClick={() => setHealthFilter(null)}>
          {
            {
              missing_bpm: "Missing BPM",
              missing_key: "Missing key",
              missing_artwork: "Missing artwork",
              low_quality: "Low quality",
              unsteady_tempo: "Unsteady tempo",
              missing_file: "Missing files",
              analysis_error: "Analysis errors",
            }[healthFilter]
          }{" "}
          ✕
        </span>
      )}
      {keys.length > 0 && (
        <span
          className="filter-chip key-chip"
          onClick={() => setKeyFilter(new Set())}
          data-tip="Clear the key filter"
        >
          {keys.length <= 4
            ? keys.map((k) => (
                <span key={k} className="key-badge" style={{ background: keyBadgeColor(k) }}>
                  {k}
                </span>
              ))
            : `${keys.length} keys`}{" "}
          ✕
        </span>
      )}
      <span className="result-count">
        {total.toLocaleString()} {query ? "results" : "tracks"}
      </span>
    </div>
  );
}
