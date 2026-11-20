// Keyboard shortcuts for the deck and the grid editor: the actions, their
// default keys, the user's own bindings (kept in localStorage — a per-machine
// preference) and the matching of a key press to an action. The top half is
// pure and checked by scripts/shortcuts-check.
import { useSyncExternalStore } from "react";

export type ActionId =
  | "playPause"
  | "gridMode"
  | "moveGrid"
  | "stretchGrid"
  | "stretchBetween"
  | "click"
  | "nudgeEarlier"
  | "nudgeLater"
  | "barOne"
  | "markerAdd"
  | "markerRemove"
  | "bpmDown"
  | "bpmUp"
  | "tap"
  | "zoomOut"
  | "zoomIn"
  | "loopIn"
  | "loopOut"
  | "loopToggle"
  | "loopClear"
  | "colourFreq"
  | "colourDrums"
  | "chordLane"
  | "undo"
  | "redo"
  | "discard"
  | "save"
  | "shortcuts";

export interface ActionDef {
  id: ActionId;
  label: string;
  group: "Deck" | "Grid editor";
  key: string;
  /** Shift + the key is the fine variant (the 10 ms nudge). */
  fine?: boolean;
}

export const ACTIONS: ActionDef[] = [
  { id: "playPause", label: "Play / pause", group: "Deck", key: "Space" },
  { id: "gridMode", label: "Open / close the grid editor", group: "Deck", key: "G" },
  { id: "moveGrid", label: "Drag moves the grid (instead of the track)", group: "Grid editor", key: "E" },
  { id: "stretchGrid", label: "Drag stretches the grid (tempo markers stay)", group: "Grid editor", key: "W" },
  { id: "stretchBetween", label: "Drag stretches between two markers only", group: "Grid editor", key: "Shift+W" },
  { id: "click", label: "Click track on / off", group: "Grid editor", key: "M" },
  { id: "nudgeEarlier", label: "Move grid earlier 1 ms (Shift: 10 ms)", group: "Grid editor", key: "ArrowLeft", fine: true },
  { id: "nudgeLater", label: "Move grid later 1 ms (Shift: 10 ms)", group: "Grid editor", key: "ArrowRight", fine: true },
  { id: "barOne", label: "Make the beat nearest the playhead bar 1", group: "Grid editor", key: "B" },
  { id: "markerAdd", label: "Tempo marker at the playhead (a new tempo from there)", group: "Grid editor", key: "K" },
  { id: "markerRemove", label: "Remove the tempo marker at the playhead", group: "Grid editor", key: "Shift+K" },
  { id: "bpmDown", label: "BPM − 0.01", group: "Grid editor", key: "-" },
  { id: "bpmUp", label: "BPM + 0.01", group: "Grid editor", key: "+" },
  { id: "tap", label: "Tap tempo", group: "Grid editor", key: "T" },
  { id: "zoomOut", label: "Zoom out", group: "Grid editor", key: "," },
  { id: "zoomIn", label: "Zoom in", group: "Grid editor", key: "." },
  { id: "loopIn", label: "Loop In at the playhead", group: "Grid editor", key: "I" },
  { id: "loopOut", label: "Loop Out at the playhead (starts looping)", group: "Grid editor", key: "O" },
  { id: "loopToggle", label: "Loop on / off", group: "Grid editor", key: "L" },
  { id: "loopClear", label: "Clear the loop", group: "Grid editor", key: "Shift+L" },
  { id: "colourFreq", label: "Colours: frequency", group: "Grid editor", key: "1" },
  { id: "colourDrums", label: "Colours: drums", group: "Grid editor", key: "2" },
  { id: "chordLane", label: "Chord lane on / off", group: "Grid editor", key: "3" },
  { id: "undo", label: "Back: undo the last grid change", group: "Grid editor", key: "Ctrl+Z" },
  { id: "redo", label: "Forward: redo the change undone", group: "Grid editor", key: "Ctrl+Y" },
  { id: "discard", label: "Discard grid changes", group: "Grid editor", key: "Shift+Backspace" },
  { id: "save", label: "Save the grid to the file", group: "Grid editor", key: "Ctrl+S" },
  { id: "shortcuts", label: "Keyboard shortcuts", group: "Grid editor", key: "?" },
];

/** null = deliberately unbound. */
export type Bindings = Record<ActionId, string | null>;

export const defaults = (): Bindings =>
  Object.fromEntries(ACTIONS.map((a) => [a.id, a.key])) as Bindings;

const MODIFIERS = new Set(["Shift", "Control", "Alt", "Meta", "AltGraph", "CapsLock", "OS"]);

/**
 * The name of a key press ("L", "Shift+L", "Ctrl+S", "Space", "ArrowLeft"),
 * or null for a press that can be no shortcut (a modifier alone, a dead key).
 * A shifted symbol is named by the symbol itself: "+" is "+" whether the
 * layout needs Shift for it or not.
 */
export function comboOf(e: {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}): string | null {
  const k = e.key;
  if (!k || MODIFIERS.has(k) || k === "Dead" || k === "Unidentified") return null;
  let name: string;
  let shiftCounts = true;
  if (k === " ") name = "Space";
  else if (k.length === 1 && /\p{L}/u.test(k)) name = k.toUpperCase();
  else if (k.length === 1) {
    name = k;
    shiftCounts = false;
  } else name = k;
  const mods = [
    e.ctrlKey || e.metaKey ? "Ctrl" : "",
    e.altKey ? "Alt" : "",
    e.shiftKey && shiftCounts ? "Shift" : "",
  ].filter(Boolean);
  return [...mods, name].join("+");
}

const SHOWN: Record<string, string> = {
  ArrowLeft: "←",
  ArrowRight: "→",
  ArrowUp: "↑",
  ArrowDown: "↓",
  Backspace: "⌫",
};

/** A combo as shown to people: arrows as arrows, unbound as a dash. */
export function displayCombo(c: string | null): string {
  if (!c) return "—";
  return c
    .split("+")
    .map((p) => SHOWN[p] ?? p)
    .join("+")
    .replace(/\+\+$/, "+ +"); // the "+" key itself after a modifier
}

/** The action a key press triggers; `fine` when Shift selects its fine variant. */
export function actionFor(combo: string, b: Bindings): { id: ActionId; fine: boolean } | null {
  const exact = ACTIONS.find((a) => b[a.id] === combo);
  if (exact) return { id: exact.id, fine: false };
  if (combo.startsWith("Shift+")) {
    const plain = combo.slice("Shift+".length);
    const fine = ACTIONS.find((a) => a.fine && b[a.id] === plain);
    if (fine) return { id: fine.id, fine: true };
  }
  return null;
}

/** The other action already using `combo`, if any. */
export function conflictWith(b: Bindings, id: ActionId, combo: string): ActionId | null {
  return ACTIONS.find((a) => a.id !== id && b[a.id] === combo)?.id ?? null;
}

/** `b` with `combo` on `id`; an action that had it is unbound, never left sharing it. */
export function withBinding(b: Bindings, id: ActionId, combo: string | null): Bindings {
  const next = { ...b };
  if (combo) {
    for (const a of ACTIONS) if (a.id !== id && next[a.id] === combo) next[a.id] = null;
  }
  next[id] = combo;
  return next;
}

/** Stored overrides on top of the defaults; anything unreadable is ignored. */
export function merge(stored: unknown): Bindings {
  let b = defaults();
  if (!stored || typeof stored !== "object") return b;
  for (const a of ACTIONS) {
    if (!(a.id in stored)) continue;
    const v = (stored as Record<string, unknown>)[a.id];
    if (v === null || typeof v === "string") b = withBinding(b, a.id, v);
  }
  return b;
}

// ---------------------------------------------------------------------
// The live bindings: one store, so the settings window and every button
// and handler see a change at once
// ---------------------------------------------------------------------

const STORE_KEY = "spindex.shortcuts";
let current: Bindings = (() => {
  try {
    return merge(JSON.parse(localStorage.getItem(STORE_KEY) ?? "null"));
  } catch {
    return defaults();
  }
})();
const listeners = new Set<() => void>();

export function setBindings(b: Bindings) {
  current = b;
  try {
    // only what differs from the defaults, so new defaults reach old installs
    const d = defaults();
    const diff = Object.fromEntries(ACTIONS.filter((a) => b[a.id] !== d[a.id]).map((a) => [a.id, b[a.id]]));
    localStorage.setItem(STORE_KEY, JSON.stringify(diff));
  } catch {
    /* kept for this session only */
  }
  listeners.forEach((l) => l());
}

export function useBindings(): Bindings {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => current,
  );
}

/** A button tooltip with its shortcut: "Mark the loop start · I". */
export function tip(text: string, id: ActionId, b: Bindings): string {
  const k = b[id];
  return k ? `${text} · ${displayCombo(k)}` : text;
}

/**
 * Whether a key press should be ignored as a shortcut: typing in a field, or
 * another layer (the visualizer, a modal) owns the keyboard.
 */
export function keyOwnedElsewhere(e: KeyboardEvent): boolean {
  const el = e.target as HTMLElement | null;
  if (el && (["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName) || el.isContentEditable)) return true;
  return !!document.querySelector(".viz-overlay, .modal-backdrop");
}
