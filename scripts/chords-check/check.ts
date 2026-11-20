// Assertions for src/chords.ts. Run from the repo root:
//   npx esbuild scripts/chords-check/check.ts --bundle --platform=node \
//     --outfile=scripts/chords-check/check.mjs --format=esm && node scripts/chords-check/check.mjs
import { chordSegments, NO_CHORD } from "../../src/chords";

let failed = 0;
function ok(cond: boolean, msg: string) {
  if (!cond) {
    failed++;
    console.error("FAIL:", msg);
  }
}

/** Harmony bytes [camelot, confidence] per ms from runs of [ms, code, conf]. */
function harmony(runs: [number, number, number][]): Uint8Array {
  const total = runs.reduce((n, r) => n + r[0], 0);
  const h = new Uint8Array(total * 2);
  let at = 0;
  for (const [len, code, conf] of runs) {
    for (let i = 0; i < len; i++, at++) {
      h[at * 2] = code;
      h[at * 2 + 1] = conf;
    }
  }
  return h;
}
const show = (s: { start: number; end: number; code: number }[]) =>
  s.map((x) => `${x.start}-${x.end}:${x.code === NO_CHORD ? "-" : x.code}`).join(" ");

// steady stretches come out as they are
{
  const s = chordSegments(harmony([[3000, 14, 200], [3000, 15, 200]]));
  ok(show(s) === "0-3000:14 3000-6000:15", `two chords, got ${show(s)}`);
}
// a blip shorter than half a second folds into the longer neighbour
{
  const s = chordSegments(harmony([[3000, 14, 200], [200, 3, 200], [2800, 15, 200]]));
  ok(show(s) === "0-3200:14 3200-6000:15", `the 200 ms blip joins its longer neighbour, got ${show(s)}`);
}
// a blip between two stretches of the same chord disappears into it
{
  const s = chordSegments(harmony([[2000, 14, 200], [300, 9, 200], [2000, 14, 200]]));
  ok(show(s) === "0-4300:14", `same chord either side: one stretch, got ${show(s)}`);
}
// low confidence is no chord: a long unclear stretch stays a gap
{
  const s = chordSegments(harmony([[2000, 14, 200], [1500, 14, 20], [2000, 17, 200]]));
  ok(show(s) === "0-2000:14 2000-3500:- 3500-5500:17", `a clear gap, got ${show(s)}`);
}
// silence is no chord too
{
  const s = chordSegments(harmony([[1000, NO_CHORD, 0], [2000, 14, 200]]));
  ok(show(s) === "0-1000:- 1000-3000:14", `silence first, got ${show(s)}`);
}
// a flicker of many short runs settles to the chord that dominates
{
  const runs: [number, number, number][] = [];
  for (let i = 0; i < 40; i++) runs.push([46, i % 3 === 0 ? 5 : 14, 200]);
  const s = chordSegments(harmony(runs));
  ok(s.length === 1 && s[0].code === 14, `flicker settles to 14, got ${show(s)}`);
}
// a short unclear moment inside a progression does not break the lane
{
  const s = chordSegments(harmony([[2000, 14, 200], [800, 14, 20], [2000, 15, 200]]));
  ok(show(s) === "0-2800:14 2800-4800:15", `an 800 ms gap folds into the chord before it, got ${show(s)}`);
}
ok(chordSegments(new Uint8Array(0)).length === 0, "nothing in, nothing out");

if (failed) {
  console.error(`${failed} chords check(s) failed`);
  process.exit(1);
}
console.log("CHORDS-OK");
