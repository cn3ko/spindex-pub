import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { EnhanceFormat, EnhanceMode } from "../api";
import ModalBackdrop from "./ModalBackdrop";
import { tempoLabel } from "./QualityPanel";

/** Past this the movement is usually a deliberate tempo change, not drift. */
const WARN_DRIFT_PCT = 10;
const MIN_BPM = 40;
const MAX_BPM = 220;

interface Props {
  trackIds: number[];
  /** Detected tempo of the first selected track, prefilled into the field. */
  suggestedBpm: number | null;
  /** Drift of the first selected track, for the summary line and warning. */
  drift: number | null;
  seratoDevice: boolean;
  onStart: (
    targetBpm: number | null,
    mode: EnhanceMode,
    format: EnhanceFormat,
    destDir: string | null,
  ) => void;
  onClose: () => void;
}

/** Dialog offering to warp a selection onto a rock-steady beat grid, either as
 *  a new file alongside the original or by replacing it in place (with a
 *  first-touch backup). Pitch is preserved — this is time-stretch only, so the
 *  analyzed key stays valid. */
export default function TempoDialog({
  trackIds,
  suggestedBpm,
  drift,
  seratoDevice,
  onStart,
  onClose,
}: Props) {
  const [mode, setMode] = useState<EnhanceMode>("new_file");
  const [format, setFormat] = useState<EnhanceFormat>("keep");
  const [destDir, setDestDir] = useState<string | null>(null);
  const [bpmText, setBpmText] = useState(suggestedBpm?.toFixed(1) ?? "");

  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [onClose]);

  const n = trackIds.length;
  const seratoWarn = seratoDevice && mode === "replace";
  const trimmed = bpmText.trim();
  const parsed = trimmed === "" ? null : Number(trimmed);
  const bpmValid =
    parsed === null || (Number.isFinite(parsed) && parsed >= MIN_BPM && parsed <= MAX_BPM);
  // An empty field, or the suggestion left untouched, means "each track keeps
  // its own tempo" — which is what a multi-selection needs, since one number
  // cannot be right for every track in it.
  const target =
    parsed === null || (suggestedBpm != null && Math.abs(parsed - suggestedBpm) < 0.05)
      ? null
      : parsed;

  return (
    <ModalBackdrop onClose={onClose}>
      <div className="modal enhance-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title-row">
          <div className="modal-title-main">
            <div className="modal-title">Steady tempo</div>
            <div className="modal-sub">{n === 1 ? "1 track" : `${n} tracks`}</div>
          </div>
        </div>

        <p className="modal-note">{tempoLabel(suggestedBpm, drift)}</p>

        <div className="field">
          <span>Target BPM</span>
          <input
            className="mono"
            value={bpmText}
            placeholder="keep each track's own tempo"
            onChange={(e) => setBpmText(e.target.value)}
          />
          {!bpmValid && (
            <span className="modal-note muted">
              enter a BPM between {MIN_BPM} and {MAX_BPM}
            </span>
          )}
        </div>

        <div className="field">
          <span>Mode</span>
          <label className="radio-row">
            <input
              type="radio"
              name="tempo-mode"
              checked={mode === "new_file"}
              onChange={() => setMode("new_file")}
            />
            Save as a new file (original untouched)
          </label>
          <label className="radio-row">
            <input
              type="radio"
              name="tempo-mode"
              checked={mode === "replace"}
              onChange={() => setMode("replace")}
            />
            Replace the file in place
          </label>
        </div>

        {mode === "new_file" && (
          <div className="field">
            <span>Destination</span>
            <label className="radio-row">
              <input
                type="radio"
                name="tempo-dest"
                checked={destDir === null}
                onChange={() => setDestDir(null)}
              />
              Next to the original
            </label>
            <label className="radio-row">
              <input
                type="radio"
                name="tempo-dest"
                checked={destDir !== null}
                onChange={async () => {
                  const dir = await open({ directory: true, title: "Save steady files to…" });
                  if (typeof dir === "string") setDestDir(dir);
                }}
              />
              <span className="dest-choice">{destDir ? destDir : "Choose folder…"}</span>
            </label>
            {destDir && (
              <button
                className="btn small"
                onClick={async () => {
                  const dir = await open({ directory: true, title: "Save steady files to…" });
                  if (typeof dir === "string") setDestDir(dir);
                }}
              >
                Change…
              </button>
            )}
          </div>
        )}

        <div className="field">
          <span>Format</span>
          <label className="radio-row">
            <input
              type="radio"
              name="tempo-format"
              checked={format === "keep"}
              onChange={() => setFormat("keep")}
            />
            Keep current format
          </label>
          <label className="radio-row">
            <input
              type="radio"
              name="tempo-format"
              checked={format === "flac"}
              onChange={() => setFormat("flac")}
            />
            Convert to FLAC
          </label>
        </div>

        <p className="modal-note">
          The track is time-stretched onto a constant grid; pitch and key are unchanged.
          A track already steady at the target is skipped.{" "}
          {mode === "replace" && (
            <>
              The original is backed up to <code>_SpinDexBackups_</code> before being
              overwritten — use "Restore original" to undo.
            </>
          )}
        </p>
        {drift != null && drift > WARN_DRIFT_PCT && (
          <p className="modal-note serato-note">
            This track's tempo moves more than {WARN_DRIFT_PCT}% — that is usually a
            deliberate tempo change rather than drift. Flattening it will change how the
            track is meant to sound.
          </p>
        )}
        {seratoWarn && (
          <p className="modal-note serato-note">
            This device has Serato crates — close Serato before replacing files, or the
            change may be lost when Serato next writes its cache.
          </p>
        )}

        <div className="modal-actions">
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button
            className="btn primary"
            disabled={!bpmValid}
            onClick={() => {
              onStart(target, mode, format, mode === "new_file" ? destDir : null);
              onClose();
            }}
          >
            Stabilize
          </button>
        </div>
      </div>
    </ModalBackdrop>
  );
}
