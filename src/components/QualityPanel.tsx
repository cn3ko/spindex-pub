import { useEffect, useRef, useState } from "react";
import * as api from "../api";
import { qualityColor } from "../theme";
import ModalBackdrop from "./ModalBackdrop";

const SPEC_TIME = 256;
const SPEC_FREQ = 128;
const SPEC_FMIN = 50;
// The spectrogram is always measured at 44,100 Hz regardless of the file's
// native sample rate, so its Nyquist (axis top / cutoff-line geometry) is
// fixed too. `sample_rate` remains a purely informational fact elsewhere.
const SPEC_NYQUIST = 22050;
/** Mirrors core::tempo::UNSTEADY_PCT — past this a track fights beatmatching. */
const UNSTEADY_PCT = 1.5;

/** Plain-words interpretation of the measurements. */
function verdict(d: api.QualityDetail): string {
  const parts: string[] = [];
  const lossless = d.format != null && ["flac", "wav", "aiff"].includes(d.format);
  if (d.cutoff_hz != null) {
    const khz = (d.cutoff_hz / 1000).toFixed(1);
    if (d.cutoff_hz >= 19000) {
      parts.push(`Full bandwidth (~${khz} kHz).`);
    } else if (d.cutoff_hz >= 16500) {
      parts.push(`Bandwidth ~${khz} kHz — consistent with a ~192-256 kbps source.`);
    } else if (d.cutoff_hz >= 14000) {
      parts.push(
        `Bandwidth ~${khz} kHz — likely a ~128 kbps source` +
          (lossless || (d.bitrate_kbps ?? 0) > 200 ? ` re-encoded as ${d.format?.toUpperCase() ?? "a larger file"}.` : "."),
      );
    } else {
      parts.push(`Bandwidth only ~${khz} kHz — heavily band-limited source.`);
    }
  } else if (d.loudness_lufs != null || d.quality_score != null) {
    // analyzed, no lossy edge found — cutoff null means full band, not "no data"
    parts.push("No band-limiting detected — full bandwidth.");
  }
  if ((d.clip_ratio ?? 0) > 0.01) parts.push("Heavy sustained clipping.");
  else if ((d.clip_ratio ?? 0) > 0.001) parts.push("Some clipping.");
  if (d.loudness_lufs != null && d.loudness_lufs < -16) {
    parts.push("Quiet master — loudness is fixable and does not affect the score.");
  }
  return parts.join(" ") || "No measurements yet — run Analyze on this track.";
}

/** Plain-words tempo verdict. `bpm` and `drift` come straight off TrackRow. */
export function tempoLabel(bpm: number | null, drift: number | null): string {
  if (bpm == null) return "Tempo not analyzed";
  if (drift == null) return `${bpm.toFixed(1)} BPM`;
  if (drift < UNSTEADY_PCT) return `${bpm.toFixed(1)} BPM · steady (±${drift.toFixed(1)}%)`;
  const lo = bpm * (1 - drift / 200);
  const hi = bpm * (1 + drift / 200);
  return `${lo.toFixed(0)}–${hi.toFixed(0)} BPM · drifts ${drift.toFixed(1)}%`;
}

/** Modal showing a track's measured quality: spectrogram, loudness meter, and a
 *  plain-words verdict. Read-only — "Enhance…" opens the enhance dialog for
 *  this track (this panel closes to make room for it). */
export default function QualityPanel({
  track,
  apolloOk,
  onClose,
  onEnhance,
  onRestore,
  onStabilize,
}: {
  track: api.TrackRow;
  /** Whether the ML restoration sidecar (Apollo) and its model are present. */
  apolloOk: boolean;
  onClose: () => void;
  onEnhance: (trackId: number) => void;
  onRestore: (trackId: number) => void;
  onStabilize: (trackId: number) => void;
}) {
  const [detail, setDetail] = useState<api.QualityDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    let cancelled = false;
    setDetail(null);
    setError(null);
    api.qualityDetail(track.id).then(
      (d) => {
        if (!cancelled) setDetail(d);
      },
      (e) => {
        if (!cancelled) setError(String(e));
      },
    );
    return () => {
      cancelled = true;
    };
  }, [track.id]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onClose]);

  // Spectrogram: columns left->right in time, low frequencies at the bottom.
  useEffect(() => {
    const canvas = canvasRef.current;
    const spec = detail?.spectrogram;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    if (!spec || spec.length !== SPEC_TIME * SPEC_FREQ) {
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      return;
    }
    const img = ctx.createImageData(SPEC_TIME, SPEC_FREQ);
    for (let t = 0; t < SPEC_TIME; t++) {
      for (let b = 0; b < SPEC_FREQ; b++) {
        const v = spec[t * SPEC_FREQ + b] / 255;
        const y = SPEC_FREQ - 1 - b; // low bands at the bottom
        const i = (y * SPEC_TIME + t) * 4;
        // dark -> turquoise -> white ramp on the app palette
        img.data[i] = Math.round(255 * Math.max(0, v * 2 - 1) * 0.85 + 13 * (1 - v));
        img.data[i + 1] = Math.round(214 * Math.min(1, v * 1.6));
        img.data[i + 2] = Math.round(196 * Math.min(1, v * 1.5) + 18 * (1 - v));
        img.data[i + 3] = 255;
      }
    }
    ctx.putImageData(img, 0, 0);

    // cutoff line: y position of cutoff_hz on the log band axis
    if (detail.cutoff_hz != null && detail.cutoff_hz < SPEC_NYQUIST * 0.95) {
      const frac = Math.log(detail.cutoff_hz / SPEC_FMIN) / Math.log(SPEC_NYQUIST / SPEC_FMIN);
      const y = Math.round((1 - Math.min(1, Math.max(0, frac))) * (SPEC_FREQ - 1));
      ctx.fillStyle = "#e05656";
      ctx.fillRect(0, y, SPEC_TIME, 1);
    }
  }, [detail]);

  const score = detail?.quality_score ?? track.quality_score;
  const lufs = detail?.loudness_lufs;
  const drift = track.tempo_drift_pct;
  const unsteady = drift != null && drift >= UNSTEADY_PCT;
  // LUFS meter: -30 .. 0 scale, club reference band -14..-8
  const lufsPct = (v: number) => Math.min(100, Math.max(0, ((v + 30) / 30) * 100));

  return (
    <ModalBackdrop onClose={onClose}>
      <div className="modal quality-panel" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title-row">
          <div className="modal-title-main">
            <div className="modal-title">Quality</div>
            <div className="modal-sub">
              {(track.artist ?? "?") + " — " + (track.title ?? track.rel_path)}
            </div>
          </div>
          {score != null && (
            <span className="quality-score" style={{ color: qualityColor(score) }}>
              {score}
            </span>
          )}
        </div>
        {error && <div className="modal-note quality-error">{error}</div>}
        <canvas
          ref={canvasRef}
          width={SPEC_TIME}
          height={SPEC_FREQ}
          className="quality-spectrogram"
        />
        <div className="quality-axis mono dim">
          <span>{SPEC_FMIN} Hz</span>
          <span>22 kHz (log)</span>
        </div>
        {lufs != null && (
          <div className="quality-lufs">
            <span className="quality-label">Loudness</span>
            <div className="quality-meter">
              <div
                className="quality-ref"
                style={{ left: `${lufsPct(-14)}%`, width: `${lufsPct(-8) - lufsPct(-14)}%` }}
              />
              <div className="quality-value" style={{ left: `${lufsPct(lufs)}%` }} />
            </div>
            <span className="mono dim">{lufs.toFixed(1)} LUFS</span>
          </div>
        )}
        <div className="quality-lufs">
          <span className="quality-label">Tempo</span>
          <span className={`mono ${unsteady ? "quality-unsteady" : "dim"}`}>
            {tempoLabel(track.bpm, track.tempo_drift_pct)}
          </span>
        </div>
        <div className="quality-facts mono dim">
          {detail?.cutoff_hz != null && <span>cutoff ~{(detail.cutoff_hz / 1000).toFixed(1)} kHz</span>}
          {detail?.clip_ratio != null && <span>clipping {(detail.clip_ratio * 100).toFixed(2)}%</span>}
          {detail?.bitrate_kbps != null && (
            <span>
              {detail.bitrate_kbps} kbps {detail.format?.toUpperCase()}
            </span>
          )}
        </div>
        <p className="quality-verdict">{detail ? verdict(detail) : error ? "" : "Loading…"}</p>
        <div className="modal-actions">
          <button className="btn" onClick={() => onEnhance(track.id)}>
            Enhance…
          </button>
          <button
            className="btn"
            disabled={drift == null}
            onClick={() => onStabilize(track.id)}
            data-tip={drift == null ? "analyze this track first" : undefined}
          >
            Steady tempo…
          </button>
          {apolloOk ? (
            <button className="btn" onClick={() => onRestore(track.id)}>
              Restore (ML)…
            </button>
          ) : (
            <span className="modal-note muted quality-apollo-hint">
              Restore (ML) — run scripts/setup-apollo.ps1 to enable
            </span>
          )}
          <button className="btn" onClick={onClose}>
            Close
          </button>
        </div>
      </div>
    </ModalBackdrop>
  );
}
