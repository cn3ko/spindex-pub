// Arithmetic for the deck's grid editor. Pure: no state, no I/O. A grid is a
// constant grid — a bar-1 line at `anchor` seconds and a tempo — the only kind
// the editor edits. Saving is Rust's job (api.saveTrackGrid).

export interface Grid {
  /** seconds of one bar-1 line; may be negative */
  anchor: number;
  bpm: number;
}

export const BEATS_PER_BAR = 4;
/** A gap longer than this between taps starts a new tap sequence. */
export const TAP_RESET_MS = 2000;
const MIN_TAPS = 4;

export const period = (g: Grid): number => 60 / g.bpm;

/** Every beat line in [from, to], with the bar-1 lines flagged. */
export function linesIn(g: Grid, from: number, to: number): { t: number; bar1: boolean }[] {
  const p = period(g);
  const first = Math.ceil((from - g.anchor) / p - 1e-9);
  const last = Math.floor((to - g.anchor) / p + 1e-9);
  const out: { t: number; bar1: boolean }[] = [];
  for (let k = first; k <= last; k++) {
    const idx = ((k % BEATS_PER_BAR) + BEATS_PER_BAR) % BEATS_PER_BAR;
    out.push({ t: g.anchor + k * p, bar1: idx === 0 });
  }
  return out;
}

/** The beat line closest to time t. */
export function nearestLine(g: Grid, t: number): number {
  const p = period(g);
  return g.anchor + Math.round((t - g.anchor) / p) * p;
}

/** Moves the whole grid by deltaSec. */
export const shift = (g: Grid, deltaSec: number): Grid => ({ ...g, anchor: g.anchor + deltaSec });

/** Makes the line nearest t bar 1. No line moves; only which one is bar 1. */
export const barOneAt = (g: Grid, t: number): Grid => ({ ...g, anchor: nearestLine(g, t) });

/**
 * A new tempo that keeps the line nearest pivotTime exactly where it is, so
 * the passage being looked at does not jump. That line keeps its beat number,
 * so bar 1 stays on the same beat of the bar. A change below the shown
 * precision (0.01 BPM) is no change: a stored 127.98999… retyped as 127.99
 * must not turn the grid dirty.
 */
export function rescaleAround(g: Grid, newBpm: number, pivotTime: number): Grid {
  if (Math.abs(newBpm - g.bpm) < 0.005) return g;
  const p = period(g);
  const k = Math.round((pivotTime - g.anchor) / p);
  const pivot = g.anchor + k * p;
  return { anchor: pivot - k * (60 / newBpm), bpm: newBpm };
}

/**
 * BPM from tap times (ms): the median gap of the latest run of taps, a run
 * ending at any gap over TAP_RESET_MS. Null until MIN_TAPS taps.
 */
export function tapBpm(taps: number[]): number | null {
  let start = 0;
  for (let i = 1; i < taps.length; i++) {
    if (taps[i] - taps[i - 1] > TAP_RESET_MS) start = i;
  }
  const run = taps.slice(start);
  if (run.length < MIN_TAPS) return null;
  const gaps = run.slice(1).map((t, i) => t - run[i]).sort((a, b) => a - b);
  const mid = gaps[Math.floor(gaps.length / 2)];
  return mid > 0 ? 60000 / mid : null;
}

/** An A-B loop over a stretch of the track, in seconds. */
export interface Loop {
  start: number;
  end: number;
}

/** Shorter than this is a slip of the finger, not a loop. */
const MIN_LOOP = 0.05;
/**
 * How far past Out playback may be and still count as having just played
 * through it (a frame or a throttled tab). Further than that, the user
 * seeked away, and the loop ends instead of dragging them back.
 */
const LOOP_EXIT_SLACK = 0.5;

/** A loop between two marks set in either order; null if unset or too short. */
export function makeLoop(a: number, b: number): Loop | null {
  if (!Number.isFinite(a) || !Number.isFinite(b)) return null;
  const [start, end] = a <= b ? [a, b] : [b, a];
  return end - start < MIN_LOOP ? null : { start, end };
}

/** At playback time t: jump back to In ("wrap"), end the loop ("exit"), or nothing. */
export function loopStep(t: number, l: Loop): "wrap" | "exit" | null {
  if (t < l.end) return null;
  return t - l.end <= LOOP_EXIT_SLACK ? "wrap" : "exit";
}

/**
 * The playback time after dragging the waveform `dxPx` pixels from where the
 * drag began at `startSec`: the track moves with the hand, as a map does, so
 * dragging left goes later. Kept within the track.
 */
export function scrubTime(startSec: number, dxPx: number, secPerPx: number, duration: number): number {
  return Math.min(duration, Math.max(0, startSec - dxPx * secPerPx));
}

/**
 * Stretches the grid by hand: the beat line nearest `pivotTime` stays where
 * it is, and the point grabbed at `grabTime` follows the pointer to
 * `newGrabTime`, every line in between spreading out or closing up. A wider
 * grid is a slower tempo. Null when the grab is too close to the pivot to
 * act as a lever (under ~1 beat), when the drag crosses the pivot, or when
 * the tempo would leave 40-250 BPM. The tempo moves in 0.01 BPM steps, like
 * the BPM field. Always stretch from the grid the drag started with, so a
 * long drag does not pile up rounding.
 */
export function stretchTo(g: Grid, pivotTime: number, grabTime: number, newGrabTime: number): Grid | null {
  const pivot = nearestLine(g, pivotTime);
  const before = grabTime - pivot;
  const after = newGrabTime - pivot;
  if (Math.abs(before) < period(g) * 0.75 || before * after <= 0) return null;
  const bpm = Math.round(g.bpm * (before / after) * 100) / 100;
  if (bpm < 40 || bpm > 250) return null;
  return rescaleAround(g, bpm, pivot);
}

// ---------------------------------------------------------------------
// Tempo maps: a grid in sections, for tracks whose tempo changes. Section k
// runs from its start to the next one's (the first also reaches back before
// its start, the last on to the end) at its own tempo. Rules, as Serato
// stores them: every section but the last holds whole beats up to the next
// marker, the first section's start is a bar 1, and bars count on through all
// sections. A one-section map is exactly the constant `Grid`.
// ---------------------------------------------------------------------

export interface Section {
  start: number;
  bpm: number;
}
export type TempoMap = Section[];

const per = (s: Section) => 60 / s.bpm;

/** The section playing at time t (the first one before its start). */
export function sectionAt(m: TempoMap, t: number): number {
  let k = 0;
  for (let i = 1; i < m.length; i++) if (m[i].start <= t + 1e-9) k = i;
  return k;
}

/** Whole beats from section k to the next marker (Infinity for the last). */
export function sectionBeats(m: TempoMap, k: number): number {
  if (k < 0 || k >= m.length - 1) return Infinity;
  return Math.round(((m[k + 1].start - m[k].start) * m[k].bpm) / 60);
}

/**
 * The running beat number of section k's first line. Every tempo marker is a
 * downbeat, as in Serato: bars restart there, and a section whose beats are
 * not a whole number of bars ends on a short bar. So changing the beats
 * between two markers never moves a bar after the second one.
 */
function beatIndexAt(m: TempoMap, k: number): number {
  let bars = 0;
  for (let j = 0; j < k; j++) bars += Math.ceil(sectionBeats(m, j) / BEATS_PER_BAR);
  return bars * BEATS_PER_BAR;
}

/**
 * Every beat line in [from, to] of the map, with the bar-1 lines flagged and
 * each line's running beat number (0 = the first bar 1; bar = beat / 4 + 1).
 */
export function mapLinesIn(
  m: TempoMap,
  from: number,
  to: number,
): { t: number; bar1: boolean; beat: number }[] {
  const out: { t: number; bar1: boolean; beat: number }[] = [];
  for (let k = 0; k < m.length; k++) {
    const s = m[k];
    const p = per(s);
    // exclusive: the line at the next marker is the next section's
    const end = k < m.length - 1 ? m[k + 1].start : Infinity;
    const lo = k === 0 ? from : Math.max(from, s.start);
    const hi = Math.min(to, end);
    if (hi < lo) continue;
    const base = beatIndexAt(m, k);
    const first = Math.ceil((lo - s.start) / p - 1e-9);
    const last = Math.floor((hi - s.start) / p + 1e-9);
    for (let n = first; n <= last; n++) {
      const t = s.start + n * p;
      if (t >= end - 1e-6) break;
      const idx = (((base + n) % BEATS_PER_BAR) + BEATS_PER_BAR) % BEATS_PER_BAR;
      out.push({ t, bar1: idx === 0, beat: base + n });
    }
  }
  return out;
}

/** The beat line closest to t, the next section's marker included. */
export function mapNearestLine(m: TempoMap, t: number): number {
  const k = sectionAt(m, t);
  const s = m[k];
  const p = per(s);
  let best = s.start + Math.round((t - s.start) / p) * p;
  if (k > 0) best = Math.max(best, s.start); // no line of this tempo before its marker
  const next = m[k + 1]?.start;
  if (next !== undefined && Math.abs(next - t) < Math.abs(best - t)) best = next;
  return best;
}

/** Tempo in range, starts in order, every section but the last at least a beat long. */
export function validMap(m: TempoMap): boolean {
  if (m.length === 0) return false;
  for (let k = 0; k < m.length; k++) {
    const s = m[k];
    if (!Number.isFinite(s.start) || !Number.isFinite(s.bpm) || s.bpm < 40 || s.bpm > 250) return false;
    if (k > 0 && (s.start <= m[k - 1].start || sectionBeats(m, k - 1) < 1)) return false;
  }
  return true;
}

export const sameMap = (a: TempoMap, b: TempoMap): boolean =>
  a.length === b.length &&
  a.every((s, k) => Math.abs(s.start - b[k].start) < 1e-6 && Math.abs(s.bpm - b[k].bpm) < 1e-6);

const checked = (m: TempoMap): TempoMap | null => (validMap(m) ? m : null);

/** Section k's tempo re-fitted so its beats, unchanged, span to the next marker. */
function refit(m: TempoMap, k: number, beats: number) {
  if (k < 0 || k >= m.length - 1) return;
  m[k] = { ...m[k], bpm: (beats * 60) / (m[k + 1].start - m[k].start) };
}

/**
 * A tempo marker on the beat line nearest t: the section there splits in two
 * at the same tempo, so nothing moves. Null at or past a section's own start
 * or next marker.
 */
export function addMarker(m: TempoMap, t: number): TempoMap | null {
  const k = sectionAt(m, t);
  const s = m[k];
  const n = Math.round((t - s.start) / per(s));
  if (n <= 0 || n >= sectionBeats(m, k)) return null;
  const out = m.slice();
  out.splice(k + 1, 0, { start: s.start + n * per(s), bpm: s.bpm });
  return checked(out);
}

/** Removes the marker of the section playing at t: it joins the one before. */
export function removeMarker(m: TempoMap, t: number): TempoMap | null {
  const k = sectionAt(m, t);
  if (k === 0) return null;
  const joined = sectionBeats(m, k - 1) + sectionBeats(m, k);
  const out = m.slice();
  out.splice(k, 1);
  // a middle section: its beats join the one before, which now spans both
  if (k < m.length - 1) refit(out, k - 1, joined);
  return checked(out);
}

/**
 * Moves section k (its start and every line in it) by deltaSec. Whole beats
 * between markers stay whole: the section before, and this one if another
 * follows, re-fit their tempo to the new spans. One section is a plain shift.
 */
export function moveSection(m: TempoMap, k: number, deltaSec: number): TempoMap | null {
  const before = sectionBeats(m, k - 1);
  const own = sectionBeats(m, k);
  const out = m.slice();
  out[k] = { ...out[k], start: out[k].start + deltaSec };
  refit(out, k - 1, before);
  refit(out, k, own);
  return checked(out);
}

/**
 * Section k at a new tempo, the beat line nearest pivotTime staying where it
 * is. Callers pivot a later section on its own marker, so the sections before
 * never move; the first section pivots on the playhead's line. A following
 * marker moves with the section's last beat, and the section after it
 * re-fits (its own end stays). A change under 0.005 BPM is no change.
 */
export function retempoSection(m: TempoMap, k: number, bpm: number, pivotTime: number): TempoMap | null {
  const s = m[k];
  if (Math.abs(bpm - s.bpm) < 0.005) return m;
  const n = Math.round((pivotTime - s.start) / per(s));
  const pivot = s.start + n * per(s);
  const beatsBefore = sectionBeats(m, k - 1);
  const own = sectionBeats(m, k);
  const nextOwn = sectionBeats(m, k + 1);
  const out = m.slice();
  out[k] = { start: pivot - (n * 60) / bpm, bpm };
  if (k < m.length - 1) {
    out[k + 1] = { ...out[k + 1], start: out[k].start + (own * 60) / bpm };
    refit(out, k + 1, nextOwn);
  }
  refit(out, k - 1, beatsBefore);
  return checked(out);
}

/**
 * Stretches section k by hand: the grabbed point follows the pointer, the
 * line nearest pivotTime stays (see retempoSection for which line callers
 * pivot on). The same lever and range rules as `stretchTo`.
 */
export function stretchSection(
  m: TempoMap,
  k: number,
  pivotTime: number,
  grabTime: number,
  newGrabTime: number,
): TempoMap | null {
  const s = m[k];
  const pivot = s.start + Math.round((pivotTime - s.start) / per(s)) * per(s);
  const before = grabTime - pivot;
  const after = newGrabTime - pivot;
  if (Math.abs(before) < per(s) * 0.75 || before * after <= 0) return null;
  const bpm = Math.round(s.bpm * (before / after) * 100) / 100;
  if (bpm < 40 || bpm > 250) return null;
  return retempoSection(m, k, bpm, pivot);
}

/**
 * Stretches section k between its two fixed ends, its start and the next
 * marker: neither moves, so nothing outside the section does either. Serato
 * stores whole beats between markers, so with both ends fixed only the beat
 * count can change: the grabbed point's pull picks the nearest count a whole
 * number of bars (4 beats) away, so the bar lines after the second marker
 * never move. The same unchanged map when the count stays; null for
 * the last section (no second marker), with no lever (grab under ~3/4 beat
 * from the start), across the start, or out of the 40-250 BPM range.
 */
export function stretchBetween(m: TempoMap, k: number, grabTime: number, newGrabTime: number): TempoMap | null {
  if (k < 0 || k >= m.length - 1) return null;
  const s = m[k];
  const before = grabTime - s.start;
  const after = newGrabTime - s.start;
  if (before < per(s) * 0.75 || after <= 0) return null;
  const beats = sectionBeats(m, k);
  // whole bars only: the count keeps its remainder, so every bar line after
  // the second marker stays where it was
  const wanted = beats + BEATS_PER_BAR * Math.round((beats * before) / after / BEATS_PER_BAR - beats / BEATS_PER_BAR);
  if (wanted === beats) return m;
  if (wanted < 1) return null;
  const out = m.slice();
  refit(out, k, wanted);
  return checked(out);
}

/**
 * The editor's stretch: section k stretches by hand and no tempo marker ever
 * moves. Between two markers only the whole beats can change (`stretchBetween`);
 * the first section, ahead of a marker, keeps that marker and its beat count,
 * so its tempo stretches smoothly and its start follows; the last section
 * pivots on its own marker (or, with no markers, on the line nearest
 * pivotTime) with nothing after it to move.
 */
export function stretchHeld(
  m: TempoMap,
  k: number,
  pivotTime: number,
  grabTime: number,
  newGrabTime: number,
): TempoMap | null {
  if (k > 0 && k < m.length - 1) return stretchBetween(m, k, grabTime, newGrabTime);
  if (k === 0 && m.length > 1) {
    const s = m[0];
    const pivot = m[1].start;
    const before = grabTime - pivot;
    const after = newGrabTime - pivot;
    if (Math.abs(before) < per(s) * 0.75 || before * after <= 0) return null;
    const bpm = Math.round(s.bpm * (before / after) * 100) / 100;
    if (bpm < 40 || bpm > 250) return null;
    const start = pivot - (sectionBeats(m, 0) * 60) / bpm;
    if (start < -10) return null; // the file cannot hold a grid starting earlier
    const out = m.slice();
    out[0] = { start, bpm };
    return checked(out);
  }
  return stretchSection(m, k, k > 0 ? m[k].start : pivotTime, grabTime, newGrabTime);
}

/** The lines a stretch of section k keeps in place (see `stretchHeld`). */
export function heldLines(m: TempoMap, k: number, playhead: number): number[] {
  if (k > 0 && k < m.length - 1) return [m[k].start, m[k + 1].start];
  if (k === 0 && m.length > 1) return [m[1].start];
  if (k > 0) return [m[k].start];
  return [mapNearestLine(m, playhead)];
}

/**
 * Makes the beat nearest t a bar 1 without moving any line. In the first
 * section its start (always a bar 1) steps back by whole beats, which only
 * renumbers the bars; in a later section a tempo marker goes on that beat at
 * the same tempo, since every marker is a downbeat.
 */
export function barOneAtMap(m: TempoMap, t: number): TempoMap | null {
  const k = sectionAt(m, t);
  const s = m[k];
  const n = Math.round((t - s.start) / per(s));
  const j = (((beatIndexAt(m, k) + n) % BEATS_PER_BAR) + BEATS_PER_BAR) % BEATS_PER_BAR;
  if (j === 0) return m;
  if (k > 0) return addMarker(m, s.start + n * per(s));
  const out = m.slice();
  out[0] = { ...out[0], start: out[0].start - (BEATS_PER_BAR - j) * per(out[0]) };
  return checked(out);
}
