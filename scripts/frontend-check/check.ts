// Assertions for small pure helpers outside the grid editor: key-candidate parsing
// (src/api.ts), the visualizer math (src/viz/engine.ts) and the split view (src/splitView.ts). Run from the repo root:
//   npx esbuild scripts/frontend-check/check.ts --bundle --platform=node \
//     --outfile=scripts/frontend-check/check.mjs --format=esm && node scripts/frontend-check/check.mjs
import { parseKeyCandidates } from "../../src/api";
import { bandEnergies, beatPhase, camelotPalette } from "../../src/viz/engine";
import {
  centerOffset,
  cleanCols,
  cycleSort,
  dragIds,
  moveCol,
  sourceKey,
  sourceOptions,
  cleanFracs3,
  isEditableSource,
  resizeThree,
  SPLIT_PANE_MIN,
  SPLIT_DEFAULT_COLS,
  toggleCol,
} from "../../src/splitView";

let failed = 0;
function ok(cond: boolean, msg: string) {
  if (!cond) {
    failed++;
    console.error("FAIL:", msg);
  }
}
const near = (a: number, b: number, eps = 1e-9) => Math.abs(a - b) <= eps;

// parseKeyCandidates: anything malformed is an empty list, never a throw
{
  ok(parseKeyCandidates(null).length === 0, "null is empty");
  ok(parseKeyCandidates("").length === 0, "empty string is empty");
  ok(parseKeyCandidates("{not json").length === 0, "broken JSON is empty");
  ok(parseKeyCandidates("null").length === 0, "JSON null is empty");
  ok(parseKeyCandidates('{"key":"8A","p":0.5}').length === 0, "an object, not an array, is empty");
  ok(parseKeyCandidates("42").length === 0, "a number is empty");
  const mixed = parseKeyCandidates(
    '[{"key":"8A","p":0.54},null,{"p":0.3},{"key":5,"p":0.1},{"key":"2A"},{"key":"3A","p":"x"},{"key":"9B","p":0.12}]',
  );
  ok(mixed.length === 2, `only well-formed entries survive, got ${mixed.length}`);
  ok(mixed[0].key === "8A" && mixed[1].key === "9B", "order kept, winner first");
}

// beatPhase
{
  ok(beatPhase(10, null, 0) === 0, "no BPM is phase 0");
  ok(beatPhase(10, 0, 0) === 0, "zero BPM is phase 0");
  ok(beatPhase(NaN, 120, 0) === 0, "NaN time is phase 0");
  ok(beatPhase(Infinity, 120, 0) === 0, "infinite time is phase 0");
  ok(near(beatPhase(0.25, 120, 0), 0.5), "half a beat in at 120 BPM");
  ok(near(beatPhase(1.0, 120, 0), 0), "on the beat is 0");
  ok(near(beatPhase(0.1, 120, 0.35), 0.5), "before the anchor wraps into [0,1)");
  for (const t of [0, 0.3, 7.77, 123.4]) {
    const p = beatPhase(t, 127.3, 0.2);
    ok(p >= 0 && p < 1, `phase in [0,1) at ${t}, got ${p}`);
  }
}

// bandEnergies
{
  const silent = bandEnergies(new Uint8Array(1024), 44100, 2048);
  ok(silent.bass === 0 && silent.mid === 0 && silent.treble === 0 && silent.level === 0, "silence is 0");
  const full = bandEnergies(new Uint8Array(1024).fill(255), 44100, 2048);
  ok(near(full.bass, 1) && near(full.mid, 1) && near(full.treble, 1) && near(full.level, 1), "full scale is 1");
  // energy only below 150 Hz: bass high, treble zero
  const binHz = 44100 / 2048;
  const low = new Uint8Array(1024);
  for (let i = 0; i < Math.floor(140 / binHz); i++) low[i] = 255;
  const b = bandEnergies(low, 44100, 2048);
  ok(b.bass > 0.5 && b.treble === 0, `bass-only spectrum: bass ${b.bass}, treble ${b.treble}`);
}

// camelotPalette: three colours in range, unknown key falls back to the accent hue
{
  const inRange = (c: number[]) => c.length === 3 && c.every((v) => v >= 0 && v <= 1 && isFinite(v));
  for (const k of ["1A", "8A", "12B", null, undefined, "bogus"]) {
    const pal = camelotPalette(k);
    ok(pal.length === 3 && pal.every(inRange), `palette in range for ${k}`);
  }
  const unknown = camelotPalette(null)[0];
  ok(unknown[1] > unknown[0] && unknown[2] > unknown[0], "unknown key is turquoise-ish (green/blue over red)");
  ok(camelotPalette("8A")[0].join() !== camelotPalette("2B")[0].join(), "different keys, different base colours");
}

// split view columns: at most six, never empty, known keys only
{
  const def = SPLIT_DEFAULT_COLS.join();
  ok(def.includes("added"), "Added is shown by default");
  ok(cleanCols(null).join() === def, "nothing stored is the default");
  ok(cleanCols("x").join() === def, "garbage is the default");
  ok(cleanCols([]).join() === def, "an empty list is the default");
  ok(cleanCols(["num", "library", "bogus"]).join() === def, "# and library are not choices");
  ok(cleanCols(["key", "key", "bpm"]).join() === "key,bpm", "duplicates dropped, order kept");
  ok(cleanCols(["title", "key", "bpm", "time"]).join() === def, "the old default becomes the new one");
  ok(cleanCols(["title", "key", "bpm", "time", "genre", "album", "year"]).length === 6, "cut to six");
  const four = ["title", "key", "bpm", "time"];
  ok(toggleCol(four, "genre").join() === "title,key,bpm,time,genre", "a fifth column is added");
  const six = ["title", "key", "bpm", "time", "genre", "album"];
  ok(toggleCol(six, "year") === six, "a seventh column is refused");
  ok(toggleCol(four, "bpm").join() === "title,key,time", "unticking removes");
  ok(toggleCol(["key"], "genre").join() === "key,genre", "ticking appends");
  ok(toggleCol(["key"], "key").join() === "key", "the last column stays");
  ok(moveCol(four, "time", "title").join() === "time,title,key,bpm", "drag to the front");
  ok(moveCol(four, "num", "title") === four, "a column not in the pane moves nothing");
  ok(moveCol(four, null, "title") === four, "no drag moves nothing");
  ok(JSON.stringify(cycleSort(null, "key")) === '{"key":"key","desc":false}', "first click ascending");
  ok(JSON.stringify(cycleSort({ key: "key", desc: false }, "key")) === '{"key":"key","desc":true}', "second descending");
  ok(cycleSort({ key: "key", desc: true }, "key") === null, "third back to default");
  ok(JSON.stringify(cycleSort({ key: "key", desc: true }, "bpm")) === '{"key":"bpm","desc":false}', "another column starts ascending");
  ok(centerOffset(0, 42, 30, 400) === 0, "the first row never scrolls negative");
  ok(centerOffset(100, 42, 30, 400) === 30 + 4200 + 21 - 200, "row 100 lands mid-viewport");
}

// split view sources: the picker list and what a drag carries
{
  const opts = sourceOptions(
    [{ id: 1, name: "Music" }],
    new Map([[1, [{ path: "House", track_count: 3 }, { path: "House/Empty", track_count: 0 }, { path: "House/Deep", track_count: 2 }]]]),
    new Map([["D:", [{ name: "Summer", file: "Summer.crate", track_ids: [4] }, { name: "Nothing", file: "Nothing.crate", track_ids: [] }]]]),
    new Map([["D:", [{ id: 7, name: "Folder", is_folder: true, track_ids: [] }, { id: 8, name: "Peak", is_folder: false, track_ids: [5] }]]]),
  );
  const keys = opts.map((o) => o.key);
  ok(keys[0] === "crate", "the open crate is offered first");
  ok(JSON.stringify(keys) === JSON.stringify(["crate", "lib:1", "folder:1:House", "folder:1:House/Empty", "folder:1:House/Deep", "serato:D::Summer.crate", "serato:D::Nothing.crate", "rbx:D::8"]),
    `empty crates stay, Rekordbox folders are left out, got ${keys.join(",")}`);
  ok(opts[4].label === "House › Deep", "nested crates read as a path");
  ok(new Set(keys).size === keys.length, "every option has its own key");
  ok(sourceKey({ kind: "serato", letter: "D:", file: "a.crate" }) === "serato:D::a.crate", "serato key");
  ok(JSON.stringify(dragIds(2, new Set([1, 2, 3]), new Set([3]))) === "[1,2]", "a selected row drags the selection, minus crate tracks");
  ok(JSON.stringify(dragIds(9, new Set([1, 2]), new Set())) === "[9]", "an unselected row drags alone");
  ok(dragIds(3, new Set(), new Set([3])).length === 0, "a track already in the crate drags nothing");
}

// three panes: the running order's menu, editable sources, and sizing
{
  const noThis = sourceOptions([{ id: 1, name: "Music" }], new Map(), new Map(), new Map(), false);
  ok(noThis.every((o) => o.key !== "crate"), "the running order's menu has no This crate");
  ok(isEditableSource({ kind: "folder", libraryId: 1, path: "A" }), "a folder crate is editable");
  ok(isEditableSource({ kind: "serato", letter: "D:", file: "a.crate" }), "a Serato crate is editable");
  ok(!isEditableSource({ kind: "library", libraryId: 1 }), "a library is read-only");
  ok(!isEditableSource({ kind: "rekordbox", letter: "D:", id: 1 }), "a Rekordbox playlist is read-only");
  ok(!isEditableSource({ kind: "crate" }), "This crate is the running order, not a second editor");
  const even = cleanFracs3(null);
  ok(near(even[0] + even[1] + even[2], 1) && near(even[0], 1 / 3), "nothing stored is an even split");
  ok(near(cleanFracs3([0.05, 0.45, 0.5])[0], 1 / 3), "a share under the minimum resets");
  ok(near(cleanFracs3([0.2, 0.3, 0.5])[1], 0.3), "a valid split is kept");
  const r0 = resizeThree([0.3, 0.3, 0.4], 0, 0.4);
  ok(near(r0[0], 0.4) && near(r0[1], 0.2) && near(r0[2], 0.4), "divider 0 moves only panes 1 and 2");
  const r1 = resizeThree([0.3, 0.3, 0.4], 1, 0.5);
  ok(near(r1[0], 0.3) && near(r1[1], 0.2) && near(r1[2], 0.5), "divider 1 moves only panes 2 and 3");
  const clamp = resizeThree([0.3, 0.3, 0.4], 0, 0.9);
  ok(near(clamp[1], SPLIT_PANE_MIN) && near(clamp[0] + clamp[1], 0.6), "a pane never goes below the minimum");
  const clamp2 = resizeThree([0.3, 0.3, 0.4], 1, 0.1);
  ok(near(clamp2[1], SPLIT_PANE_MIN) && near(clamp2[0], 0.3), "divider 1 cannot squeeze pane 2 away");
}

if (failed) {
  console.error(`FRONTEND-FAIL ${failed}`);
  process.exit(1);
}
console.log("FRONTEND-OK");
