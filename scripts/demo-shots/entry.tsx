// Renders the whole SpinDex app with an invented demo library, for README
// screenshots that show no real collection. The Tauri backend is mocked
// (window.__TAURI_INTERNALS__); every artist, title and cover is made up, and
// the waveforms are synthesized (a 124 BPM house groove), never decoded audio.
// Build: npx esbuild scripts/demo-shots/entry.tsx --bundle --outfile=scripts/demo-shots/bundle.js --loader:.css=css
// The deck needs scripts/demo-shots/silence.wav (any 367 s WAV; git-ignored). Serve with
// `python scripts/deck-smoke/serve.py scripts/demo-shots 8766` and open index.html at 1440x900;
// hide the Library/Added/Updated columns via localStorage `spindex.hiddencols` for the README shots.
import React from "react";
import ReactDOM from "react-dom/client";
import App from "../../src/App";
import type { TrackRow } from "../../src/api";

// ---- the demo collection -------------------------------------------------

const ARTISTS = [
  "Nova Lane", "The Velvet Static", "Kito Marrow", "Sundial Club", "Ama Rios", "Low Tide Collective",
  "Juno Park", "Mirrorball Society", "Dalia Frost", "Orbit & Ivy", "Echo Harbor", "Felix Amber",
];
const TITLES = [
  "Midnight Signal", "Coastline", "Golden Hour", "Paper Moons", "Body Language", "After the Rain",
  "Neon Garden", "Slow Burn", "Lighthouse", "Satellite Hearts", "Velvet Rooms", "Deep Blue Line",
  "Night Bloom", "Second Sun", "Electric Tide", "Rooftop Fever", "Glass House", "Warm Static",
  "Sway", "Morning Drive", "Undertow", "Starling", "Heat Haze", "Low Light", "Open Water",
  "Silver Lining", "Daydreamer", "Pulse Theory", "Tidal", "Weightless", "Afterglow", "City Lights",
];
const MIXES = ["Extended Mix", "Original Mix", "Club Mix", "Dub", "Edit", "Extended Mix"];
const CRATES = ["Deep House", "Disco Edits", "Tech House", "Warm Up"];
const GENRE_OF: Record<string, string> = {
  "Deep House": "Deep House", "Disco Edits": "Nu Disco", "Tech House": "Tech House", "Warm Up": "House",
};
const KEYS = ["8A", "9A", "5A", "11B", "4A", "10A", "2A", "7B", "6A", "12A", "1A", "3B"];
const FORMATS: [string, number][] = [["aiff", 1411], ["mp3", 320], ["flac", 1018], ["wav", 1411], ["mp3", 320]];

const tracks: TrackRow[] = [];
TITLES.forEach((title, i) => {
  const crate = CRATES[i % CRATES.length];
  const [format, kbps] = FORMATS[i % FORMATS.length];
  const artist = ARTISTS[(i * 5) % ARTISTS.length];
  const full = `${title} (${MIXES[i % MIXES.length]})`;
  const onStick = i % 3 === 0;
  tracks.push({
    id: i + 1, library_id: onStick ? 2 : 1,
    rel_path: `${onStick ? "" : crate + "/"}${artist} - ${full}.${format === "aiff" ? "aiff" : format}`,
    title: full, artist, album: `${title} EP`, genre: GENRE_OF[crate], label: ["Harbor Records", "Neon Pulse", "Slow Lane"][i % 3],
    year: 2019 + (i % 7), bpm: [122, 124, 123, 126, 120, 125, 118, 124][i % 8],
    key_camelot: i === 5 ? null : KEYS[i % KEYS.length], rating: [5, 4, null, 3, 4, 5, null, 2][i % 8],
    comment: null, duration_secs: 330 + ((i * 37) % 160), bitrate_kbps: kbps, format,
    has_artwork: i % 7 !== 3, is_missing: false, added_at: Date.UTC(2026, 9, 10) - i * 86_400_000 * 3,
    modified_at: String(Date.UTC(2026, 9, 12) - i * 86_400_000), key_source: "analysis",
    key_confidence: i % 9 === 4 ? 0.55 : 0.9,
    key_candidates: JSON.stringify([{ key: KEYS[i % KEYS.length], p: 0.8 }, { key: KEYS[(i + 3) % KEYS.length], p: 0.12 }]),
    loudness_lufs: -9 - (i % 5), clip_ratio: 0, cutoff_hz: kbps === 320 ? 19800 : null,
    quality_score: i === 9 ? 38 : i % 6 === 2 ? 68 : 92 + (i % 8), enhanced_at: null, crate_pos: null,
    tempo_drift_pct: 0.3, serato_grid_source: null,
  } as TrackRow);
});
// one duplicate pair for the duplicates view
tracks.push({ ...tracks[1], id: 99, library_id: 2, rel_path: `${tracks[1].artist} - ${tracks[1].title}.mp3`, format: "mp3", bitrate_kbps: 192, quality_score: 51 });

const libraries = [
  { id: 1, name: "Music", root_path: "C:\\Music", is_removable: false, last_scanned_at: "2026-10-15", track_count: tracks.filter((t) => t.library_id === 1).length, volume_serial: null },
  { id: 2, name: "Tracks (DJ STICK)", root_path: "E:\\Tracks", is_removable: true, last_scanned_at: "2026-10-15", track_count: tracks.filter((t) => t.library_id === 2).length, volume_serial: "4A1B2C3D" },
];
const crates = CRATES.map((name) => ({
  path: name, name, depth: 0, track_count: tracks.filter((t) => t.library_id === 1 && t.rel_path.startsWith(name + "/")).length,
}));
const stickIds = tracks.filter((t) => t.library_id === 2).map((t) => t.id);
const seratoCrates = [
  { name: "Friday Set", file: "Friday Set.crate", track_ids: stickIds.slice(0, 7), missing: 0 },
  { name: "Peak Time", file: "Peak Time.crate", track_ids: stickIds.slice(3, 10), missing: 0 },
  { name: "Sunday Brunch", file: "Sunday Brunch.crate", track_ids: stickIds.slice(1, 6), missing: 0 },
];

function listTracks(a: Record<string, unknown>) {
  let rows = tracks.slice();
  const ids = a.trackIds as number[] | null;
  if (ids) rows = ids.map((id) => tracks.find((t) => t.id === id)!).filter(Boolean);
  if (a.libraryId != null) rows = rows.filter((t) => t.library_id === a.libraryId);
  if (typeof a.folder === "string") {
    const f = a.folder as string;
    rows = rows.filter((t) => (t.rel_path.includes("/") ? t.rel_path.slice(0, t.rel_path.lastIndexOf("/")) : "") === f);
  }
  const q = String(a.query ?? "").toLowerCase();
  if (q) rows = rows.filter((t) => `${t.artist} ${t.title} ${t.genre}`.toLowerCase().includes(q));
  if (a.health === "missing_key") rows = rows.filter((t) => !t.key_camelot);
  if (a.health === "low_quality") rows = rows.filter((t) => (t.quality_score ?? 100) < 45);
  const key = a.sort as string | null;
  if (key && key !== "num") {
    const v = (t: TrackRow) => (t as unknown as Record<string, unknown>)[{ key: "key_camelot", time: "duration_secs", quality: "quality_score", bitrate: "bitrate_kbps", added: "added_at" }[key] ?? key];
    rows.sort((x, y) => String(v(x) ?? "").localeCompare(String(v(y) ?? ""), undefined, { numeric: true }));
    if (a.descending) rows.reverse();
  }
  const off = Number(a.offset ?? 0);
  return { rows: rows.slice(off, off + Number(a.limit ?? 500)), total: rows.length };
}

// ---- synthesized audio pictures -------------------------------------------

const BPM = 124;
const BEAT = 60 / BPM;
function groove(t: number) {
  // returns [sample, kick, hats, tonal] for time t (seconds)
  const inBeat = t % BEAT;
  const bar = Math.floor(t / (BEAT * 4));
  const breakdown = bar % 16 >= 12; // a breakdown every 16 bars
  const kick = breakdown ? 0 : Math.exp(-inBeat * 9) * Math.sin(2 * Math.PI * (52 + 90 * Math.exp(-inBeat * 30)) * inBeat);
  const off = (t + BEAT / 2) % BEAT;
  const hat = Math.exp(-off * 60) * (Math.sin(t * 9000) * Math.sin(t * 13100));
  const pad = 0.25 * Math.sin(2 * Math.PI * 220 * t) * (0.6 + 0.4 * Math.sin(t * 0.7)) * (breakdown ? 1.6 : 1);
  return [0.9 * kick + 0.35 * hat + pad, Math.abs(kick), Math.abs(hat) * 2, Math.abs(pad) * 2];
}
function gridPayloads(seconds: number) {
  const ms = Math.floor(seconds * 1000);
  const wave = new Uint8Array(4 + ms * 12);
  const col = new Uint8Array(4 + ms * 5);
  new DataView(wave.buffer).setUint32(0, ms, true);
  new DataView(col.buffer).setUint32(0, ms, true);
  const bands = wave.subarray(4, 4 + ms * 4);
  const shape = new Int8Array(wave.buffer, 4 + ms * 4, ms * 8);
  const drums = col.subarray(4, 4 + ms * 3);
  const harm = col.subarray(4 + ms * 3);
  const chords = [14, 16, 10, 12]; // 8A 9A 6A 7A, two bars each
  for (let i = 0; i < ms; i++) {
    let peak = 0, k = 0, h = 0, p = 0;
    for (let q = 0; q < 4; q++) {
      let lo = 1, hi = -1;
      for (let s = 0; s < 6; s++) {
        const [x, kk, hh, pp] = groove((i + q / 4 + s / 24) / 1000);
        lo = Math.min(lo, x); hi = Math.max(hi, x);
        peak = Math.max(peak, Math.abs(x)); k = Math.max(k, kk); h = Math.max(h, hh); p = Math.max(p, pp);
      }
      const sc = (v: number) => Math.round(Math.sign(v) * Math.sqrt(Math.min(1, Math.abs(v))) * 127);
      shape[i * 8 + q * 2] = sc(lo);
      shape[i * 8 + q * 2 + 1] = sc(hi);
    }
    const b = (v: number) => Math.min(255, Math.round(v * 255));
    bands.set([b(peak), b(k), b(p), b(h)], i * 4);
    drums.set([b(k), b(h), b(p)], i * 3);
    const bar = Math.floor(i / 1000 / (BEAT * 8));
    harm.set([chords[bar % 4], 200], i * 2);
  }
  return { wave: wave.buffer, colours: col.buffer, bands, ms };
}
function overview(bands: Uint8Array, ms: number) {
  const peaks: number[] = [];
  const out: number[] = [];
  for (let b = 0; b < 400; b++) {
    const a = Math.floor((b * ms) / 400), e = Math.floor(((b + 1) * ms) / 400);
    const m = [0, 0, 0, 0];
    for (let i = a; i < e; i++) for (let k = 0; k < 4; k++) m[k] = Math.max(m[k], bands[i * 4 + k]);
    peaks.push(m[0]);
    out.push(m[1], m[2], m[3]);
  }
  return { peaks, bands: out };
}
const synth = gridPayloads(150);
const ov = overview(synth.bands, synth.ms);

// ---- the mocked backend ---------------------------------------------------

const replies: Record<string, (a: Record<string, unknown>) => unknown> = {
  list_libraries: () => libraries,
  list_crates: (a) => (a.libraryId === 1 ? crates : []),
  list_devices: () => [{ letter: "E:", label: "DJ STICK", serial: "4A1B2C3D", known_library_id: 2 }],
  list_tracks: listTracks,
  library_health: () => ({ duplicates: 2, missing_bpm: 0, missing_key: 1, missing_artwork: 4, missing_file: 0, analysis_error: 0, low_quality: 1, unsteady_tempo: 0 }),
  list_serato_crates: (a) => (a.letter === "E:" ? seratoCrates : []),
  list_rekordbox_crates: () => [],
  list_prepared_rekordbox: () => [],
  list_genres: () => [{ genre: "Deep House", count: 8 }, { genre: "Tech House", count: 8 }, { genre: "Nu Disco", count: 8 }, { genre: "House", count: 8 }],
  analyzer_available: () => true,
  apollo_ready: () => false,
  artwork_path: (a) => (tracks.find((t) => t.id === a.trackId)?.has_artwork ? `covers/${((a.trackId as number) - 1) % 8}.svg` : null),
  get_waveform: () => ov.peaks,
  waveform_bands: () => ov.bands,
  beat_grid: () => null,
  downbeat_grid: () => null,
  prepare_playback: () => ({ path: "silence.wav", transcoded: false }),
  track_grid: () => ({ sections: [{ start_sec: 0, bpm: BPM }], source: "detected", file_unreadable: false }),
  grid_waveform: () => synth.wave,
  grid_colours: () => synth.colours,
  find_duplicates_in_set: (a) => replies.find_duplicates(a),
  find_duplicates: () => ({
    unhashed: 0,
    groups: [{
      level: "same_tags", suggested_keeper_id: 2,
      tracks: [tracks[1], tracks.find((t) => t.id === 99)!].map((t) => ({
        id: t.id, title: t.title, artist: t.artist, rel_path: t.rel_path, bitrate_kbps: t.bitrate_kbps,
        format: t.format, file_size: (t.bitrate_kbps ?? 320) * 1000 * (t.duration_secs ?? 300) / 8, duration_secs: t.duration_secs, quality_score: t.quality_score,
      })),
    }],
  }),
};

(window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: Record<string, unknown> = {}) => {
    if (cmd in replies) return replies[cmd](args);
    if (cmd.startsWith("plugin:event|")) return 1;
    if (cmd.startsWith("list_")) return [];
    return null;
  },
  transformCallback: () => Math.floor(Math.random() * 1e9),
  unregisterCallback: () => {},
  convertFileSrc: (p: string) => p,
};
(window as unknown as { __TAURI_EVENT_PLUGIN_INTERNALS__: unknown }).__TAURI_EVENT_PLUGIN_INTERNALS__ = {
  unregisterListener: () => {},
};

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
