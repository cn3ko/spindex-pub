// The grid editor's chord lane: the harmony analysis (a chord guess with a
// confidence for every millisecond, from frames ~46 ms apart) merged into
// stretches someone can read. Pure; checked by scripts/chords-check.

/** `harmony` byte for "no clear chord" (wave_colour::NO_CHORD). */
export const NO_CHORD = 255;
/** Below this confidence (0-255) a moment counts as no clear chord. */
export const CHORD_MIN_CONF = 64;
/** A stretch shorter than this folds into a neighbour: chords hold for beats. */
export const MIN_CHORD_MS = 500;
/**
 * An unclear stretch shorter than this folds into a neighbour too: on a real
 * house track brief unclear moments split nearly every chord, 35 gaps in 90
 * segments; a progression reads as one lane through them.
 */
export const MIN_GAP_MS = 1500;

/** One stretch of the lane, in milliseconds; `end` is exclusive. */
export interface ChordSegment {
  start: number;
  end: number;
  /** Camelot index (see waveColors.camelotOf), or NO_CHORD for a gap. */
  code: number;
}

/**
 * Harmony bytes ([camelot, confidence] per ms) as readable stretches:
 * unclear moments become gaps, and every stretch under MIN_CHORD_MS (a gap:
 * MIN_GAP_MS, and only between two chords) folds into the longer neighbour
 * beside it (the shortest first, so a flicker settles to
 * the chord that dominates it). Neighbours of the same chord join.
 */
export function chordSegments(h: Uint8Array): ChordSegment[] {
  const n = Math.floor(h.length / 2);
  const runs: ChordSegment[] = [];
  for (let m = 0; m < n; m++) {
    const code = h[m * 2] !== NO_CHORD && h[m * 2 + 1] >= CHORD_MIN_CONF ? h[m * 2] : NO_CHORD;
    const last = runs[runs.length - 1];
    if (last && last.code === code) last.end = m + 1;
    else runs.push({ start: m, end: m + 1, code });
  }
  for (;;) {
    let idx = -1;
    let shortest = Infinity;
    for (let i = 0; i < runs.length && runs.length > 1; i++) {
      const len = runs[i].end - runs[i].start;
      // a gap folds only between two chords: at the track's start or end it
      // is real (an intro, an outro), not a hiccup inside a progression
      const gap = runs[i].code === NO_CHORD;
      if (gap && (i === 0 || i === runs.length - 1)) continue;
      const min = gap ? MIN_GAP_MS : MIN_CHORD_MS;
      if (len < min && len < shortest) {
        shortest = len;
        idx = i;
      }
    }
    if (idx < 0) break;
    const prev = runs[idx - 1];
    const next = runs[idx + 1];
    const intoPrev = !!prev && (!next || prev.end - prev.start >= next.end - next.start);
    if (intoPrev) prev.end = runs[idx].end;
    else next.start = runs[idx].start;
    runs.splice(idx, 1);
    // the stretch that grew may now touch one of the same chord
    const at = intoPrev ? idx - 1 : idx;
    if (runs[at + 1] && runs[at + 1].code === runs[at].code) {
      runs[at].end = runs[at + 1].end;
      runs.splice(at + 1, 1);
    }
    if (at > 0 && runs[at - 1].code === runs[at].code) {
      runs[at - 1].end = runs[at].end;
      runs.splice(at, 1);
    }
  }
  return runs;
}
