import { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import * as api from "../api";
import { formatDuration } from "../theme";
import { registerDeckAudio } from "../viz/audioTap";
import GridEditor from "./GridEditor";
import { actionFor, comboOf, tip, useBindings } from "../shortcuts";
import { bandColour } from "../waveColors";
import { useElementSize } from "../useElementSize";

interface Props {
  track: api.TrackRow | null;
  /** Name of the library the loaded track belongs to. */
  libraryName: string | null;
  /** Bumped by the parent to force replay of the same track. */
  playToken: number;
  /** Bumped when an analysis run finishes so the waveform refreshes. */
  waveToken: number;
  /** Bumped when artwork changes so the cover refreshes. */
  artToken: number;
  onPrev: () => void;
  onNext: () => void;
  /** Natural end of the track (repeat-one is decided by the parent). */
  onEnded: () => void;
  shuffle: boolean;
  repeat: "off" | "all" | "one";
  onToggleShuffle: () => void;
  onCycleRepeat: () => void;
  onError: (message: string, trackId: number) => void;
  /** Opens the metadata editor for the loaded track. */
  onEdit: () => void;
  /** Grid mode: the deck grows and shows the beat-grid editor. */
  gridMode: boolean;
  onToggleGrid: () => void;
  onGridDirtyChange: (dirty: boolean) => void;
  onGridSaved: () => void;
  /** Grid editor errors: a toast only, never the playback-failure path. */
  onGridError: (msg: string) => void;
  onNotify: (msg: string) => void;
}

function trackDisplayName(t: api.TrackRow): string {
  if (t.title) return t.title;
  const file = t.rel_path.split("/").pop() ?? t.rel_path;
  return file.replace(/\.[^.]+$/, "");
}

const ACCENT = "#38d6c4";
const LINE = "#2a303b";
const PLACEHOLDER = "#232833";

export default function DeckPlayer({
  track,
  libraryName,
  playToken,
  waveToken,
  artToken,
  onPrev,
  onNext,
  onEnded,
  shuffle,
  repeat,
  onToggleShuffle,
  onCycleRepeat,
  onError,
  onEdit,
  gridMode,
  onToggleGrid,
  onGridDirtyChange,
  onGridSaved,
  onGridError,
  onNotify,
}: Props) {
  const audioRef = useRef<HTMLAudioElement>(null);
  /** The source set for the current track; errors from any other are ignored. */
  const loadedSrcRef = useRef<string | null>(null);
  const gridSupported = !!track && api.GRID_FORMATS.includes((track.format ?? "").toLowerCase());
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const canvasSize = useElementSize(canvasRef);
  const [playing, setPlaying] = useState(false);
  const [position, setPosition] = useState(0);
  const [duration, setDuration] = useState(0);
  const [volume, setVolume] = useState(0.5);
  const [peaks, setPeaks] = useState<number[] | null>(null);
  const [bands, setBands] = useState<number[] | null>(null);
  const [artUrl, setArtUrl] = useState<string | null>(null);

  // Make the audio element reachable for the visualizer's Web Audio tap.
  useEffect(() => {
    if (audioRef.current) registerDeckAudio(audioRef.current);
  }, []);

  // Cover art; the temp file path is stable per track, so cache-bust with
  // the tokens that signal a possible change.
  useEffect(() => {
    if (!track) {
      setArtUrl(null);
      return;
    }
    let stale = false;
    api
      .artworkPath(track.id)
      .then((p) => {
        if (!stale) {
          setArtUrl(p ? `${convertFileSrc(p)}?v=${track.id}-${artToken}` : null);
        }
      })
      .catch(() => {
        if (!stale) setArtUrl(null);
      });
    return () => {
      stale = true;
    };
  }, [track?.id, artToken]); // eslint-disable-line react-hooks/exhaustive-deps

  // Load + autoplay whenever the track (or replay token) changes.
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    if (!track) {
      audio.pause();
      audio.removeAttribute("src");
      setPlaying(false);
      setPosition(0);
      setDuration(0);
      setPeaks(null);
      return;
    }
    let stale = false;
    setPosition(0);
    // Until the new source is set, the element still plays the previous
    // track; an error from that source is not this track's failure.
    loadedSrcRef.current = null;
    api
      .preparePlayback(track.id)
      .then((info) => {
        if (stale) return;
        // cache-buster: replaying a track reuses its path
        audio.src = `${convertFileSrc(info.path)}?v=${track.id}-${playToken}`;
        loadedSrcRef.current = audio.src; // read back: the browser normalizes it
        return audio.play();
      })
      .catch((e) => {
        if (!stale) onError(String(e), track.id);
      });
    return () => {
      stale = true;
    };
  }, [track?.id, playToken]); // eslint-disable-line react-hooks/exhaustive-deps

  // Waveform peaks load separately so an analysis refresh (waveToken bump)
  // never interrupts playback.
  useEffect(() => {
    if (!track) {
      setPeaks(null);
      setBands(null);
      return;
    }
    let stale = false;
    api
      .getWaveform(track.id)
      .then((p) => {
        if (!stale) setPeaks(p);
      })
      .catch(() => {});
    api
      .getWaveformBands(track.id)
      .then((b) => {
        if (!stale) setBands(b);
      })
      .catch(() => {
        if (!stale) setBands(null);
      });
    return () => {
      stale = true;
    };
  }, [track?.id, waveToken]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    const audio = audioRef.current;
    if (audio) audio.volume = volume;
  }, [volume]);

  // Waveform rendering: 400 peak buckets, played part in accent color.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    const w = canvas.clientWidth;
    const h = canvas.clientHeight;
    if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
      canvas.width = w * dpr;
      canvas.height = h * dpr;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const progress = duration > 0 ? position / duration : 0;
    if (!peaks || peaks.length === 0) {
      // no analysis yet: flat placeholder bar
      ctx.fillStyle = PLACEHOLDER;
      ctx.fillRect(0, h / 2 - 1.5, w, 3);
      ctx.fillStyle = ACCENT;
      ctx.fillRect(0, h / 2 - 1.5, w * progress, 3);
      return;
    }
    const n = peaks.length;
    const step = w / n;
    const barW = Math.max(1, step - 0.75);
    // Normalize to the track's own maximum with a gentle 0.85 exponent:
    // enough lift that quiet passages stay readable, while keeping real
    // dynamic contrast between loud and quiet sections. The loudest peak
    // reaches only 70% of the canvas so the waveform has headroom and
    // the track's dynamics ("breathing") stay visible.
    const max = Math.max(1, ...peaks);
    const hasBands = !!bands && bands.length === n * 3;
    // Bass RMS dwarfs the other bands in absolute terms, so a raw energy
    // blend paints everything red. Normalize each band against its own
    // track-wide maximum ("how active is this band relative to itself",
    // Serato-style) and square the weights so the dominant band pops.
    let maxL = 1;
    let maxM = 1;
    let maxH = 1;
    if (hasBands) {
      for (let i = 0; i < n; i++) {
        maxL = Math.max(maxL, bands![i * 3]);
        maxM = Math.max(maxM, bands![i * 3 + 1]);
        maxH = Math.max(maxH, bands![i * 3 + 2]);
      }
    }
    for (let i = 0; i < n; i++) {
      const amp = Math.max(2, Math.pow(peaks[i] / max, 0.85) * h * 0.7);
      const x = i * step;
      const played = i / n <= progress;
      const colour = hasBands
        ? bandColour(bands![i * 3] / maxL, bands![i * 3 + 1] / maxM, bands![i * 3 + 2] / maxH, played ? 1 : 0.45)
        : null;
      if (colour) {
        ctx.fillStyle = colour;
        ctx.globalAlpha = 1;
      } else {
        ctx.fillStyle = played ? ACCENT : LINE;
        ctx.globalAlpha = played ? 1 : 0.9;
      }
      ctx.fillRect(x, (h - amp) / 2, barW, amp);
    }
    ctx.globalAlpha = 1;
    // playhead
    if (duration > 0) {
      ctx.fillStyle = "#e8eaee";
      ctx.fillRect(w * progress - 0.75, 0, 1.5, h);
    }
  }, [peaks, bands, position, duration, canvasSize]);

  const seek = useCallback(
    (e: React.MouseEvent<HTMLCanvasElement>) => {
      const audio = audioRef.current;
      if (!audio || !duration) return;
      const rect = e.currentTarget.getBoundingClientRect();
      const frac = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
      audio.currentTime = frac * duration;
    },
    [duration],
  );

  const togglePlay = useCallback(() => {
    const audio = audioRef.current;
    if (!audio || !track) return;
    if (audio.paused) audio.play().catch(() => {});
    else audio.pause();
  }, [track]);

  // The deck's shortcuts (play/pause, the grid editor), as bound in the
  // shortcut settings. Not while typing or in a dialog; the visualizer keeps
  // play/pause, as it always had.
  const keys = useBindings();
  const canToggleGrid = gridMode || (!!track && gridSupported);
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target && ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName)) return;
      if (document.querySelector(".modal-backdrop")) return;
      const combo = comboOf(e);
      const hit = combo ? actionFor(combo, keys) : null;
      if (hit?.id === "playPause") {
        e.preventDefault();
        togglePlay();
      } else if (hit?.id === "gridMode" && canToggleGrid && !document.querySelector(".viz-overlay")) {
        e.preventDefault();
        onToggleGrid();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [togglePlay, keys, canToggleGrid, onToggleGrid]);

  return (
    <div className={"deck" + (gridMode ? " grid-mode" : "")}>
      <audio
        ref={audioRef}
        // CORS-enabled load: the asset protocol answers with
        // Access-Control-Allow-Origin, and without this the viz's
        // MediaElementSource would taint and permanently silence all output.
        crossOrigin="anonymous"
        onPlay={() => setPlaying(true)}
        onPause={() => setPlaying(false)}
        onEnded={onEnded}
        onTimeUpdate={(e) => setPosition(e.currentTarget.currentTime)}
        onDurationChange={(e) => setDuration(e.currentTarget.duration || 0)}
        onError={() => {
          const src = audioRef.current?.src;
          if (track && src && src === loadedSrcRef.current) onError("playback failed", track.id);
        }}
      />
      <div className="deck-row">
        <div className="art">
          {artUrl ? (
            <img src={artUrl} alt="" className="art-img" draggable={false} />
          ) : (
            <svg viewBox="0 0 24 24" className={playing ? "spinning" : ""}>
              <circle cx="12" cy="12" r="9" />
              <circle cx="12" cy="12" r="2.6" />
            </svg>
          )}
        </div>
        <div className="deck-info">
          <div className="name">{track ? trackDisplayName(track) : "No track loaded"}</div>
          <div className="artist">{track?.artist ?? "—"}</div>
          <div className="deck-lib" data-tip={libraryName ?? undefined}>
            {track && libraryName ? libraryName : "—"}
          </div>
          <div className="deck-meta">
            <div>
              BPM <b>{track?.bpm != null ? track.bpm.toFixed(1) : "—"}</b>
            </div>
            <div>
              KEY <b>{track?.key_camelot ?? "—"}</b>
            </div>
            <div>
              FMT <b>{track?.format?.toUpperCase() ?? "—"}</b>
            </div>
          </div>
          <button className="deck-edit" onClick={onEdit} disabled={!track}>
            Edit metadata
          </button>
          <button
            className={"deck-edit" + (gridMode ? " active" : "")}
            onClick={onToggleGrid}
            disabled={!gridMode && (!track || !gridSupported)}
            data-tip={
              track && !gridSupported
                ? "Grid editing supports MP3, WAV, AIFF and FLAC"
                : gridMode
                  ? tip("Close the grid editor", "gridMode", keys)
                  : tip("Edit the beat grid", "gridMode", keys)
            }
          >
            Grid
          </button>
        </div>
        <div className="transport">
          <div className="transport-row">
          <button className="tbtn" onClick={onPrev} data-tip="Previous" disabled={!track}>
            <svg viewBox="0 0 24 24">
              <path d="M6 5v14M20 5l-10 7 10 7z" />
            </svg>
          </button>
          <button className="tbtn play" onClick={togglePlay} data-tip={tip("Play / pause", "playPause", keys)} disabled={!track}>
            {playing ? (
              <svg viewBox="0 0 24 24">
                <path d="M8 5v14M16 5v14" />
              </svg>
            ) : (
              <svg viewBox="0 0 24 24">
                <path d="M7 4l13 8-13 8z" />
              </svg>
            )}
          </button>
          <button className="tbtn" onClick={onNext} data-tip="Next" disabled={!track}>
            <svg viewBox="0 0 24 24">
              <path d="M18 5v14M4 5l10 7-10 7z" />
            </svg>
          </button>
          </div>
          <div className="transport-row modes">
          <button
            className={"tbtn mode" + (shuffle ? " active" : "")}
            onClick={onToggleShuffle}
            data-tip={shuffle ? "Shuffle on" : "Shuffle off"}
            disabled={!track}
          >
            <svg viewBox="0 0 24 24">
              <path fill="none" d="M3 17h4L17 7h4M18 4l3 3-3 3M3 7h4l10 10h4M18 14l3 3-3 3" />
            </svg>
          </button>
          <button
            className={"tbtn mode" + (repeat !== "off" ? " active" : "")}
            onClick={onCycleRepeat}
            data-tip={
              repeat === "off" ? "Repeat off" : repeat === "all" ? "Repeat all" : "Repeat one"
            }
            disabled={!track}
          >
            <svg viewBox="0 0 24 24">
              <path fill="none" d="M17 3l4 4-4 4M21 7H8a5 5 0 0 0-5 5M7 21l-4-4 4-4M3 17h13a5 5 0 0 0 5-5" />
            </svg>
            {repeat === "one" && <span className="rpt-one mono">1</span>}
          </button>
          </div>
        </div>
        <div className="wave-wrap">
          <canvas ref={canvasRef} className="wave" onClick={seek} />
          <div className="wave-footer">
            <div className="wave-time mono">
              {formatDuration(position)} / {formatDuration(duration || track?.duration_secs)}
            </div>
            <div className="volume">
              <svg viewBox="0 0 24 24">
                <path d="M4 9v6h4l5 4V5L8 9H4zM16 9a4 4 0 0 1 0 6" />
              </svg>
              <input
                type="range"
                min={0}
                max={1}
                step={0.02}
                value={volume}
                onChange={(e) => setVolume(Number(e.target.value))}
              />
            </div>
          </div>
        </div>
      </div>
      {gridMode && track && gridSupported && (
        <GridEditor
          key={track.id}
          track={track}
          audio={audioRef.current}
          position={position}
          duration={duration}
          overviewPeaks={peaks}
          overviewBands={bands}
          onDirtyChange={onGridDirtyChange}
          onSaved={onGridSaved}
          onError={onGridError}
          onNotify={onNotify}
        />
      )}
    </div>
  );
}
