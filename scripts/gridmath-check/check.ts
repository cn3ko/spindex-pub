// Assertions for src/gridMath.ts. Run from the repo root:
//   npx esbuild scripts/gridmath-check/check.ts --bundle --platform=node \
//     --outfile=scripts/gridmath-check/check.mjs --format=esm && node scripts/gridmath-check/check.mjs
import {
  addMarker, barOneAt, barOneAtMap, linesIn, loopStep, makeLoop, mapLinesIn, mapNearestLine,
  moveSection, removeMarker, retempoSection, sameMap, scrubTime, sectionAt, sectionBeats, stretchBetween, stretchHeld, stretchSection,
  stretchTo, validMap, type TempoMap, nearestLine, period, rescaleAround, shift, tapBpm, type Grid,
} from "../../src/gridMath";
import { canRedo, canUndo, commitHistory, emptyHistory, redoHistory, undoHistory } from "../../src/gridHistory";

let failed = 0;
function ok(cond: boolean, msg: string) {
  if (!cond) {
    failed++;
    console.error("FAIL:", msg);
  }
}
const near = (a: number, b: number, eps = 1e-9) => Math.abs(a - b) <= eps;

const g: Grid = { anchor: 0.05, bpm: 120 }; // 0.5 s per beat

// lines far from the anchor, bar-1 flagged every 4 beats
{
  const ls = linesIn(g, 240.0, 242.0);
  ok(ls.length === 4, `4 lines in 2 s at 120 BPM, got ${ls.length}`);
  ok(near(ls[0].t, 240.05), `first line 240.05, got ${ls[0].t}`);
  ok(ls.filter((l) => l.bar1).length === 1, "exactly one bar-1 in 2 s");
  const bar1 = ls.find((l) => l.bar1)!;
  ok(near(((bar1.t - g.anchor) / period(g)) % 4, 0, 1e-6), "bar-1 on a multiple of 4 beats");
}
// a negative anchor and a window before it
{
  const neg: Grid = { anchor: -0.3, bpm: 120 };
  const ls = linesIn(neg, -1, 0.5);
  ok(near(ls[0].t, -0.8) && near(ls[1].t, -0.3) && near(ls[2].t, 0.2), "lines around a negative anchor");
  ok(ls[1].bar1 && !ls[0].bar1, "the anchor itself is bar 1");
}
// nearestLine and barOneAt
{
  ok(near(nearestLine(g, 10.2), 10.05), "10.2 is nearest 10.05");
  ok(near(nearestLine(g, 10.31), 10.55), "10.31 is nearest 10.55");
  const moved = barOneAt(g, 10.31);
  ok(near(moved.anchor, 10.55) && moved.bpm === 120, "bar 1 moves to the nearest line");
  const before = linesIn(g, 10, 12).map((l) => l.t);
  const after = linesIn(moved, 10, 12).map((l) => l.t);
  ok(before.length === after.length && before.every((t, i) => near(t, after[i])), "no line moves");
}
// shift
ok(near(shift(g, -0.001).anchor, 0.049), "shift by -1 ms");
// rescaleAround keeps the pivot line fixed
{
  const pivot = nearestLine(g, 120.2); // 120.05
  const r = rescaleAround(g, 121, 120.2);
  ok(r.bpm === 121, "new bpm");
  ok(linesIn(r, pivot - 0.01, pivot + 0.01).some((l) => near(l.t, pivot, 1e-6)), "pivot line stays put");
  const beatsFromAnchor = Math.round((pivot - g.anchor) / period(g));
  const newBeats = Math.round((pivot - r.anchor) / period(r));
  ok(beatsFromAnchor === newBeats, "the pivot keeps its beat number, so bar 1 does not shift");
}
// a BPM "change" below display precision is no change at all
{
  const same = rescaleAround(g, g.bpm + 1e-6, 120.2);
  ok(same === g, "a sub-0.005 BPM change returns the same grid, so nothing turns dirty");
  const f32: Grid = { anchor: 0.05, bpm: 127.98999786376953 }; // 127.99 stored as f32
  ok(rescaleAround(f32, 127.99, 60) === f32, "retyping the shown 127.99 changes nothing");
}
// tap tempo
{
  ok(tapBpm([0, 500, 1000]) === null, "3 taps are not enough");
  const t = tapBpm([0, 500, 1000, 1500, 2000]);
  ok(t !== null && near(t, 120, 1e-9), `5 taps at 500 ms = 120, got ${t}`);
  const reset = tapBpm([0, 500, 1000, 1500, 9000, 9500, 10000, 10500]);
  ok(reset !== null && near(reset, 120, 1e-9), "a gap over 2 s starts a new sequence");
  ok(tapBpm([0, 500, 1000, 5000]) === null, "after a reset only 1 tap remains");
}

// A-B loop: marks in either order, too-short loops refused
{
  const l = makeLoop(12.5, 10.0)!;
  ok(l !== null && l.start === 10.0 && l.end === 12.5, "marks are ordered");
  ok(makeLoop(10.0, 10.02) === null, "a loop under 50 ms is no loop");
  ok(makeLoop(10.0, Number.NaN) === null, "an unset mark is no loop");
}
// A-B loop: what to do at playback time t
{
  const l = makeLoop(10.0, 12.0)!;
  ok(loopStep(11.0, l) === null, "inside the loop: keep playing");
  ok(loopStep(9.0, l) === null, "before it: play into it");
  ok(loopStep(12.0, l) === "wrap", "at Out: back to In");
  ok(loopStep(12.03, l) === "wrap", "a frame past Out: back to In");
  ok(loopStep(15.0, l) === "exit", "seeked well past Out: the loop ends rather than fighting the seek");
}

// dragging the waveform pulls the track like a map, kept inside the track
{
  ok(near(scrubTime(30, -100, 0.01, 180), 31), "drag left 100 px at 10 ms/px: 1 s later");
  ok(near(scrubTime(30, 50, 0.01, 180), 29.5), "drag right: earlier");
  ok(scrubTime(0.2, 100, 0.01, 180) === 0, "not before the start");
  ok(scrubTime(179.5, -100, 0.01, 180) === 180, "not past the end");
}

// stretching: the line at the pivot stays, the grabbed line follows the hand
{
  const g0: Grid = { anchor: 0.05, bpm: 120 }; // lines every 0.5 s: 10.05, 10.55, ...
  const pivot = 10.05;
  // grab the line 8 beats later (14.05) and pull it 40 ms later: the grid widens
  const g1 = stretchTo(g0, pivot, 14.05, 14.09)!;
  ok(g1 !== null, "a stretch over 8 beats works");
  ok(g1.bpm < 120, `a wider grid is a slower tempo, got ${g1 !== null && g1.bpm}`);
  ok(near(nearestLine(g1, pivot), pivot, 1e-6), "the pivot line has not moved");
  const grabbed = linesIn(g1, 14.0, 14.2).map((l) => l.t);
  ok(grabbed.some((t) => Math.abs(t - 14.09) < 0.001), `the grabbed line lands on the hand, got ${grabbed}`);
  ok(near(g1.bpm * 100, Math.round(g1.bpm * 100), 1e-6), "tempo in 0.01 BPM steps, like the field");
  // pulling it earlier narrows the grid
  ok(stretchTo(g0, pivot, 14.05, 14.0)!.bpm > 120, "a narrower grid is a faster tempo");
  // grabbing at the pivot, or dragging across it, is no stretch
  ok(stretchTo(g0, pivot, 10.1, 10.3) === null, "too close to the pivot to stretch");
  ok(stretchTo(g0, pivot, 14.05, 9.0) === null, "dragging across the pivot is no stretch");
  // works on the other side of the pivot too
  const g2 = stretchTo(g0, pivot, 6.05, 6.01)!;
  ok(g2 !== null && g2.bpm < 120, "pulling a line before the pivot further away widens too");
  // the tempo stays within the editor's range
  ok(stretchTo(g0, pivot, 10.65, 30) === null || stretchTo(g0, pivot, 10.65, 30)!.bpm >= 40, "never below 40 BPM");
}

// tempo maps: sections with their own tempo, bar numbering running on
{
  const one: TempoMap = [{ start: 0.05, bpm: 120 }];
  const plain = linesIn({ anchor: 0.05, bpm: 120 }, 200, 210).map((l) => `${l.t.toFixed(6)}${l.bar1}`);
  const asMap = mapLinesIn(one, 200, 210).map((l) => `${l.t.toFixed(6)}${l.bar1}`);
  ok(plain.join() === asMap.join(), "a one-section map draws exactly the constant grid");

  // a marker at 30.0 snaps to the nearest beat (30.05) and changes nothing before it
  const two = addMarker(one, 30.02)!;
  ok(two !== null && two.length === 2, "adding a marker splits the map");
  ok(near(two[1].start, 30.05), `the marker snaps to a beat line, got ${two && two[1].start}`);
  const before = (m: TempoMap) => mapLinesIn(m, 0, 30.0).map((l) => `${l.t.toFixed(6)}${l.bar1}`).join();
  ok(before(one) === before(two), "lines before the marker are unchanged");
  ok(addMarker(two, 30.05) === null, "no second marker on a marker");

  // the later section takes its own tempo, pivoting on its marker
  const faster = retempoSection(two, 1, 125, two[1].start)!;
  ok(faster !== null && near(faster[1].start, 30.05) && faster[1].bpm === 125, "the later section is 125 BPM from its marker");
  ok(before(two) === before(faster), "the section before stays exactly as it was");
  const after = mapLinesIn(faster, 30.0, 32.0).map((l) => l.t);
  ok(near(after[1] - after[0], 60 / 125), "lines after the marker are 125 BPM apart");
  // bars restart at each marker; 60 beats before it make 15 whole bars
  ok(mapLinesIn(faster, 30.0, 30.1)[0].bar1 === true, "60 beats in, the marker is bar 1");
  ok(mapLinesIn(faster, 30.0, 30.1)[0].beat === 60, "lines carry their running beat number");
  ok(mapLinesIn(faster, 30.5, 30.6)[0].beat === 61, "...counting on after the marker");
  ok(mapLinesIn(one, -1.0, 0.1)[0].beat === -2, "...negative before the first bar");

  // moving the later section re-fits the one before: whole beats still fit
  const moved = moveSection(faster, 1, 0.02)!;
  ok(moved !== null && near(moved[1].start, 30.07), "the marker moved 20 ms");
  ok(sectionBeats(moved, 0) === sectionBeats(faster, 0), "the section before keeps its beat count");
  ok(near(moved[0].bpm, (sectionBeats(moved, 0) * 60) / (30.07 - 0.05)), "...and re-fits its tempo to the new span");
  ok(moved[1].bpm === 125, "the moved section keeps its own tempo");

  // the section at a time, and which line is nearest
  ok(sectionAt(faster, 10) === 0 && sectionAt(faster, 40) === 1 && sectionAt(faster, -5) === 0, "sectionAt");
  ok(near(mapNearestLine(faster, 30.06), 30.05), "the marker itself is the nearest line");

  // stretching a later section pivots on its marker
  const st = stretchSection(faster, 1, 30.05, 30.05 + 8 * 0.48, 30.05 + 8 * 0.48 + 0.04)!;
  ok(st !== null && st[1].bpm < 125 && near(st[1].start, 30.05), "stretching the later section keeps its marker");
  ok(before(st) === before(faster), "...and leaves the section before alone");

  // removing the marker merges back
  const merged = removeMarker(faster, 40)!;
  ok(merged !== null && merged.length === 1 && merged[0].bpm === 120, "removing the last marker leaves the first section");
  ok(removeMarker(one, 10) === null, "the first section has no marker to remove");

  // a marker between two others: the middle section keeps whole beats both ways
  const three = addMarker(addMarker(one, 30.02)!, 60.02)!;
  ok(three.length === 3 && sectionBeats(three, 0) === 60 && sectionBeats(three, 1) === 60, "three sections, 60 beats each");
  const mid = removeMarker(three, 40)!;
  ok(mid.length === 2 && sectionBeats(mid, 0) === 120 && near(mid[1].start, 60.05), "removing the middle marker joins its beats");

  // bar 1 here: renumbers, moves no line
  const b1 = barOneAtMap(faster, 31.03)!; // 31.01 is beat 2 of the section after the marker
  const allLines = (m: TempoMap) => mapLinesIn(m, 0, 60).map((l) => l.t.toFixed(6)).join();
  ok(allLines(b1) === allLines(faster), "bar 1 here moves no line");
  const at = mapLinesIn(b1, 30.9, 31.1).find((l) => Math.abs(l.t - mapNearestLine(faster, 31.03)) < 1e-6)!;
  ok(at !== undefined && at.bar1, "the beat nearest the playhead is bar 1 now");

  // stretching between two markers: both stay, only the beat count between them changes
  {
    const m3 = addMarker(addMarker(one, 30.02)!, 60.02)!; // 0.05 | 30.05 | 60.05, 60 beats each at 120
    const linesAfter = (m: TempoMap) => mapLinesIn(m, 60.05, 90).map((l) => l.t.toFixed(6)).join();
    // grab beat 30 of the middle section (45.05) and pull it 1 s later: slower,
    // 60 * 15 / 16 = 56.25 beats wanted -> a whole bar less, 56
    const sb = stretchBetween(m3, 1, 45.05, 46.05)!;
    ok(sb !== null && sectionBeats(sb, 1) === 56, "dragging later drops a whole bar between the markers");
    ok(near(sb[1].start, 30.05) && near(sb[2].start, 60.05), "both markers stay put");
    ok(near(sb[1].bpm, (56 * 60) / 30), "the tempo is the whole beats over the fixed span");
    const bars = (m: TempoMap, a: number, b: number) =>
      mapLinesIn(m, a, b).map((l) => `${l.t.toFixed(6)}${l.bar1}`).join();
    ok(bars(sb, 60.05, 90) === bars(m3, 60.05, 90), "every bar line after B stays where it was");
    ok(bars(sb, 0, 30.0) === bars(m3, 0, 30.0), "and every bar line before A");
    ok(stretchBetween(m3, 1, 45.05, 45.55) === m3, "under half a bar of change is no change");
    ok(sb[0].bpm === 120 && sb[2].bpm === 120 && linesAfter(sb) === linesAfter(m3), "the lines after the second marker are untouched");
    const faster2 = stretchBetween(m3, 1, 45.05, 44.05)!; // 64.3 wanted -> 64
    ok(faster2 !== null && sectionBeats(faster2, 1) === 64 && near(faster2[2].start, 60.05), "dragging earlier adds a whole bar");
    // a section whose beats are not whole bars keeps its remainder, so bars after it still stay
    const odd = addMarker(addMarker(one, 30.02)!, 35.02)!; // 10 beats between
    const oddLess = stretchBetween(odd, 1, 34.05, 36.05)!; // 10 * 4 / 6 = 6.7 -> 6
    ok(oddLess !== null && sectionBeats(oddLess, 1) === 6, "10 beats step to 6, not 7");
    ok(bars(oddLess, 35.05, 60) === bars(odd, 35.05, 60), "bars after an odd section stay too");
    ok(stretchBetween(m3, 2, 70, 71) === null, "the last section has no second marker");
    ok(stretchBetween(m3, 1, 30.1, 30.5) === null, "no lever right at the marker");
    // the first section: its start and the first marker stay
    const s0 = stretchBetween(m3, 0, 15.05, 16.05)!;
    ok(s0 !== null && near(s0[0].start, 0.05) && near(s0[1].start, 30.05) && sectionBeats(s0, 0) === 56, "first section between its start and the marker");
  }

  // the stretch the editor uses: no marker ever moves
  {
    const m3 = addMarker(addMarker(one, 30.02)!, 60.02)!; // 0.05 | 30.05 | 60.05
    const markers = (m: TempoMap) => m.slice(1).map((s) => s.start.toFixed(6)).join();
    // a middle section: both markers stay, whole beats step
    const mid = stretchHeld(m3, 1, 30.05, 45.05, 46.05)!;
    ok(mid !== null && markers(mid) === markers(m3) && sectionBeats(mid, 1) === 56, "a middle section keeps both markers");
    // the first section: its marker stays, the tempo stretches smoothly, its start follows
    const first = stretchHeld(m3, 0, 15, 15.05, 15.15)!;
    ok(first !== null && markers(first) === markers(m3), "the first section keeps its marker");
    ok(sectionBeats(first, 0) === 60 && first[0].bpm !== 120 && first[0].start !== m3[0].start, "smooth tempo, same beats, start follows");
    ok(near((first[1].start - first[0].start) * first[0].bpm / 60, 60, 1e-6), "still whole beats to the marker");
    ok(m3[2].bpm === first[2].bpm && m3[1].bpm === first[1].bpm, "later sections untouched");
    // the last section: pivots on its own marker, nothing after it to move
    const last = stretchHeld(m3, 2, 70, 68.05, 68.15)!;
    ok(last !== null && markers(last) === markers(m3) && last[2].bpm !== 120, "the last section stretches on its marker");
    // a map with no markers: as before, around the playhead's line
    const single = stretchHeld(one, 0, 10.05, 14.05, 14.15)!;
    ok(single !== null && near(mapNearestLine(single, 10.05), 10.05, 1e-6), "no markers: the playhead's line stays");
  }

  // every tempo marker is a downbeat (as in Serato): bars restart there
  {
    const odd = addMarker(one, 5.05)!; // 10 beats before the marker: not a multiple of 4
    const at = mapLinesIn(odd, 5.0, 5.1)[0];
    ok(at.bar1, "a marker 10 beats in is still a bar 1");
    ok(at.beat === 12, `bar numbers go on after the short bar: bar 4 at the marker, got beat ${at.beat}`);
    // so changing the beats between two markers moves no bar after the second one
    const m3 = addMarker(addMarker(one, 30.02)!, 60.02)!;
    const barsAfter = (m: TempoMap) => mapLinesIn(m, 60.05, 90).map((l) => `${l.t.toFixed(6)}${l.bar1}`).join();
    const fewer = stretchBetween(m3, 1, 45.05, 46.05)!; // 60 -> 56 beats
    ok(sectionBeats(fewer, 1) === 56 && barsAfter(fewer) === barsAfter(m3), "bars after B stay when the beats between change");
    const barsBefore = (m: TempoMap) => mapLinesIn(m, 0, 30.0).map((l) => `${l.t.toFixed(6)}${l.bar1}`).join();
    ok(barsBefore(fewer) === barsBefore(m3), "bars before A stay too");
  }
  // bar 1 here in a later section: a marker on that beat (each marker is a downbeat), no line moves
  {
    const two2 = addMarker(one, 30.02)!;
    const lines = (m: TempoMap) => mapLinesIn(m, 0, 60).map((l) => l.t.toFixed(6)).join();
    const b = barOneAtMap(two2, 31.07)!; // 31.05 is beat 3 after the marker at 30.05
    ok(b !== null && b.length === 3 && near(b[2].start, 31.05), "a marker lands on that beat");
    ok(lines(b) === lines(two2), "no line moves");
    ok(barOneAtMap(two2, 30.06) === two2, "on the marker itself it already is bar 1");
    // setting bar 1 in one part never moves the bars of another
    const flags = (m: TempoMap, a: number, z: number) =>
      mapLinesIn(m, a, z).map((l) => `${l.t.toFixed(6)}${l.bar1}`).join();
    ok(flags(b, 0, 30.0) === flags(two2, 0, 30.0), "bar 1 after the marker leaves the bars before it");
    const early = barOneAtMap(two2, 1.07)!; // beat 2 of the first part
    ok(early !== null && flags(early, 30.05, 60) === flags(two2, 30.05, 60), "bar 1 before the marker leaves the bars after it");
    ok(mapLinesIn(early, 1.0, 1.1)[0].bar1, "and is bar 1 where it was set");
  }

  // nonsense is refused
  ok(!validMap([{ start: 0, bpm: 120 }, { start: 0.1, bpm: 120 }]), "markers under a beat apart");
  ok(!validMap([{ start: 0, bpm: 120 }, { start: 10, bpm: 300 }]), "a section out of range");
  ok(validMap(faster), "a sound map passes");
  ok(sameMap(faster, faster.map((s) => ({ ...s }))) && !sameMap(faster, moved), "sameMap");
}

// undo / redo: one step per finished edit
{
  const A: TempoMap = [{ start: 0.1, bpm: 120 }];
  const B: TempoMap = [{ start: 0.2, bpm: 120 }];
  const C: TempoMap = [{ start: 0.2, bpm: 124 }];
  const D: TempoMap = [{ start: 0.3, bpm: 124 }];
  let h = commitHistory(emptyHistory, A, 0, false); // the starting grid is no step
  ok(!canUndo(h) && !canRedo(h), "the starting grid has nothing to undo");
  h = commitHistory(h, B, 1000, false);
  h = commitHistory(h, C, 3000, false);
  ok(h.past.length === 2, "two edits, two steps");
  ok(commitHistory(h, C, 3100, true) === h, "the same grid again is no step");
  const u1 = undoHistory(h, C)!;
  ok(u1 !== null && sameMap(u1.grid, B) && canRedo(u1.history), "back gives the grid before");
  const u2 = undoHistory(u1.history, u1.grid)!;
  ok(sameMap(u2.grid, A) && !canUndo(u2.history), "back again gives the start, then nothing");
  ok(undoHistory(u2.history, u2.grid) === null, "nothing more to undo");
  const r1 = redoHistory(u2.history, u2.grid)!;
  ok(sameMap(r1.grid, B), "forward gives the step again");
  const r2 = redoHistory(r1.history, r1.grid)!;
  ok(sameMap(r2.grid, C) && !canRedo(r2.history), "forward to the newest, then nothing");
  // the undone grid coming back through the editor's own commit is no new step
  ok(commitHistory(u1.history, u1.grid, 9000, true) === u1.history, "an undone grid committing is no step");
  // a new edit after going back drops the steps ahead
  const fresh = commitHistory(u1.history, D, 9000, false);
  ok(!canRedo(fresh) && fresh.past.length === 2, "a new edit clears forward");
  // quick repeated keys (held arrow) merge into one step, a drag never does
  let k = commitHistory(emptyHistory, A, 0, false);
  k = commitHistory(k, B, 1000, true);
  k = commitHistory(k, C, 1100, true);
  k = commitHistory(k, D, 1200, true);
  ok(k.past.length === 1 && sameMap(undoHistory(k, D)!.grid, A), "a burst of key steps is one step back to before it");
  const dragAfterKey = commitHistory(k, B, 1300, false);
  ok(dragAfterKey.past.length === 2, "a drag right after a key step is its own step");
}

if (failed) {
  console.error(`${failed} gridMath check(s) failed`);
  process.exit(1);
}
console.log("GRIDMATH-OK");
