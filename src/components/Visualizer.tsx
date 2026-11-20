import { useEffect, useRef, useState } from "react";
import * as api from "../api";
import { getAudioTap, getDeckAudio } from "../viz/audioTap";
import { bandEnergies, beatPhase, camelotPalette, type Rgb } from "../viz/engine";
import { VizRenderer } from "../viz/renderer";
import { SCENES } from "../viz/scenes";

interface Props {
  track: api.TrackRow | null;
  onClose: () => void;
}

const FADE_BEATS = 2;
const SILENCE_FRAMES = 90;

function cssColor([r, g, b]: Rgb): string {
  return `rgb(${Math.round(r * 255)}, ${Math.round(g * 255)}, ${Math.round(b * 255)})`;
}

export default function Visualizer({ track, onClose }: Props) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [scene, setScene] = useState(0);
  const [auto, setAuto] = useState(true);
  const [chromeHidden, setChromeHidden] = useState(false);
  const [flashKey, setFlashKey] = useState(0);
  const [use2d, setUse2d] = useState(false);

  // Animation state lives in refs so the rAF loop never re-renders React.
  const rendererRef = useRef<VizRenderer | null>(null);
  const sceneRef = useRef(0);
  const autoRef = useRef(true);
  const fadeRef = useRef({ prev: 0, startBeats: -1e9 });
  const anchorRef = useRef(0);
  const prevBassRef = useRef(0);
  const silenceRef = useRef(0);
  const peaksModeRef = useRef(false);
  const peaksRef = useRef<number[] | null>(null);
  const paletteRef = useRef(camelotPalette(null));
  const bpmRef = useRef<number | null>(null);
  const beatsNowRef = useRef(0);

  sceneRef.current = scene;
  autoRef.current = auto;

  // Track-dependent inputs: palette, BPM, whole-track peaks. One song keeps
  // one scene: auto mode picks it from the track id, so a song always gets
  // the same look and only track changes switch scenes.
  useEffect(() => {
    paletteRef.current = camelotPalette(track?.key_camelot);
    bpmRef.current = track?.bpm ?? null;
    anchorRef.current = 0;
    if (track && autoRef.current) {
      const pick = track.id % SCENES.length;
      if (pick !== sceneRef.current) switchScene(pick);
    }
    peaksRef.current = null;
    rendererRef.current?.setPeaks(null);
    if (!track) return;
    let stale = false;
    api
      .getWaveform(track.id)
      .then((p) => {
        if (stale || !p || p.length === 0) return;
        const max = Math.max(1e-6, ...p);
        peaksRef.current = p.map((v) => v / max);
        rendererRef.current?.setPeaks(p);
      })
      .catch(() => {});
    return () => {
      stale = true;
    };
  }, [track?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  // Renderer + rAF loop + resize, once.
  useEffect(() => {
    const canvas = canvasRef.current;
    const wrap = wrapRef.current;
    if (!canvas || !wrap) return;
    let renderer: VizRenderer | null = null;
    let ctx2d: CanvasRenderingContext2D | null = null;
    try {
      renderer = new VizRenderer(canvas);
      rendererRef.current = renderer;
      if (peaksRef.current) renderer.setPeaks(peaksRef.current);
    } catch {
      setUse2d(true);
      ctx2d = canvas.getContext("2d");
    }

    const resize = () => {
      const dpr = window.devicePixelRatio || 1;
      const w = Math.max(1, Math.round(wrap.clientWidth * dpr));
      const h = Math.max(1, Math.round(wrap.clientHeight * dpr));
      if (canvas.width !== w || canvas.height !== h) {
        canvas.width = w;
        canvas.height = h;
        renderer?.resize(w, h);
      }
    };
    resize();
    const ro = new ResizeObserver(resize);
    ro.observe(wrap);

    const tap = getAudioTap();
    const freq = tap ? new Uint8Array(tap.analyser.frequencyBinCount) : new Uint8Array(1024);
    if (!tap) peaksModeRef.current = true;
    const synth = new Uint8Array(256);
    const start = performance.now();
    let raf = 0;

    const frame = () => {
      raf = requestAnimationFrame(frame);
      const audio = getDeckAudio();
      const time = (performance.now() - start) / 1000;
      const t = audio?.currentTime ?? 0;
      const dur = audio?.duration ?? 0;
      const progress = dur > 0 ? t / dur : 0;
      const playing = !!audio && !audio.paused;

      let bands = { bass: 0, mid: 0, treble: 0, level: 0 };
      if (tap && !peaksModeRef.current) {
        tap.analyser.getByteFrequencyData(freq);
        bands = bandEnergies(freq, tap.ctx.sampleRate, tap.analyser.fftSize);
        // WebView2 CORS case: element plays but the graph reads silence.
        if (playing && t > 0.5 && bands.level === 0) {
          silenceRef.current++;
          if (silenceRef.current > SILENCE_FRAMES) peaksModeRef.current = true;
        } else if (bands.level > 0) {
          silenceRef.current = 0;
        }
      }

      const bpm = bpmRef.current ?? 120; // fallback clock so pulses stay rhythmic
      const beat = playing ? beatPhase(t, bpm, anchorRef.current) : 1;
      const beats = Math.max(0, (t - anchorRef.current) * (bpm / 60));
      beatsNowRef.current = beats;
      const pulse = Math.pow(1 - beat, 4);

      if (peaksModeRef.current) {
        // Drive everything from the stored waveform shape + beat clock.
        const pk = peaksRef.current;
        const p = pk && pk.length > 0 ? pk[Math.min(pk.length - 1, Math.floor(progress * pk.length))] : 0.5;
        const e = playing ? p : 0;
        bands = { bass: e * (0.55 + 0.45 * pulse), mid: e * 0.7, treble: e * 0.45, level: e };
        for (let i = 0; i < synth.length; i++) {
          const fall = 1 - i / synth.length;
          synth[i] = Math.round(255 * e * fall * (0.65 + 0.35 * Math.sin(i * 0.35 + time * 2)));
        }
      } else {
        // Nudge the beat anchor toward strong bass onsets.
        const flux = bands.bass - prevBassRef.current;
        prevBassRef.current = bands.bass;
        if (flux > 0.15 && beat > 0.25 && beat < 0.75) {
          const err = beat > 0.5 ? beat - 1 : beat;
          anchorRef.current += err * (60 / bpm) * 0.2;
        }
      }

      const mix = Math.min(1, Math.max(0, (beats - fadeRef.current.startBeats) / FADE_BEATS));
      const [col1, col2, col3] = paletteRef.current;
      if (renderer) {
        renderer.setSpectrum(peaksModeRef.current ? synth : freq);
        renderer.draw(fadeRef.current.prev, sceneRef.current, mix, {
          time,
          bass: bands.bass,
          mid: bands.mid,
          treble: bands.treble,
          level: bands.level,
          beat,
          beats,
          progress,
          col1,
          col2,
          col3,
        });
      } else if (ctx2d) {
        // 2D fallback: spectrum bars with beat-pulsed baseline.
        const w = canvas.width;
        const h = canvas.height;
        ctx2d.fillStyle = "#0d0f12";
        ctx2d.fillRect(0, 0, w, h);
        const data = peaksModeRef.current ? synth : freq;
        const bars = 64;
        const step = Math.floor(data.length / 2 / bars);
        const bw = w / bars;
        for (let i = 0; i < bars; i++) {
          let s = 0;
          for (let j = 0; j < step; j++) s += data[i * step + j];
          const v = s / (step * 255);
          const bh = Math.max(2, v * h * 0.7 * (0.8 + 0.2 * pulse));
          ctx2d.fillStyle = cssColor(col1);
          ctx2d.fillRect(i * bw + 1, h - bh, bw - 2, bh);
          ctx2d.fillStyle = cssColor(col2);
          ctx2d.fillRect(i * bw + 1, h - bh, bw - 2, Math.min(4, bh));
        }
        ctx2d.fillStyle = cssColor(col3);
        ctx2d.fillRect(0, h - 2 - pulse * 4, w, 2);
      }
    };
    raf = requestAnimationFrame(frame);

    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
      renderer?.dispose();
      rendererRef.current = null;
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const switchScene = (n: number) => {
    fadeRef.current = { prev: sceneRef.current, startBeats: beatsNowRef.current };
    setScene(n);
    setFlashKey((k) => k + 1);
  };

  // Keyboard controls.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")) return;
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      } else if (e.key === "ArrowRight") {
        switchScene((sceneRef.current + 1) % SCENES.length);
      } else if (e.key === "ArrowLeft") {
        switchScene((sceneRef.current + SCENES.length - 1) % SCENES.length);
      } else if (e.key >= "0" && e.key <= "9") {
        const idx = e.key === "0" ? 9 : Number(e.key) - 1;
        if (idx < SCENES.length) switchScene(idx);
      } else if (e.key === "a" || e.key === "A") {
        setAuto((v) => !v);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onClose]); // eslint-disable-line react-hooks/exhaustive-deps

  // Chrome fades after 3s without mouse movement.
  useEffect(() => {
    let timer = window.setTimeout(() => setChromeHidden(true), 3000);
    const move = () => {
      setChromeHidden(false);
      window.clearTimeout(timer);
      timer = window.setTimeout(() => setChromeHidden(true), 3000);
    };
    window.addEventListener("mousemove", move);
    return () => {
      window.clearTimeout(timer);
      window.removeEventListener("mousemove", move);
    };
  }, []);

  const name = track?.title ?? track?.rel_path.split("/").pop() ?? "";

  return (
    <div className="viz-overlay" ref={wrapRef}>
      <canvas ref={canvasRef} className="viz-canvas" />
      <div className={`viz-chrome ${chromeHidden ? "hidden" : ""}`}>
        <button className="viz-close" onClick={onClose} data-tip="Close (Esc)">
          ✕
        </button>
        <div className="viz-trackinfo">
          <div className="viz-trackname">
            {name}
            {track?.artist ? ` — ${track.artist}` : ""}
          </div>
          <div className="viz-trackmeta mono">
            {track?.bpm != null ? `${track.bpm.toFixed(1)} BPM` : "no BPM"}
            {" · "}
            {track?.key_camelot ?? "no key"}
            {" · "}
            {SCENES[scene].name}
            {auto ? " · auto" : ""}
            {use2d ? " · 2D" : ""}
          </div>
        </div>
        <div className="viz-hint">←/→ scenes · 1–9,0 direct · A auto per song · Esc close</div>
      </div>
      <div className="viz-scenename" key={flashKey}>
        {SCENES[scene].name}
      </div>
    </div>
  );
}
