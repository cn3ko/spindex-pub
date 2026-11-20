// Headless layout check for the deck strip and the grid editor: renders the
// real DeckPlayer with the real App.css inside the app's grid, with the Tauri
// backend mocked. Serve this folder over HTTP with a `wave.bin` beside it: a
// real track's grid_edit::detail_payload followed by its colour_payload minus
// that payload's own length header (u32 ms, 4 + 8 + 3 + 2 bytes per ms).
// `?grid=1` opens grid mode, `?pos=30` renders the grid editor alone with its
// playhead at 30 s, `?slow=MS` delays both grid replies, as a debug build does,
// with `?pos`, `?loop=IN,OUT` marks an A-B loop through the I and O keys, and
// `?audio=FILE` gives the deck a file from this folder to play (serve with
// serve.py, not `python -m http.server`: seeking in audio needs Range requests).
// Build: npx esbuild scripts/deck-smoke/entry.tsx --bundle --outfile=scripts/deck-smoke/bundle.js --loader:.css=css
import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import "../../src/App.css";
import DeckPlayer from "../../src/components/DeckPlayer";
import GridEditor from "../../src/components/GridEditor";
import type { TrackRow } from "../../src/api";

const params = new URLSearchParams(location.search);
const grid = params.get("grid") === "1";
// ?pos=SECONDS renders the grid editor alone with its playhead there
const pos = params.get("pos");

async function boot() {
  const wave = new Uint8Array(await (await fetch("wave.bin", { cache: "no-store" })).arrayBuffer());
  // grid_edit::detail_payload: u32 LE ms count, then [peak,low,mid,high] per ms, …
  const ms = new DataView(wave.buffer).getUint32(0, true);
  // the deck's 400-bucket overview from the same bytes: peak + bands
  const peaks: number[] = [];
  const bands: number[] = [];
  for (let b = 0; b < 400; b++) {
    const a = Math.floor((b * ms) / 400);
    const e = Math.floor(((b + 1) * ms) / 400);
    const m = [0, 0, 0, 0];
    for (let i = a; i < e; i++) for (let k = 0; k < 4; k++) m[k] = Math.max(m[k], wave[4 + i * 4 + k]);
    peaks.push(m[0]);
    bands.push(m[1], m[2], m[3]);
  }
  // the two grid replies, cut from the combined file with their own headers
  const header = wave.slice(0, 4);
  const join = (...parts: Uint8Array[]) => {
    const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
    let at = 0;
    for (const p of parts) {
      out.set(p, at);
      at += p.length;
    }
    return out.buffer;
  };
  const waveReply = join(header, wave.subarray(4, 4 + ms * 12));
  const colourReply = join(header, wave.subarray(4 + ms * 12, 4 + ms * 17));
  const slow = Number(params.get("slow") ?? 0);
  const later = (v: unknown) => new Promise((r) => setTimeout(() => r(v), slow));
  const replies: Record<string, unknown> = {
    get_waveform: peaks,
    waveform_bands: bands,
    artwork_path: null,
    prepare_playback: { path: params.get("audio") ?? "none.wav", transcoded: false },
    track_grid: { sections: [{ start_sec: 0.1, bpm: 125 }], source: "file", file_unreadable: false },
    grid_waveform: waveReply,
    grid_colours: colourReply,
  };
  (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
    // every call is kept in window.__calls, so a probe can check what was saved
    invoke: async (cmd: string, args: unknown) => {
      const w = window as unknown as { __calls?: [string, unknown][] };
      (w.__calls ??= []).push([cmd, args]);
      return cmd.startsWith("grid_") ? later(replies[cmd]) : cmd in replies ? replies[cmd] : null;
    },
    transformCallback: () => 0,
    convertFileSrc: (p: string) => p,
  };
  const track = {
    id: 1, library_id: 1, rel_path: "Library WAV/50 Cent - In Da Club (Lemo Tech House Remix).wav",
    title: "In Da Club (Lemo Tech House Remix)", artist: "50 Cent", album: null, genre: null, label: null,
    year: null, bpm: 125, key_camelot: "8A", rating: null, comment: null, duration_secs: ms / 1000,
    bitrate_kbps: 1411, format: "wav", has_artwork: false, is_missing: false, added_at: null,
    modified_at: "0", key_source: null, key_confidence: null, key_candidates: null, loudness_lufs: null,
    clip_ratio: null, cutoff_hz: null, quality_score: null, enhanced_at: null, crate_pos: null,
    tempo_drift_pct: null, serato_grid_source: null,
  } as TrackRow;
  const noop = () => {};
  if (pos !== null) {
    // with no audio the editor reads its playhead from `position`: move it to
    // each mark and press the key, as a user would
    const marks = (params.get("loop") ?? "").split(",").map(Number).filter(Number.isFinite);
    const press = (key: string) => window.dispatchEvent(new KeyboardEvent("keydown", { key }));
    const Editor = () => {
      const [at, setAt] = useState(Number(pos));
      useEffect(() => {
        if (marks.length !== 2) return;
        const t: number[] = [];
        t.push(window.setTimeout(() => setAt(marks[0]), 300));
        t.push(window.setTimeout(() => press("i"), 450));
        t.push(window.setTimeout(() => setAt(marks[1]), 600));
        t.push(window.setTimeout(() => press("o"), 750));
        t.push(window.setTimeout(() => setAt(Number(pos)), 900));
        return () => t.forEach(clearTimeout);
      }, []);
      return (
        <GridEditor
          track={track} audio={null} position={at} duration={ms / 1000}
          overviewPeaks={peaks} overviewBands={bands} onDirtyChange={noop} onSaved={noop}
          onError={noop} onNotify={noop}
        />
      );
    };
    createRoot(document.getElementById("root")!).render(
      <div className="deck grid-mode" style={{ width: 1150, padding: 16 }}>
        <Editor />
      </div>,
    );
    setTimeout(() => (document.title = "DECK-OK"), 800);
    return;
  }
  createRoot(document.getElementById("root")!).render(
    <div className={"app" + (grid ? " grid-mode" : "")}>
      <div style={{ gridArea: "titlebar", background: "#14171c" }} />
      <div style={{ gridArea: "sidebar", background: "#14171c" }} />
      <div style={{ gridArea: "main" }} />
      <DeckPlayer
        track={track} libraryName="Library WAV (USB)" playToken={0} waveToken={0} artToken={0}
        onPrev={noop} onNext={noop} onEnded={noop} shuffle={false} repeat="off"
        onToggleShuffle={noop} onCycleRepeat={noop} onError={noop} onEdit={noop}
        gridMode={grid} onToggleGrid={noop} onGridDirtyChange={noop} onGridSaved={noop}
        onGridError={noop} onNotify={noop}
      />
    </div>,
  );
  setTimeout(() => (document.title = "DECK-OK"), 800);
}
boot().catch((e) => (document.title = "DECK-FAIL " + e));
