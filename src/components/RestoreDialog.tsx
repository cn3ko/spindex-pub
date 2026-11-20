import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { EnhanceFormat, EnhanceMode } from "../api";
import ModalBackdrop from "./ModalBackdrop";

interface Props {
  trackIds: number[];
  /** Soft-gate eligibility notes computed by App (may be empty). */
  hints: string[];
  seratoDevice: boolean;
  onStart: (mode: EnhanceMode, format: EnhanceFormat, destDir: string | null) => void;
  onClose: () => void;
}

/** Dialog offering ML restoration (Apollo) of lossy-damaged tracks, written
 *  as a new file or an in-place replacement with a first-touch backup. */
export default function RestoreDialog({ trackIds, hints, seratoDevice, onStart, onClose }: Props) {
  const [mode, setMode] = useState<EnhanceMode>("new_file");
  const [format, setFormat] = useState<EnhanceFormat>("flac");
  // NewFile only: null = save next to each original (default)
  const [destDir, setDestDir] = useState<string | null>(null);

  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [onClose]);

  const n = trackIds.length;
  const hours = (n * 30) / 60;
  const estimate =
    n === 1 ? "~30 min" : `${n} tracks ≈ ${hours < 1.05 ? "1 hour" : `${hours.toFixed(1)} hours`}`;

  return (
    <ModalBackdrop onClose={onClose}>
      <div className="modal enhance-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title-row">
          <div className="modal-title-main">
            <div className="modal-title">Restore (ML)</div>
            <div className="modal-sub">{n === 1 ? "1 track" : `${n} tracks`}</div>
          </div>
        </div>

        <div className="field">
          <span>Mode</span>
          <label className="radio-row">
            <input
              type="radio"
              name="restore-mode"
              checked={mode === "new_file"}
              onChange={() => setMode("new_file")}
            />
            Save as a new file (original untouched)
          </label>
          <label className="radio-row">
            <input
              type="radio"
              name="restore-mode"
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
                name="restore-dest"
                checked={destDir === null}
                onChange={() => setDestDir(null)}
              />
              Next to the original
            </label>
            <label className="radio-row">
              <input
                type="radio"
                name="restore-dest"
                checked={destDir !== null}
                onChange={async () => {
                  const dir = await open({ directory: true, title: "Save restored files to…" });
                  if (typeof dir === "string") setDestDir(dir);
                }}
              />
              <span className="dest-choice">
                {destDir ? destDir : "Choose folder…"}
              </span>
            </label>
            {destDir && (
              <button
                className="btn small"
                onClick={async () => {
                  const dir = await open({ directory: true, title: "Save restored files to…" });
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
              name="restore-format"
              checked={format === "keep"}
              onChange={() => setFormat("keep")}
            />
            Keep current format
          </label>
          <label className="radio-row">
            <input
              type="radio"
              name="restore-format"
              checked={format === "flac"}
              onChange={() => setFormat("flac")}
            />
            Convert to FLAC
          </label>
        </div>

        {format === "keep" && (
          <p className="modal-note">
            Restored detail is lost again when saved to a lossy format — FLAC
            preserves it.
          </p>
        )}
        {hints.map((h) => (
          <p key={h} className="modal-note">{h}</p>
        ))}
        <p className="modal-note">
          Runs the Apollo restoration model on this machine ({estimate}, runs in
          the background). Reduces lossy compression artifacts. The improvement
          is audible rather than visible — the quality score is unlikely to
          change. {mode === "replace" && (
            <>The original is backed up to <code>_SpinDexBackups_</code> — use
            "Restore original" to undo.</>
          )}
        </p>
        {seratoDevice && mode === "replace" && (
          <p className="modal-note serato-note">
            This device has Serato crates — close Serato before replacing files.
          </p>
        )}
        <p className="modal-note muted">Apollo — JusperLee, CC BY-SA 4.0.</p>

        <div className="modal-actions">
          <button className="btn" onClick={onClose}>Cancel</button>
          <button
            className="btn primary"
            onClick={() => {
              onStart(mode, format, mode === "new_file" ? destDir : null);
              onClose();
            }}
          >
            Restore
          </button>
        </div>
      </div>
    </ModalBackdrop>
  );
}
