// Waveform colours, shared by the deck's overview and the grid editor so the
// same sound looks the same in both.
type Rgb = readonly [number, number, number];

// Frequency: bass/kick red, mids (melody, the SpinDex accent) turquoise,
// highs (hats, air) pale blue.
const FREQ: readonly Rgb[] = [
  [0xe0, 0x56, 0x56],
  [0x38, 0xd6, 0xc4],
  [0xbf, 0xe4, 0xf2],
];
// Drums vs tonal: kick red, snare/hats warm yellow, sustained tonal violet.
const DRUMS: readonly Rgb[] = [
  [0xe0, 0x56, 0x56],
  [0xf2, 0xd1, 0x6b],
  [0x8b, 0x7c, 0xf6],
];
/** The grid editor's layered Freq view: low, mid, high, back to front. */
export const FREQ_LAYERS = ["rgba(224,86,86,0.9)", "rgba(56,214,196,0.85)", "rgba(191,228,242,0.85)"];
export const DRUM_LEGEND = [
  { label: "kick", colour: "#e05656" },
  { label: "snare/hats", colour: "#f2d16b" },
  { label: "tonal", colour: "#8b7cf6" },
];

const css = (c: Rgb, dim = 1) => `rgb(${(c[0] * dim) | 0},${(c[1] * dim) | 0},${(c[2] * dim) | 0})`;

/**
 * Blend of a palette for levels 0..1, each already relative to its own
 * loudest moment (bass energy dwarfs the rest in absolute terms, so a raw
 * blend paints everything red). Weights are squared so the dominant part
 * pops. `dim` darkens. Null when all levels are silent.
 */
function blend(levels: readonly number[], palette: readonly Rgb[], dim: number): string | null {
  const w = levels.map((l) => l * l);
  const tot = w.reduce((a, b) => a + b, 0);
  if (tot <= 0) return null;
  const c = [0, 1, 2].map((k) => w.reduce((s, wi, i) => s + wi * palette[i][k], 0) / tot) as unknown as Rgb;
  return css(c, dim);
}

/** Frequency colour for low/mid/high levels 0..1. */
export function bandColour(low: number, mid: number, high: number, dim = 1): string | null {
  return blend([low, mid, high], FREQ, dim);
}

/** Drums-vs-tonal colour for kick/hats/tonal levels 0..1. */
export function drumColour(kick: number, hats: number, tonal: number): string | null {
  return blend([kick, hats, tonal], DRUMS, 1);
}

/** Camelot code ("8A") for a harmony byte, or null for "no chord". */
export function camelotOf(index: number): string | null {
  if (index > 23) return null;
  return `${Math.floor(index / 2) + 1}${index % 2 ? "B" : "A"}`;
}

/** Chord names for the legend: "8A" → "A minor" (root of the chord). */
const MAJOR_BY_NUMBER = ["B", "F#", "Db", "Ab", "Eb", "Bb", "F", "C", "G", "D", "A", "E"];
const MINOR_BY_NUMBER = ["G#", "D#", "Bb", "F", "C", "G", "D", "A", "E", "B", "F#", "C#"];
export function chordName(index: number): string | null {
  if (index > 23) return null;
  const n = Math.floor(index / 2);
  return index % 2 ? `${MAJOR_BY_NUMBER[n]} major` : `${MINOR_BY_NUMBER[n]} minor`;
}
