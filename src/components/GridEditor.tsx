import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "../api";
import {
  addMarker,
  barOneAtMap,
  loopStep,
  makeLoop,
  mapLinesIn,
  mapNearestLine,
  moveSection,
  removeMarker,
  retempoSection,
  sameMap,
  scrubTime,
  sectionAt,
  sectionBeats,
  heldLines,
  stretchBetween,
  stretchHeld,
  tapBpm,
  BEATS_PER_BAR,
  type TempoMap,
} from "../gridMath";
import { actionFor, comboOf, keyOwnedElsewhere, tip, useBindings, type ActionId } from "../shortcuts";
import { getAudioTap } from "../viz/audioTap";
import ShortcutsDialog from "./ShortcutsDialog";
import { chordSegments, NO_CHORD } from "../chords";
import { keyBadgeColor } from "../theme";
import {
  bandColour,
  camelotOf,
  chordName,
  drumColour,
  DRUM_LEGEND,
  FREQ_LAYERS,
} from "../waveColors";
import { useElementSize } from "../useElementSize";
import { canRedo, canUndo, commitHistory, emptyHistory, redoHistory, undoHistory } from "../gridHistory";

interface Props {
  track: api.TrackRow;
  audio: HTMLAudioElement | null;
  position: number;
  duration: number;
  overviewPeaks: number[] | null;
  /** the deck's stored band levels, interleaved low/mid/high per overview bucket */
  overviewBands: number[] | null;
  onDirtyChange: (dirty: boolean) => void;
  onSaved: () => void;
  onError: (msg: string) => void;
  onNotify: (msg: string) => void;
}

const MIN_SPAN = 0.25; // seconds across the zoomed view (~¼ ms per pixel)
const MAX_SPAN = 30;
const ACCENT = "#38d6c4";
// The waveform is coloured by band (bass red, mids turquoise, highs blue), so
// the grid lines stay neutral: a red bar-1 line would vanish on red kicks.
const BAR1 = "#ffffff";
const BEAT = "rgba(232,234,238,0.35)";
const PLAYHEAD = "#f0a63c";
// the A-B loop in green, the DJ-software convention (and apart from the amber
// playhead): a light band between its marks, dimmer while it is off
const LOOP_FILL = "rgba(79,192,122,0.16)";
const LOOP_FILL_OFF = "rgba(79,192,122,0.06)";
const LOOP_EDGE = "rgba(79,192,122,0.9)";
// a tempo marker: where a section with its own tempo starts
const MARKER = "#b48cff";
const BAR_NUMBER = "#e8eaee";
// the real waveform's edge over the layered bands
const SHAPE_EDGE = "rgba(232,234,238,0.55)";

/**
 * The line a tempo edit of section k keeps in place: a later section's own
 * marker (so the sections before it never move), the first section's line
 * nearest the playhead.
 */
function pivotLine(m: TempoMap, k: number, playhead: number): number {
  return k > 0 ? m[k].start : mapNearestLine(m, Math.min(playhead, m[1]?.start ?? playhead));
}
const WAVE = "#5c6575";
const ZERO = "rgba(232,234,238,0.08)";
type ColourMode = "freq" | "drums";
const COLOUR_MODES: { mode: ColourMode; action: ActionId; label: string; tip: string }[] = [
  { mode: "freq", action: "colourFreq", label: "Freq", tip: "Three bands layered: bass red at the back, mids turquoise, highs pale blue in front" },
  { mode: "drums", action: "colourDrums", label: "Drums", tip: "Colour drum hits apart from sustained, tonal sound" },
];
const COLOUR_KEY = "spindex.grid.colour";
const CHORDS_KEY = "spindex.grid.chords";
const SPAN_KEY = "spindex.grid.span";
const CLICK_KEY = "spindex.grid.click";
/** A remembered number, or `fallback` when unset, unreadable or out of range. */
function loadNumber(key: string, fallback: number, min: number, max: number): number {
  try {
    const v = Number(localStorage.getItem(key));
    return localStorage.getItem(key) !== null && Number.isFinite(v) && v >= min && v <= max ? v : fallback;
  } catch {
    return fallback;
  }
}
function store(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* remembered for this session only */
  }
}
function loadChordLane(): boolean {
  try {
    return localStorage.getItem(CHORDS_KEY) !== "0"; // on unless turned off
  } catch {
    return true;
  }
}
function loadColourMode(): ColourMode {
  try {
    const v = localStorage.getItem(COLOUR_KEY);
    if (v === "freq" || v === "drums") return v; // a stored "harmony" is now the chord lane
  } catch {
    /* storage unavailable: default */
  }
  return "freq";
}

/** A press that moves less than this is a click (jump there), not a drag. */
const DRAG_SLOP_PX = 3;
const CLICK_AHEAD = 0.12; // seconds of clicks scheduled ahead

export default function GridEditor({
  track,
  audio,
  position,
  duration,
  overviewPeaks,
  overviewBands,
  onDirtyChange,
  onSaved,
  onError,
  onNotify,
}: Props) {
  const [start, setStart] = useState<api.TrackGrid | null>(null);
  // the grid as tempo sections: one for a steady track, more where it changes
  const [grid, setGrid] = useState<TempoMap | null>(null);
  const [saved, setSaved] = useState<TempoMap | null>(null);
  // Back / Forward through this track's grid changes (a fresh history per track)
  const [history, setHistory] = useState(emptyHistory);
  const [detail, setDetail] = useState<api.GridWave | null>(null);
  const [detailError, setDetailError] = useState<string | null>(null);
  // drum and harmony colours arrive after the waveform; until then (or if
  // they fail) those modes draw the frequency colours
  const [colours, setColours] = useState<api.GridColours | null>(null);
  const [coloursFailed, setColoursFailed] = useState(false);
  // zoom and the click track open as they were left (Move/Stretch grid
  // always open off, so a drag can never move a grid by surprise)
  const [span, setSpanState] = useState(() => loadNumber(SPAN_KEY, 6, MIN_SPAN, MAX_SPAN));
  const setSpan = (f: (s: number) => number) =>
    setSpanState((s) => {
      const next = f(s);
      store(SPAN_KEY, String(next));
      return next;
    });
  const [colourMode, setColourModeState] = useState<ColourMode>(loadColourMode);
  const setColourMode = (m: ColourMode) => {
    setColourModeState(m);
    try {
      localStorage.setItem(COLOUR_KEY, m);
    } catch {
      /* remembered for this session only */
    }
  };
  const [chordLane, setChordLaneState] = useState(loadChordLane);
  const setChordLane = (on: boolean) => {
    setChordLaneState(on);
    try {
      localStorage.setItem(CHORDS_KEY, on ? "1" : "0");
    } catch {
      /* remembered for this session only */
    }
  };
  const [click, setClickState] = useState(() => loadNumber(CLICK_KEY, 0, 0, 1) === 1);
  const setClick = (f: (c: boolean) => boolean) =>
    setClickState((c) => {
      const next = f(c);
      store(CLICK_KEY, next ? "1" : "0");
      return next;
    });
  // A-B loop: replay one stretch while the grid is adjusted against it
  const [markIn, setMarkIn] = useState<number | null>(null);
  const [markOut, setMarkOut] = useState<number | null>(null);
  const [looping, setLooping] = useState(false);
  const loop = useMemo(
    () => (markIn !== null && markOut !== null ? makeLoop(markIn, markOut) : null),
    [markIn, markOut],
  );
  const [saving, setSaving] = useState(false);
  const [bpmText, setBpmText] = useState("");
  const taps = useRef<number[]>([]);
  const zoomRef = useRef<HTMLCanvasElement>(null);
  const overRef = useRef<HTMLCanvasElement>(null);
  const zoomSize = useElementSize(zoomRef);
  const chordRef = useRef<HTMLCanvasElement>(null);
  const chordSize = useElementSize(chordRef);
  const overSize = useElementSize(overRef);
  // What dragging the waveform does: move through the track (default), move
  // the grid, or stretch it around the line at the playhead. `scrubAt` shows
  // where a drag has taken the playhead before the audio's own position
  // (updated a few times a second) catches up.
  const [dragMode, setDragMode] = useState<"navigate" | "move" | "stretch" | "between">("navigate");
  // the section a stretch drag holds, so its FIXED lines and beat count show
  // for it while dragging (else for the section at the playhead)
  const [dragSection, setDragSection] = useState<number | null>(null);
  const [scrubAt, setScrubAt] = useState<number | null>(null);
  const drag = useRef<
    | { kind: "grid"; x: number; section: number; from: TempoMap }
    | { kind: "stretch"; x: number; grab: number; section: number; pivot: number; from: TempoMap }
    | { kind: "between"; x: number; grab: number; section: number; from: TempoMap }
    | { kind: "scrub"; x: number; start: number; moved: boolean }
    | null
  >(null);
  const pendingSeek = useRef<number | null>(null);
  const [showKeys, setShowKeys] = useState(false);
  const keys = useBindings();

  const dirty = !!grid && !!saved && !sameMap(grid, saved);
  // the section at the playhead: what the BPM field, the nudges and the
  // marker buttons work on
  const playSection = grid ? sectionAt(grid, position) : 0;
  const sectionBpm = grid ? grid[playSection].bpm : null;
  useEffect(() => onDirtyChange(dirty), [dirty, onDirtyChange]);
  useEffect(() => () => onDirtyChange(false), [onDirtyChange]);

  // load the starting grid and the detailed waveform for this track
  useEffect(() => {
    let stale = false;
    setStart(null);
    setGrid(null);
    setSaved(null);
    setHistory(emptyHistory);
    setDetail(null);
    setDetailError(null);
    setColours(null);
    setColoursFailed(false);
    api
      .trackGrid(track.id)
      .then((s) => {
        if (stale) return;
        const g: TempoMap = s.sections.map((x) => ({ start: x.start_sec, bpm: x.bpm }));
        setStart(s);
        setGrid(g);
        setSaved(g);
      })
      .catch((e) => {
        if (!stale) onError(String(e));
      });
    api
      .gridWaveform(track.id)
      .then((d) => {
        if (stale) return;
        setDetail(d);
        // only now: the colours reuse this decode, and the waveform must
        // never wait on them
        api
          .gridColours(track.id)
          .then((c) => {
            if (!stale && c.ms === d.ms) setColours(c);
          })
          .catch(() => {
            if (!stale) setColoursFailed(true);
          });
      })
      .catch((e) => {
        if (!stale) setDetailError(String(e));
      });
    return () => {
      stale = true;
    };
  }, [track.id]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (sectionBpm !== null) setBpmText(sectionBpm.toFixed(2));
  }, [sectionBpm]);

  const centre = scrubAt ?? position;
  const view = useMemo(
    () => ({ from: centre - span / 2, to: centre + span / 2 }),
    [centre, span],
  );
  // once the audio reports the scrubbed-to position, follow the audio again
  useEffect(() => {
    if (scrubAt === null || drag.current) return;
    if (Math.abs(position - scrubAt) < 0.1) {
      setScrubAt(null);
      return;
    }
    const t = window.setTimeout(() => setScrubAt(null), 600);
    return () => clearTimeout(t);
  }, [position, scrubAt]);

  // zoomed view: the real waveform shape (¼ ms min/max), grid lines, playhead
  // in the centre
  useEffect(() => {
    const c = zoomRef.current;
    if (!c) return;
    const ctx = c.getContext("2d");
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    const w = c.clientWidth;
    const h = c.clientHeight;
    if (c.width !== w * dpr || c.height !== h * dpr) {
      c.width = w * dpr;
      c.height = h * dpr;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const secPerPx = (view.to - view.from) / w;
    const mid = h / 2;
    ctx.fillStyle = ZERO;
    ctx.fillRect(0, mid - 0.5, w, 1);
    if (detail && detail.ms > 0) {
      const { ms, bands, shape } = detail;
      // Drums before its colours arrive (or when they failed): the plain shape
      // in grey, never another mode's colours under the Drums button
      const mode = colourMode === "drums" && !colours ? "plain" : colourMode;
      const quarters = ms * 4;
      const scale = (h / 2 - 2) / 127;
      // per pixel: the lowest and highest sample in its span draw the real
      // up-and-down shape; the busiest band levels in it set the colour.
      // Zoomed past one bucket per pixel, a pixel shows the bucket it is in.
      for (let x = 0; x < w; x++) {
        const t0 = view.from + x * secPerPx;
        const t1 = t0 + secPerPx;
        if (t1 <= 0 || t0 * 1000 >= ms) continue;
        const qa = Math.max(0, Math.floor(t0 * 4000));
        const qb = Math.min(quarters, Math.max(qa + 1, Math.ceil(t1 * 4000)));
        // start from the first bucket, not from 0: zoomed in, a pixel covers
        // part of one cycle and must show only that stretch of the curve
        let mn = shape[qa * 2];
        let mx = shape[qa * 2 + 1];
        for (let q = qa + 1; q < qb; q++) {
          if (shape[q * 2] < mn) mn = shape[q * 2];
          if (shape[q * 2 + 1] > mx) mx = shape[q * 2 + 1];
        }
        if (mx === 0 && mn === 0) continue;
        const ma = Math.max(0, Math.floor(t0 * 1000));
        const mb = Math.min(ms, Math.max(ma + 1, Math.ceil(t1 * 1000)));
        // the strongest levels in the pixel's milliseconds
        const [src, stride, off] = mode === "drums" && colours ? [colours.drums, 3, 0] : [bands, 4, 1];
        let a = 0;
        let b = 0;
        let c = 0;
        for (let i = ma; i < mb; i++) {
          const o = i * stride + off;
          if (src[o] > a) a = src[o];
          if (src[o + 1] > b) b = src[o + 1];
          if (src[o + 2] > c) c = src[o + 2];
        }
        // stored on a square-root scale; ^1.4 here shows amplitude^0.7: quiet
        // attacks stay visible, loud hits still stand out from the rest
        const ease = (v: number) => Math.sign(v) * Math.pow(Math.abs(v) / 127, 1.4) * 127;
        const top = mid - ease(mx) * scale;
        const bottom = mid - ease(mn) * scale;
        if (mode !== "freq") {
          // the real shape, coloured by which kind of hit is sounding (grey
          // while the drum colours are not there)
          ctx.fillStyle = mode === "drums" ? (drumColour(a / 255, b / 255, c / 255) ?? WAVE) : WAVE;
          ctx.fillRect(x, top, 1, Math.max(1, bottom - top));
          continue;
        }
        // three bands layered, each as tall as it is busy, the tallest at the
        // back: a fixed order let the hats (near their peak all through a
        // house track) cover the kicks, so a kick and a hat on the same beat
        // now both show as nested outlines
        const layers = [a, b, c]
          .map((v, band) => ({ band, half: Math.pow(v / 255, 0.7) * (h / 2 - 2) }))
          .filter((l) => l.half > 0.5)
          .sort((p, q) => q.half - p.half);
        for (const l of layers) {
          ctx.fillStyle = FREQ_LAYERS[l.band];
          ctx.fillRect(x, mid - l.half, 1, l.half * 2);
        }
        // and the real waveform's edge, faintly, where the hit edges are exact
        ctx.fillStyle = SHAPE_EDGE;
        ctx.fillRect(x, top, 1, 1);
        ctx.fillRect(x, bottom - 1, 1, 1);
      }
    }
    if (!detail) {
      ctx.fillStyle = WAVE;
      ctx.font = "11px sans-serif";
      ctx.textAlign = "center";
      ctx.fillText(detailError ? "Waveform unavailable" : "Loading waveform…", w / 2, mid - 8);
    } else if (colourMode === "drums" && !colours) {
      // say why the waveform is grey
      const text = coloursFailed ? "Drum colours unavailable for this track" : "Drum colours loading…";
      ctx.font = "11px sans-serif";
      ctx.textAlign = "center";
      const tw = ctx.measureText(text).width;
      ctx.fillStyle = "rgba(13,15,18,0.8)";
      ctx.fillRect(w / 2 - tw / 2 - 8, h - 24, tw + 16, 18);
      ctx.fillStyle = "#e8eaee";
      ctx.fillText(text, w / 2, h - 11);
    }
    // the loop: shaded between its marks, brighter while it is playing; a
    // lone In mark shows where Out will start from
    const xAt = (t: number) => (t - view.from) / secPerPx;
    if (loop) {
      ctx.fillStyle = looping ? LOOP_FILL : LOOP_FILL_OFF;
      ctx.fillRect(xAt(loop.start), 0, xAt(loop.end) - xAt(loop.start), h);
    }
    const marks: [number, string][] = loop
      ? [
          [loop.start, "IN"],
          [loop.end, "OUT"],
        ]
      : markIn !== null
        ? [[markIn, "IN"]]
        : [];
    ctx.font = "9px sans-serif";
    ctx.textAlign = "left";
    for (const [m, label] of marks) {
      ctx.fillStyle = LOOP_EDGE;
      ctx.fillRect(xAt(m) - 1, 0, 2, h);
      ctx.fillText(label, xAt(m) + 3, 10);
    }
    if (grid) {
      const lines = mapLinesIn(grid, view.from, view.to);
      for (const l of lines) {
        const x = (l.t - view.from) / secPerPx;
        ctx.fillStyle = l.bar1 ? BAR1 : BEAT;
        ctx.fillRect(x - (l.bar1 ? 1 : 0.5), 0, l.bar1 ? 2 : 1, h);
      }
      // bar numbers at the top of each bar line (from bar 1 on), on a dark
      // backing so they read over any waveform colour
      ctx.font = "10px sans-serif";
      ctx.textAlign = "left";
      for (const l of lines) {
        if (!l.bar1 || l.beat < 0) continue;
        const x = (l.t - view.from) / secPerPx;
        const label = String(l.beat / BEATS_PER_BAR + 1);
        const tw = ctx.measureText(label).width;
        ctx.fillStyle = "rgba(13,15,18,0.75)";
        ctx.fillRect(x + 2, 2, tw + 6, 13);
        ctx.fillStyle = BAR_NUMBER;
        ctx.fillText(label, x + 5, 12);
      }
    }
    // tempo markers: where a section with its own tempo starts
    if (grid) {
      ctx.font = "9px sans-serif";
      ctx.textAlign = "left";
      for (let k = 1; k < grid.length; k++) {
        const x = (grid[k].start - view.from) / secPerPx;
        if (x < -40 || x > w + 2) continue;
        ctx.fillStyle = MARKER;
        ctx.fillRect(x - 1, 0, 2, h);
        ctx.fillRect(x - 1, 17, 40, 12); // the tab, below the bar numbers
        ctx.fillStyle = "#0d0f12";
        ctx.fillText(grid[k].bpm.toFixed(2), x + 2, 26);
      }
    }
    // stretching: the lines that stay put, in the accent colour; between two
    // markers the stretch steps whole beats, so their count shows too
    if (grid && (dragMode === "stretch" || dragMode === "between")) {
      const k = dragSection ?? sectionAt(grid, centre);
      const bounded = k < grid.length - 1;
      const heldTimes =
        dragMode === "between" ? (bounded ? [grid[k].start, grid[k + 1].start] : []) : heldLines(grid, k, centre);
      const held = heldTimes.map((t) => (t - view.from) / secPerPx);
      ctx.font = "9px sans-serif";
      ctx.textAlign = "left";
      ctx.fillStyle = ACCENT;
      for (const x of held) {
        ctx.fillRect(x - 1.5, 0, 3, h);
        ctx.fillText("FIXED", x + 4, h - 6);
      }
      if (held.length === 2) {
        const label = `${sectionBeats(grid, k)} beats`;
        const tw = ctx.measureText(label).width;
        const lx = Math.min(Math.max((held[0] + held[1]) / 2, tw / 2 + 6), w - tw / 2 - 6);
        ctx.fillStyle = "rgba(13,15,18,0.8)";
        ctx.fillRect(lx - tw / 2 - 6, h - 32, tw + 12, 15);
        ctx.fillStyle = ACCENT;
        ctx.textAlign = "center";
        ctx.fillText(label, lx, h - 21);
      }
      if (dragMode === "between" && !bounded) {
        const text = "No tempo marker after this point: add one with + Marker, then drag between the two";
        ctx.textAlign = "center";
        const tw = ctx.measureText(text).width;
        ctx.fillStyle = "rgba(13,15,18,0.8)";
        ctx.fillRect(w / 2 - tw / 2 - 8, h - 24, tw + 16, 16);
        ctx.fillStyle = "#e8eaee";
        ctx.fillText(text, w / 2, h - 12);
      }
    }
    ctx.fillStyle = PLAYHEAD;
    ctx.fillRect(w / 2 - 0.75, 0, 1.5, h);
  }, [detail, detailError, colours, coloursFailed, grid, view, zoomSize, colourMode, loop, looping, markIn, dragMode, centre, dragSection]);

  // the chord lane: the harmony analysis merged into readable stretches
  const chordSegs = useMemo(() => (colours ? chordSegments(colours.harmony) : []), [colours]);
  useEffect(() => {
    const c = chordRef.current;
    if (!c || !chordLane) return;
    const ctx = c.getContext("2d");
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    const w = c.clientWidth;
    const h = c.clientHeight;
    if (c.width !== w * dpr || c.height !== h * dpr) {
      c.width = w * dpr;
      c.height = h * dpr;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const spp = (view.to - view.from) / w;
    ctx.textAlign = "left";
    for (const s of chordSegs) {
      if (s.code === NO_CHORD) continue; // no clear chord: the lane's own grey
      const t0 = s.start / 1000;
      const t1 = s.end / 1000;
      if (t1 < view.from || t0 > view.to) continue;
      const x0 = Math.max(0, (t0 - view.from) / spp);
      const x1 = Math.min(w, (t1 - view.from) / spp);
      const code = camelotOf(s.code);
      ctx.fillStyle = keyBadgeColor(code);
      ctx.fillRect(x0 + 1, 1, Math.max(1, x1 - x0 - 2), h - 2);
      // the label where it fits: the code, and the chord's name when wide
      const name = chordName(s.code);
      const label = x1 - x0 > 110 && name ? `${code} · ${name}` : x1 - x0 > 28 ? code : null;
      if (label) {
        ctx.fillStyle = "#0d0f12";
        ctx.font = "600 10px sans-serif";
        ctx.fillText(label, x0 + 5, h / 2 + 3.5);
      }
    }
    ctx.fillStyle = PLAYHEAD;
    ctx.fillRect(w / 2 - 0.75, 0, 1.5, h);
  }, [chordSegs, view, chordSize, chordLane]);

  // the chord at the playhead, for the lane's legend
  const chordHere = useMemo(() => {
    if (!chordLane || !colours || colours.ms === 0) return null;
    const m = Math.min(colours.ms - 1, Math.max(0, Math.round(position * 1000)));
    const idx = colours.harmony[m * 2];
    const conf = colours.harmony[m * 2 + 1];
    const code = camelotOf(idx);
    return code && conf >= 64 ? `${code} · ${chordName(idx)}` : "no clear chord";
  }, [chordLane, colours, position]);

  // overview with the zoom window box
  useEffect(() => {
    const c = overRef.current;
    if (!c) return;
    const ctx = c.getContext("2d");
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    const w = c.clientWidth;
    const h = c.clientHeight;
    if (c.width !== w * dpr || c.height !== h * dpr) {
      c.width = w * dpr;
      c.height = h * dpr;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    if (overviewPeaks && overviewPeaks.length) {
      const n = overviewPeaks.length;
      const max = Math.max(1, ...overviewPeaks);
      const step = w / n;
      const bands = overviewBands && overviewBands.length === n * 3 ? overviewBands : null;
      // each band against its own loudest bucket, as on the deck
      const bandMax = [1, 1, 1];
      if (bands) for (let i = 0; i < n * 3; i++) bandMax[i % 3] = Math.max(bandMax[i % 3], bands[i]);
      overviewPeaks.forEach((p, i) => {
        const amp = Math.max(1, (p / max) * h);
        const colour = bands
          ? bandColour(bands[i * 3] / bandMax[0], bands[i * 3 + 1] / bandMax[1], bands[i * 3 + 2] / bandMax[2])
          : null;
        ctx.fillStyle = colour ?? WAVE;
        ctx.fillRect(i * step, (h - amp) / 2, Math.max(1, step - 0.5), amp);
      });
    }
    if (duration > 0) {
      const x0 = (Math.max(0, view.from) / duration) * w;
      const x1 = (Math.min(duration, view.to) / duration) * w;
      ctx.strokeStyle = ACCENT;
      ctx.fillStyle = "rgba(56,214,196,0.15)";
      ctx.fillRect(x0, 0, x1 - x0, h);
      ctx.strokeRect(x0 + 0.5, 0.5, Math.max(1, x1 - x0 - 1), h - 1);
      // tempo markers, so the sections can be found along the whole track
      if (grid) {
        ctx.fillStyle = MARKER;
        for (let k = 1; k < grid.length; k++) ctx.fillRect((grid[k].start / duration) * w - 1, 0, 2, h);
      }
      // the loop, at least 2 px wide so a short one stays findable
      if (loop) {
        const a = (loop.start / duration) * w;
        const b = (loop.end / duration) * w;
        ctx.fillStyle = looping ? LOOP_EDGE : LOOP_FILL;
        ctx.fillRect(a, 0, Math.max(2, b - a), h);
      }
    }
  }, [overviewPeaks, overviewBands, duration, view, overSize, loop, looping, grid]);

  // click track: schedule blips a little ahead of the audio, re-synced each frame
  useEffect(() => {
    if (!click || !audio || !grid) return;
    const tap = getAudioTap();
    if (!tap) {
      onNotify("The click needs Web Audio, which is unavailable here");
      setClick(() => false);
      return;
    }
    const { ctx } = tap;
    let raf = 0;
    let scheduledUpTo = -Infinity;
    const live: OscillatorNode[] = [];
    const stopAll = () => {
      live.splice(0).forEach((o) => {
        try {
          o.stop();
        } catch {
          /* already stopped */
        }
      });
    };
    let lastMedia = audio.currentTime;
    const frame = () => {
      raf = requestAnimationFrame(frame);
      const media = audio.currentTime;
      if (audio.paused || media < lastMedia - 0.005 || media - lastMedia > 0.25) {
        // paused, seeked, or a loop jumped back: drop what was queued and
        // start over from here (a short loop jumps back less than 0.25 s)
        stopAll();
        scheduledUpTo = media;
      }
      lastMedia = media;
      if (audio.paused) return;
      const rate = audio.playbackRate || 1;
      // while looping, nothing past Out: playback never gets there
      const upTo = looping && loop ? Math.min(media + CLICK_AHEAD, loop.end - 0.001) : media + CLICK_AHEAD;
      for (const l of mapLinesIn(grid, Math.max(scheduledUpTo, media), upTo)) {
        if (l.t <= scheduledUpTo) continue;
        const when = ctx.currentTime + (l.t - media) / rate;
        const osc = ctx.createOscillator();
        const gain = ctx.createGain();
        osc.frequency.value = l.bar1 ? 1760 : 1175;
        gain.gain.setValueAtTime(l.bar1 ? 0.35 : 0.2, when);
        gain.gain.exponentialRampToValueAtTime(0.0001, when + 0.03);
        osc.connect(gain).connect(ctx.destination);
        osc.start(when);
        osc.stop(when + 0.035);
        live.push(osc);
        osc.onended = () => {
          const i = live.indexOf(osc);
          if (i >= 0) live.splice(i, 1);
        };
      }
      scheduledUpTo = Math.max(scheduledUpTo, upTo);
    };
    raf = requestAnimationFrame(frame);
    return () => {
      cancelAnimationFrame(raf);
      stopAll();
    };
  }, [click, audio, grid, onNotify, looping, loop]);

  // the loop itself: at Out, back to In; a seek far past Out ends the loop
  useEffect(() => {
    if (!looping || !loop || !audio) return;
    let raf = 0;
    const frame = () => {
      raf = requestAnimationFrame(frame);
      const step = loopStep(audio.currentTime, loop);
      if (step === "wrap") audio.currentTime = loop.start;
      else if (step === "exit") setLooping(false);
    };
    raf = requestAnimationFrame(frame);
    return () => cancelAnimationFrame(raf);
  }, [looping, loop, audio]);

  // the playhead as exactly as it is known: the prop follows timeupdate (~4×/s)
  const now = useCallback(() => (audio ? audio.currentTime : position), [audio, position]);
  const setIn = useCallback(() => setMarkIn(now()), [now]);
  const setOut = useCallback(() => {
    const t = now();
    setMarkOut(t);
    // marking Out starts the loop: the usual "mark, then listen" gesture
    if (markIn !== null && makeLoop(markIn, t)) setLooping(true);
  }, [now, markIn]);
  const toggleLoop = useCallback(() => {
    if (loop) setLooping((l) => !l);
  }, [loop]);
  const clearLoop = () => {
    setMarkIn(null);
    setMarkOut(null);
    setLooping(false);
  };

  // nudges move the section at the playhead
  const nudge = (ms: number) =>
    setGrid((g) => (g ? (moveSection(g, sectionAt(g, now()), ms / 1000) ?? g) : g));

  // The editor's keyboard shortcuts, as bound in the shortcut settings. The
  // listener stays put; it reads the bindings and actions through refs, so it
  // always runs this render's handlers.
  const keysRef = useRef(keys);
  keysRef.current = keys;
  const actionsRef = useRef<Partial<Record<ActionId, (fine: boolean) => void>>>({});
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (keyOwnedElsewhere(e)) return;
      const combo = comboOf(e);
      const hit = combo ? actionFor(combo, keysRef.current) : null;
      const run = hit ? actionsRef.current[hit.id] : undefined;
      if (!hit || !run) return; // not ours (the deck's keys are DeckPlayer's)
      e.preventDefault();
      run(hit.fine);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const setBpm = useCallback(
    (bpm: number) => {
      if (!Number.isFinite(bpm) || bpm < 40 || bpm > 250) {
        // an unusable entry reverts to the grid's own tempo
        setBpmText((t) => (sectionBpm !== null ? sectionBpm.toFixed(2) : t));
        return;
      }
      setGrid((g) => {
        if (!g) return g;
        const k = sectionAt(g, position);
        const next = retempoSection(g, k, Math.round(bpm * 100) / 100, pivotLine(g, k, position));
        if (!next) setBpmText(g[k].bpm.toFixed(2)); // would squeeze a section below a beat
        return next ?? g;
      });
    },
    [position, sectionBpm],
  );

  const onTap = () => {
    const now = performance.now();
    taps.current = [...taps.current, now].slice(-16);
    const b = tapBpm(taps.current);
    if (b) setBpm(b);
  };

  // zoom around the playhead (the view's centre): factor > 1 shows more time
  const zoomBy = (factor: number) =>
    setSpan((s) => Math.min(MAX_SPAN, Math.max(MIN_SPAN, s * factor)));
  const onWheel = (e: React.WheelEvent) => zoomBy(e.deltaY > 0 ? 1.2 : 1 / 1.2);

  // Seeks the audio at most once a frame while scrubbing: a seek per mouse
  // move would queue up far more than the element can serve.
  const seekSoon = (t: number) => {
    const pending = pendingSeek.current;
    pendingSeek.current = t;
    if (pending !== null) return;
    requestAnimationFrame(() => {
      if (audio && pendingSeek.current !== null) audio.currentTime = pendingSeek.current;
      pendingSeek.current = null;
    });
  };
  const secPerPx = () => span / (zoomRef.current?.clientWidth || 1);

  const onDown = (e: React.PointerEvent<HTMLCanvasElement>) => {
    if (e.button !== 0) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    if (dragMode === "move") {
      // the section under the pointer moves
      const r = e.currentTarget.getBoundingClientRect();
      const at = view.from + (e.clientX - r.left) * secPerPx();
      if (grid) drag.current = { kind: "grid", x: e.clientX, section: sectionAt(grid, at), from: grid };
    } else if (dragMode === "stretch") {
      if (!grid) return;
      // grab the point under the pointer; its section stretches and no
      // tempo marker moves (see stretchHeld)
      const r = e.currentTarget.getBoundingClientRect();
      const grab = view.from + (e.clientX - r.left) * secPerPx();
      const k = sectionAt(grid, grab);
      drag.current = { kind: "stretch", x: e.clientX, grab, section: k, pivot: pivotLine(grid, k, now()), from: grid };
      setDragSection(k);
    } else if (dragMode === "between") {
      if (!grid) return;
      // grab the point under the pointer; both ends of its section stay
      const r = e.currentTarget.getBoundingClientRect();
      const grab = view.from + (e.clientX - r.left) * secPerPx();
      const k = sectionAt(grid, grab);
      if (k >= grid.length - 1) {
        onNotify("Stretching between markers needs a tempo marker after this point: add one with + Marker.");
        return;
      }
      drag.current = { kind: "between", x: e.clientX, grab, section: k, from: grid };
      setDragSection(k);
    } else {
      drag.current = { kind: "scrub", x: e.clientX, start: audio?.currentTime ?? position, moved: false };
    }
  };
  const onMove = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const d = drag.current;
    if (!d) return;
    const dx = e.clientX - d.x;
    if (d.kind === "grid") {
      // from the map the drag began with, so nothing piles up
      const g = moveSection(d.from, d.section, dx * secPerPx());
      if (g) setGrid(g);
      return;
    }
    if (d.kind === "stretch") {
      const g = stretchHeld(d.from, d.section, d.pivot, d.grab, d.grab + dx * secPerPx());
      if (g) setGrid(g);
      return;
    }
    if (d.kind === "between") {
      const g = stretchBetween(d.from, d.section, d.grab, d.grab + dx * secPerPx());
      if (g) setGrid(g);
      return;
    }
    if (!d.moved && Math.abs(dx) < DRAG_SLOP_PX) return; // still a click
    d.moved = true;
    const t = scrubTime(d.start, dx, secPerPx(), duration);
    setScrubAt(t);
    seekSoon(t);
  };
  const onUp = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const d = drag.current;
    drag.current = null;
    setDragSection(null);
    if (d && d.kind !== "scrub" && grid) {
      // a whole drag of the grid is one step back
      setHistory((h) => commitHistory(h, grid, performance.now(), false));
    }
    if (!d || d.kind !== "scrub") return;
    let t: number;
    if (d.moved) {
      t = scrubTime(d.start, e.clientX - d.x, secPerPx(), duration);
    } else {
      // a click: jump to the point under the pointer
      const r = e.currentTarget.getBoundingClientRect();
      t = Math.min(duration, Math.max(0, view.from + (e.clientX - r.left) * secPerPx()));
    }
    setScrubAt(t);
    if (audio) audio.currentTime = t;
  };

  const seekOverview = (e: React.MouseEvent<HTMLCanvasElement>) => {
    if (!audio || !duration) return;
    const r = e.currentTarget.getBoundingClientRect();
    audio.currentTime = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)) * duration;
  };

  const save = async () => {
    if (!grid) return;
    setSaving(true);
    try {
      await api.saveTrackGrid(track.id, grid.map((s) => ({ start_sec: s.start, bpm: s.bpm })));
      setSaved(grid);
      onNotify("Grid saved to the file. In Serato: Rescan ID3 Tags.");
      onSaved();
    } catch (e) {
      onError(`Could not save the grid: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  const discard = () => saved && setGrid(saved);
  // every finished change is a step; quick key steps in a row are one, and a
  // drag commits as one step when it ends (onUp)
  useEffect(() => {
    if (!grid || drag.current) return;
    setHistory((h) => commitHistory(h, grid, performance.now(), true));
  }, [grid]);
  const undo = () => {
    if (!grid || drag.current) return;
    const r = undoHistory(history, grid);
    if (!r) return;
    setHistory(r.history);
    setGrid(r.grid);
  };
  const redo = () => {
    if (!grid || drag.current) return;
    const r = redoHistory(history, grid);
    if (!r) return;
    setHistory(r.history);
    setGrid(r.grid);
  };
  const barOne = () => grid && setGrid(barOneAtMap(grid, now()) ?? grid);
  const markerAdd = () => {
    if (!grid) return;
    const next = addMarker(grid, now());
    if (next) setGrid(next);
    else onNotify("A tempo marker needs a beat of its own: not on or right next to another marker.");
  };
  const markerRemove = () => {
    if (!grid) return;
    const next = removeMarker(grid, now());
    if (next) setGrid(next);
  };
  const run = (id: ActionId, fine = false) => actionsRef.current[id]?.(fine);
  // every action of the grid editor, run by its key or its button alike
  actionsRef.current = {
    moveGrid: () => setDragMode((m) => (m === "move" ? "navigate" : "move")),
    stretchGrid: () => setDragMode((m) => (m === "stretch" ? "navigate" : "stretch")),
    stretchBetween: () => setDragMode((m) => (m === "between" ? "navigate" : "between")),
    click: () => setClick((c) => !c),
    nudgeEarlier: (fine) => nudge(fine ? -10 : -1),
    nudgeLater: (fine) => nudge(fine ? 10 : 1),
    barOne,
    bpmDown: () => sectionBpm !== null && setBpm(sectionBpm - 0.01),
    bpmUp: () => sectionBpm !== null && setBpm(sectionBpm + 0.01),
    markerAdd,
    markerRemove,
    tap: onTap,
    zoomOut: () => zoomBy(1.5),
    zoomIn: () => zoomBy(1 / 1.5),
    loopIn: setIn,
    loopOut: setOut,
    loopToggle: toggleLoop,
    loopClear: clearLoop,
    colourFreq: () => setColourMode("freq"),
    colourDrums: () => setColourMode("drums"),
    chordLane: () => setChordLane(!chordLane),
    undo,
    redo,
    discard: () => dirty && discard(),
    save: () => dirty && !saving && save(),
    shortcuts: () => setShowKeys(true),
  };

  const note = start
    ? [
        start.source === "file" && "Grid from the file",
        start.source === "detected" && "Grid from SpinDex's analysis",
        start.source === "bpm" && "No grid yet — started from the BPM",
        start.source === "none" && "No grid and no BPM — started at 120",
        grid && grid.length > 1 && `${grid.length} tempo sections`,
        start.file_unreadable && "the file's grid could not be read",
      ]
        .filter(Boolean)
        .join(" · ")
    : "Loading grid…";

  return (
    <div className="grid-editor">
      <canvas
        ref={chordRef}
        className={"grid-chords" + (chordLane ? "" : " off")}
        data-tip="The chords along the track: the analysis merged into stretches (grey where none is clear)"
      />
      <div className="grid-zoom-wrap">
      <canvas
        ref={zoomRef}
        className={"grid-zoom" + (dragMode !== "navigate" ? " moving-grid" : "")}
        onWheel={onWheel}
        onPointerDown={onDown}
        onPointerMove={onMove}
        onPointerUp={onUp}
        onPointerCancel={() => {
          drag.current = null;
          setDragSection(null);
        }}
        data-tip={
          dragMode === "move"
            ? "Drag to move the grid · wheel to zoom"
            : dragMode === "stretch"
              ? "Drag a beat line onto its hit to stretch its section; the FIXED lines and every tempo marker stay · wheel to zoom"
              : dragMode === "between"
                ? "Drag a beat line between two markers: both FIXED markers and every bar outside stay, the beats between them change a whole bar at a time · wheel to zoom"
              : "Drag to move through the track · click to jump there · wheel to zoom"
        }
      />
        <div className="grid-colour-mode">
          <div className="grid-colour-switch">
            {COLOUR_MODES.map((m) => (
              <button
                key={m.mode}
                className={colourMode === m.mode ? "active" : ""}
                onClick={() => setColourMode(m.mode)}
                data-tip={tip(m.tip, m.action, keys)}
              >
                {m.label}
                {m.mode !== "freq" && detail && !colours ? "…" : ""}
              </button>
            ))}
          </div>
          {colourMode === "drums" && (
            <div className="grid-colour-legend">
              {DRUM_LEGEND.map((l) => (
                <span key={l.label}>
                  <i style={{ background: l.colour }} />
                  {l.label}
                </span>
              ))}
            </div>
          )}
          <div className="grid-colour-switch">
            <button
              className={chordLane ? "active" : ""}
              onClick={() => run("chordLane")}
              data-tip={tip("The chord lane above the waveform, in the key-badge colours", "chordLane", keys)}
            >
              Chords{chordLane && detail && !colours ? "…" : ""}
            </button>
          </div>
          {chordLane && chordHere && <div className="grid-colour-legend mono">{chordHere}</div>}
        </div>
      </div>
      <canvas ref={overRef} className="grid-overview" onClick={seekOverview} />
      <div className="grid-controls">
        <span className="grid-note">
          {detailError ? `${note} · waveform unavailable: ${detailError}` : note}
        </span>
        <button
          className={"btn" + (click ? " active" : "")}
          onClick={() => run("click")}
          data-tip={tip("Click on every beat, accented on bar 1", "click", keys)}
        >
          Click
        </button>
        <button
          className={"btn" + (dragMode === "move" ? " active" : "")}
          onClick={() => run("moveGrid")}
          data-tip={tip(
            dragMode === "move"
              ? "Dragging the waveform moves the grid — turn off to drag through the track"
              : "Let dragging the waveform move the grid",
            "moveGrid",
            keys,
          )}
        >
          Move grid
        </button>
        <button
          className={"btn" + (dragMode === "stretch" ? " active" : "")}
          disabled={!grid}
          onClick={() => run("stretchGrid")}
          data-tip={tip(
            dragMode === "stretch"
              ? "Dragging a beat line stretches the grid (changes the BPM) — turn off to drag through the track"
              : "Let dragging a beat line stretch the grid: tempo markers never move. Between two markers the count steps a whole bar at a time (Serato stores whole beats there, and whole bars keep the bars after the marker in place); elsewhere the tempo stretches smoothly",
            "stretchGrid",
            keys,
          )}
        >
          Stretch grid
        </button>
        <button
          className={"btn" + (dragMode === "between" ? " active" : "")}
          disabled={!grid || grid.length < 2}
          onClick={() => run("stretchBetween")}
          data-tip={tip(
            grid && grid.length < 2
              ? "Needs a tempo marker: add one with + Marker first"
              : dragMode === "between"
                ? "Dragging a beat line stretches only between its two markers — turn off to drag through the track"
                : "Let dragging a beat line stretch the grid between two markers only: both stay put and nothing outside moves. Serato stores whole beats between markers, so the lines step a whole bar at a time and every bar after the second marker stays put — pull further for the next step",
            "stretchBetween",
            keys,
          )}
        >
          Between markers
        </button>
        <button
          className="btn"
          onClick={(e) => run("nudgeEarlier", e.shiftKey)}
          data-tip={tip("Move grid earlier 1 ms (Shift: 10 ms)", "nudgeEarlier", keys)}
        >
          ◀
        </button>
        <button
          className="btn"
          onClick={(e) => run("nudgeLater", e.shiftKey)}
          data-tip={tip("Move grid later 1 ms (Shift: 10 ms)", "nudgeLater", keys)}
        >
          ▶
        </button>
        <button
          className="btn"
          disabled={!grid}
          onClick={() => run("barOne")}
          data-tip={tip("Make the beat nearest the playhead bar 1, only for the part between its tempo markers (after a marker this adds a marker on that beat: every marker is a downbeat)", "barOne", keys)}
        >
          Bar 1 here
        </button>
        <button
          className="btn"
          disabled={!grid}
          onClick={() => run("markerAdd")}
          data-tip={tip("Tempo marker on the beat at the playhead: from there on the grid has its own tempo", "markerAdd", keys)}
        >
          + Marker
        </button>
        <button
          className="btn"
          disabled={!grid || playSection === 0}
          onClick={() => run("markerRemove")}
          data-tip={tip("Remove the tempo marker of the section at the playhead", "markerRemove", keys)}
        >
          − Marker
        </button>
        <button
          className="btn"
          disabled={!grid}
          onClick={() => run("bpmDown")}
          data-tip={tip("BPM − 0.01", "bpmDown", keys)}
        >
          −
        </button>
        <input
          className="grid-bpm mono"
          value={bpmText}
          onChange={(e) => setBpmText(e.target.value)}
          onBlur={() => setBpm(Number(bpmText))}
          onKeyDown={(e) => e.key === "Enter" && setBpm(Number(bpmText))}
        />
        <button
          className="btn"
          disabled={!grid}
          onClick={() => run("bpmUp")}
          data-tip={tip("BPM + 0.01", "bpmUp", keys)}
        >
          +
        </button>
        <button className="btn" onClick={() => run("tap")} data-tip={tip("Tap along; 4 taps set the BPM", "tap", keys)}>
          Tap
        </button>
        <span className="grid-label">Zoom</span>
        <button
          className="btn"
          disabled={span >= MAX_SPAN}
          onClick={() => run("zoomOut")}
          data-tip={tip("Zoom out (or the mouse wheel over the waveform)", "zoomOut", keys)}
        >
          −
        </button>
        <span className="grid-span mono">{span < 1 ? span.toFixed(2) : span < 10 ? span.toFixed(1) : Math.round(span)} s</span>
        <button
          className="btn"
          disabled={span <= MIN_SPAN}
          onClick={() => run("zoomIn")}
          data-tip={tip("Zoom in (or the mouse wheel over the waveform)", "zoomIn", keys)}
        >
          +
        </button>
        <span className="grid-label">Loop</span>
        <button className="btn" onClick={() => run("loopIn")} data-tip={tip("Mark the loop start at the playhead", "loopIn", keys)}>
          In
        </button>
        <button
          className="btn"
          onClick={() => run("loopOut")}
          data-tip={tip("Mark the loop end at the playhead and start looping", "loopOut", keys)}
        >
          Out
        </button>
        <button
          className={"btn" + (looping ? " active" : "")}
          disabled={!loop}
          onClick={() => run("loopToggle")}
          data-tip={loop ? tip("Loop on / off", "loopToggle", keys) : "Mark In and Out first"}
        >
          {looping ? "Looping" : "Loop"}
        </button>
        <button
          className="btn"
          disabled={markIn === null && markOut === null}
          onClick={() => run("loopClear")}
          data-tip={tip("Clear the loop marks", "loopClear", keys)}
        >
          ✕
        </button>
        <span className="grid-spacer" />
        <button
          className="btn sym"
          disabled={!canUndo(history)}
          onClick={() => run("undo")}
          data-tip={tip("Back: undo the last grid change", "undo", keys)}
        >
          ↶
        </button>
        <button
          className="btn sym"
          disabled={!canRedo(history)}
          onClick={() => run("redo")}
          data-tip={tip("Forward: redo the change you went back from", "redo", keys)}
        >
          ↷
        </button>
        <button
          className="btn"
          disabled={!dirty}
          onClick={() => run("discard")}
          data-tip={tip("Undo the changes since the last save", "discard", keys)}
        >
          Discard
        </button>
        <button
          className="btn primary"
          disabled={!dirty || saving}
          onClick={() => run("save")}
          data-tip={tip("Write the grid into the file", "save", keys)}
        >
          {saving ? "Saving…" : "Save to file"}
        </button>
        <button
          className="btn icon"
          onClick={() => run("shortcuts")}
          data-tip={tip("Keyboard shortcut settings", "shortcuts", keys)}
        >
          {/* the same gear as the table's column settings */}
          <svg viewBox="0 0 24 24">
            <path d="M12 8.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7z" />
            <path d="M19.4 13.5a7.6 7.6 0 0 0 0-3l2-1.5-2-3.5-2.4 1a7.6 7.6 0 0 0-2.6-1.5L14 2.5h-4l-.4 2.5A7.6 7.6 0 0 0 7 6.5l-2.4-1-2 3.5 2 1.5a7.6 7.6 0 0 0 0 3l-2 1.5 2 3.5 2.4-1a7.6 7.6 0 0 0 2.6 1.5l.4 2.5h4l.4-2.5a7.6 7.6 0 0 0 2.6-1.5l2.4 1 2-3.5-2-1.5z" />
          </svg>
        </button>
      </div>
      {showKeys && <ShortcutsDialog onClose={() => setShowKeys(false)} />}
    </div>
  );
}
