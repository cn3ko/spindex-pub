// Back / Forward for the grid editor: one step per finished edit. Pure, so
// scripts/gridmath-check can pin it.
import { sameMap, type TempoMap } from "./gridMath";

export interface GridHistory {
  past: TempoMap[];
  future: TempoMap[];
  /** The grid the newest step ended on, null before the starting grid. */
  settled: TempoMap | null;
  /** When the newest step was taken, and whether a quick key step may join it. */
  at: number;
  mergeable: boolean;
}

export const emptyHistory: GridHistory = { past: [], future: [], settled: null, at: 0, mergeable: false };

/** Key steps closer together than this (a held arrow key) are one step. */
const MERGE_MS = 600;
const MAX_STEPS = 200;

export const canUndo = (h: GridHistory) => h.past.length > 0;
export const canRedo = (h: GridHistory) => h.future.length > 0;

/**
 * Records `grid` as the current state. The first grid starts the history
 * without a step; the same grid again changes nothing; a new grid is a step,
 * clears the steps ahead, and joins the previous step when both are quick
 * mergeable ones (`merge`: key or button steps, never a drag).
 */
export function commitHistory(h: GridHistory, grid: TempoMap, now: number, merge: boolean): GridHistory {
  if (!h.settled) return { ...h, settled: grid, at: now, mergeable: false };
  if (sameMap(grid, h.settled)) return h;
  const joins = merge && h.mergeable && now - h.at < MERGE_MS && h.past.length > 0;
  const past = joins ? h.past : [...h.past, h.settled].slice(-MAX_STEPS);
  return { past, future: [], settled: grid, at: now, mergeable: merge };
}

/** One step back from `current`: the grid to show and the new history. */
export function undoHistory(h: GridHistory, current: TempoMap): { grid: TempoMap; history: GridHistory } | null {
  if (!canUndo(h)) return null;
  const grid = h.past[h.past.length - 1];
  return {
    grid,
    history: { past: h.past.slice(0, -1), future: [...h.future, current], settled: grid, at: 0, mergeable: false },
  };
}

/** One step forward again after a step back. */
export function redoHistory(h: GridHistory, current: TempoMap): { grid: TempoMap; history: GridHistory } | null {
  if (!canRedo(h)) return null;
  const grid = h.future[h.future.length - 1];
  return {
    grid,
    history: { past: [...h.past, current], future: h.future.slice(0, -1), settled: grid, at: 0, mergeable: false },
  };
}
