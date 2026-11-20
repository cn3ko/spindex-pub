// Headless check of the split view: renders the whole App with the Tauri
// backend mocked (one library, crates "House" (12 tracks) and "Techno" (empty); list_tracks sorts by
// key/bpm, move/reorder commands change the mock crate order).
// Build: npx esbuild scripts/split-smoke/entry.tsx --bundle --outfile=scripts/split-smoke/bundle.js --loader:.css=css
// Serve this folder over HTTP and open index.html; every invoke is in window.__calls.
import { createRoot } from "react-dom/client";
const W = window as any;
W.__calls = [];
const keys = ["8A","3B","12A","5A","8B","1A","10A","3A","6B","11A","2A","7A"];
// crates by folder: "House" holds tracks 1-12 in order, "Techno" starts empty;
// tracks 13-18 sit in the library root. Adding moves a track (same library).
const crates: Record<string, number[]> = { House: keys.map((_, i) => i + 1), Techno: [] };
let loose = [13, 14, 15, 16, 17, 18];
const folderOf = (id: number) => Object.keys(crates).find((f) => crates[f].includes(id)) ?? "";
const row = (id: number) => ({
  id, library_id: 1, rel_path: folderOf(id) ? `${folderOf(id)}/track${id}.mp3` : `track${id}.mp3`,
  title: `Track ${id}`, artist: `Artist ${id}`,
  album: "Album", genre: "House", label: null, year: 2020, bpm: 120 + id, key_camelot: keys[(id - 1) % 12],
  rating: (id % 5) + 1, comment: null, duration_secs: 200 + id * 3, bitrate_kbps: 320, format: "mp3",
  has_artwork: false, is_missing: false, added_at: Date.now(), modified_at: String(Date.now()),
  key_source: "analysis", key_confidence: 0.9, key_candidates: null, loudness_lufs: null, clip_ratio: null,
  cutoff_hz: null, quality_score: null, enhanced_at: null, crate_pos: null, tempo_drift_pct: null,
  serato_grid_source: null,
});
const camel = (k: string) => parseInt(k) * 2 + (k.endsWith("B") ? 1 : 0);
W.__crates = crates;
W.__TAURI_INTERNALS__ = {
  transformCallback: () => 0,
  invoke: async (cmd: string, args: any) => {
    W.__calls.push([cmd, args]);
    switch (cmd) {
      case "list_libraries": return [{ id: 1, name: "Music", root_path: "C:\Music\\", is_removable: false, last_scanned_at: null, track_count: 18, volume_serial: null }];
      case "list_crates": return Object.keys(crates).map((f) => ({ path: f, name: f, depth: 0, track_count: crates[f].length }));
      case "list_tracks": {
        let rows = args.folder != null && crates[args.folder]
          ? crates[args.folder].map((id, i) => ({ ...row(id), crate_pos: i + 1 }))
          : [...Object.values(crates).flat(), ...loose].map(row);
        if (args.query) rows = rows.filter(r => r.title.toLowerCase().includes(String(args.query).toLowerCase()));
        if (args.keys) rows = rows.filter(r => args.keys.includes(r.key_camelot));
        if (args.sort === "key") rows.sort((a, b) => camel(a.key_camelot!) - camel(b.key_camelot!));
        if (args.sort === "bpm") rows.sort((a, b) => a.bpm! - b.bpm!);
        if (args.descending) rows.reverse();
        return { rows: rows.slice(args.offset, args.offset + args.limit), total: rows.length };
      }
      case "add_tracks_to_crate": {
        const into = crates[args.folder];
        const ids: number[] = args.trackIds.filter((x: number) => !into.includes(x));
        for (const f of Object.keys(crates)) crates[f] = crates[f].filter(x => !ids.includes(x));
        loose = loose.filter(x => !ids.includes(x));
        const target = crates[args.folder];
        target.splice(Math.min(args.beforeIndex, target.length), 0, ...ids);
        return { moved: ids.length, copied: 0, skipped: 0, failed: 0 };
      }
      case "delete_tracks": {
        const ids: number[] = args.trackIds;
        for (const f of Object.keys(crates)) crates[f] = crates[f].filter(x => !ids.includes(x));
        loose = loose.filter(x => !ids.includes(x));
        return { deleted: ids.length, failed: [] };
      }
      case "reorder_crate_tracks": {
        const order = crates[args.folder];
        const ids: number[] = args.trackIds; const t: number = args.beforeIndex;
        const rest = order.filter(x => !ids.includes(x));
        const before = order.slice(0, t).filter(x => !ids.includes(x)).length;
        rest.splice(before, 0, ...ids); crates[args.folder] = rest; return null;
      }
      case "move_crate_tracks": {
        const order = crates[args.folder];
        const id: number = args.trackIds[0]; const i = order.indexOf(id); const j = i + args.delta;
        if (j >= 0 && j < order.length) { order.splice(i, 1); order.splice(j, 0, id); }
        return null;
      }
      case "sort_crate_tracks": {
        // the real orders come from Rust (crate_sort.rs, unit-tested); the
        // mock only needs a visible change: key low to high, else reversed
        const order = crates[args.folder];
        if (args.mode === "key_asc") order.sort((a, b) => camel(row(a).key_camelot!) - camel(row(b).key_camelot!));
        else order.reverse();
        return null;
      }
      case "library_health": return new Proxy({}, { get: () => 0 });
      case "list_devices": return [];
      default:
        if (cmd.startsWith("list_")) return [];
        if (cmd.endsWith("_ready") || cmd.endsWith("available")) return false;
        return null;
    }
  },
};
W.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
import("../../src/App").then(({ default: App }) => {
  createRoot(document.getElementById("root")!).render(<App />);
});
