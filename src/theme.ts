// Design tokens from CLAUDE.md / the approved mockup.
export const colors = {
  bg0: "#0d0f12",
  bg1: "#14171c",
  bg2: "#1b1f26",
  bg3: "#232833",
  line: "#2a303b",
  text0: "#e8eaee",
  text1: "#9aa3b2",
  text2: "#5c6575",
  accent: "#38d6c4",
  accentDim: "rgba(56, 214, 196, 0.13)",
  red: "#e05656",
  amber: "#f0a63c",
  green: "#4fc07a",
} as const;

// Camelot wheel badge colors, keyed by canonical Camelot code.
const camelotColors: Record<string, string> = {
  "1A": "#7ee8c9", "1B": "#56d6a8",
  "2A": "#a7e87e", "2B": "#84d656",
  "3A": "#d6e87e", "3B": "#c9d656",
  "4A": "#e8d07e", "4B": "#d6b356",
  "5A": "#e8a97e", "5B": "#d68a56",
  "6A": "#e8877e", "6B": "#d65f56",
  "7A": "#e87ea3", "7B": "#d65685",
  "8A": "#d67ee8", "8B": "#bb56d6",
  "9A": "#a97ee8", "9B": "#8a56d6",
  "10A": "#7e8fe8", "10B": "#5670d6",
  "11A": "#7ec2e8", "11B": "#56aad6",
  "12A": "#7ee8e4", "12B": "#56d6cf",
};

/** Normalize a raw tag value ("8a", "08A") to a Camelot code, or null. */
export function normalizeCamelot(raw: string | null | undefined): string | null {
  if (!raw) return null;
  const m = raw.trim().toUpperCase().match(/^0?([1-9]|1[0-2])\s*([AB])$/);
  return m ? `${m[1]}${m[2]}` : null;
}

/** Badge color for a raw key value; falls back to a neutral for non-Camelot keys. */
export function keyBadgeColor(raw: string | null | undefined): string {
  const code = normalizeCamelot(raw);
  return (code && camelotColors[code]) || "#5c6575";
}

/** Quality score bands: green >= 75, amber 45-74, red < 45. */
export function qualityBand(score: number): "good" | "warn" | "bad" {
  return score >= 75 ? "good" : score >= 45 ? "warn" : "bad";
}

export function qualityColor(score: number): string {
  const band = qualityBand(score);
  return band === "good" ? "#4fc07a" : band === "warn" ? "#f0a63c" : "#e05656";
}

/** Timestamp from epoch millis (number or numeric string) as DD.MM.YYYY HH:mm. */
/** Lossless formats read in the accent colour so lossy files stand out while
 *  scanning. `format` is the file extension (see metadata::AUDIO_EXTENSIONS), so an
 *  .m4a holding ALAC counts as lossy here — the extension can't reveal the codec. */
const LOSSLESS_FORMATS = ["flac", "wav", "aiff", "aif"];

export function isLossless(format: string | null | undefined): boolean {
  if (!format) return false;
  return LOSSLESS_FORMATS.includes(format.trim().toLowerCase());
}

export function formatDate(ms: number | string | null | undefined): string {
  if (ms == null) return "–";
  const n = typeof ms === "string" ? Number(ms) : ms;
  if (!Number.isFinite(n) || n <= 0) return "–";
  const d = new Date(n);
  const day = String(d.getDate()).padStart(2, "0");
  const month = String(d.getMonth() + 1).padStart(2, "0");
  const hours = String(d.getHours()).padStart(2, "0");
  const minutes = String(d.getMinutes()).padStart(2, "0");
  return `${day}.${month}.${d.getFullYear()} ${hours}:${minutes}`;
}

/** Full-precision timestamp as DD.MM.YYYY HH:mm:ss.SSS (millis is the stored resolution). */
export function formatDateFull(ms: number | string | null | undefined): string {
  if (ms == null) return "–";
  const n = typeof ms === "string" ? Number(ms) : ms;
  if (!Number.isFinite(n) || n <= 0) return "–";
  const d = new Date(n);
  const day = String(d.getDate()).padStart(2, "0");
  const month = String(d.getMonth() + 1).padStart(2, "0");
  const hours = String(d.getHours()).padStart(2, "0");
  const minutes = String(d.getMinutes()).padStart(2, "0");
  const seconds = String(d.getSeconds()).padStart(2, "0");
  const millis = String(d.getMilliseconds()).padStart(3, "0");
  return `${day}.${month}.${d.getFullYear()} ${hours}:${minutes}:${seconds}.${millis}`;
}

export function formatDuration(secs: number | null | undefined): string {
  if (secs == null || !isFinite(secs)) return "–";
  const total = Math.round(secs);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}
