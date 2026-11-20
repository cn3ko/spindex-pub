import { keyBadgeColor } from "../theme";

export type Rgb = [number, number, number];

export interface Bands {
  bass: number; // 20-150 Hz
  mid: number; // 150-2000 Hz
  treble: number; // 2-10 kHz
  level: number; // overall
}

/** Average byte-frequency energy (0..1) per band. */
export function bandEnergies(freq: Uint8Array, sampleRate: number, fftSize: number): Bands {
  const binHz = sampleRate / fftSize;
  const avg = (lo: number, hi: number): number => {
    const a = Math.max(0, Math.floor(lo / binHz));
    const b = Math.min(freq.length - 1, Math.ceil(hi / binHz));
    if (b < a) return 0;
    let s = 0;
    for (let i = a; i <= b; i++) s += freq[i];
    return s / ((b - a + 1) * 255);
  };
  return {
    bass: avg(20, 150),
    mid: avg(150, 2000),
    treble: avg(2000, 10000),
    level: avg(20, 10000),
  };
}

/** Beat phase 0..1 from playback time, analyzer BPM and an anchor offset. */
export function beatPhase(t: number, bpm: number | null | undefined, anchor: number): number {
  if (!bpm || bpm <= 0 || !isFinite(t)) return 0;
  const beats = (t - anchor) * (bpm / 60);
  return beats - Math.floor(beats);
}

function hexToRgb(hex: string): Rgb {
  const n = parseInt(hex.slice(1), 16);
  return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255];
}

function rgbToHsl([r, g, b]: Rgb): [number, number, number] {
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  if (max === min) return [0, 0, l];
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h: number;
  if (max === r) h = ((g - b) / d + (g < b ? 6 : 0)) / 6;
  else if (max === g) h = ((b - r) / d + 2) / 6;
  else h = ((r - g) / d + 4) / 6;
  return [h, s, l];
}

function hslToRgb([h, s, l]: [number, number, number]): Rgb {
  if (s === 0) return [l, l, l];
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
  const p = 2 * l - q;
  const f = (t: number): number => {
    t = ((t % 1) + 1) % 1;
    if (t < 1 / 6) return p + (q - p) * 6 * t;
    if (t < 1 / 2) return q;
    if (t < 2 / 3) return p + (q - p) * (2 / 3 - t) * 6;
    return p;
  };
  return [f(h + 1 / 3), f(h), f(h - 1 / 3)];
}

/**
 * Three-color palette from the track's Camelot key badge color:
 * [base, hue +40deg brighter, hue -60deg deeper]. Unknown key falls back to
 * the neutral badge gray, which we replace with the turquoise accent.
 */
export function camelotPalette(key: string | null | undefined): [Rgb, Rgb, Rgb] {
  let hex = keyBadgeColor(key);
  if (hex === "#5c6575") hex = "#38d6c4";
  const [h, s, l] = rgbToHsl(hexToRgb(hex));
  const sat = Math.max(s, 0.45);
  const lum = Math.min(Math.max(l, 0.5), 0.62);
  return [
    hslToRgb([h, sat, lum]),
    hslToRgb([h + 40 / 360, sat, Math.min(lum + 0.12, 0.75)]),
    hslToRgb([h - 60 / 360, sat, Math.max(lum - 0.22, 0.22)]),
  ];
}
