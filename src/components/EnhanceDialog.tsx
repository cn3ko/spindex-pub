import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { EnhanceFormat, EnhanceMode } from "../api";
import ModalBackdrop from "./ModalBackdrop";

interface Props {
  trackIds: number[];
  seratoDevice: boolean;
  onStart: (mode: EnhanceMode, format: EnhanceFormat, destDir: string | null) => void;
  onClose: () => void;
}

/** Dialog offering to loudness-normalize (and optionally transcode to FLAC)
 *  a selection of tracks, either as a new file alongside the original or by
 *  replacing it in place (with a first-touch backup). */
export default function EnhanceDialog({ trackIds, seratoDevice, onStart, onClose }: Props) {
  const [mode, setMode] = useState<EnhanceMode>("new_file");
  const [format, setFormat] = useState<EnhanceFormat>("keep");
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
  const seratoWarn = seratoDevice && mode === "replace";

  return (
    <ModalBackdrop onClose={onClose}>
      <div className="modal enhance-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title-row">
          <div className="modal-title-main">
            <div className="modal-title">Enhance</div>
            <div className="modal-sub">
              {n === 1 ? "1 track" : `${n} tracks`}
            </div>
          </div>
        </div>

        <div className="field">
          <span>Mode</span>
          <label className="radio-row">
            <input
              type="radio"
              name="enhance-mode"
              checked={mode === "new_file"}
              onChange={() => setMode("new_file")}
            />
            Save as a new file (original untouched)
          </label>
          <label className="radio-row">
            <input
              type="radio"
              name="enhance-mode"
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
                name="enhance-dest"
                checked={destDir === null}
                onChange={() => setDestDir(null)}
              />
              Next to the original
            </label>
            <label className="radio-row">
              <input
                type="radio"
                name="enhance-dest"
                checked={destDir !== null}
                onChange={async () => {
                  const dir = await open({ directory: true, title: "Save enhanced files to…" });
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
                  const dir = await open({ directory: true, title: "Save enhanced files to…" });
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
              name="enhance-format"
              checked={format === "keep"}
              onChange={() => setFormat("keep")}
            />
            Keep current format
          </label>
          <label className="radio-row">
            <input
              type="radio"
              name="enhance-format"
              checked={format === "flac"}
              onChange={() => setFormat("flac")}
            />
            Convert to FLAC
          </label>
        </div>

        <p className="modal-note">
          Loudness is normalized to a target of −9 LUFS; tracks already at target are
          skipped. {mode === "replace" && (
            <>The original is backed up to <code>_SpinDexBackups_</code> before being
            overwritten — use "Restore original" to undo.</>
          )}
        </p>
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
            onClick={() => {
              onStart(mode, format, mode === "new_file" ? destDir : null);
              onClose();
            }}
          >
            Enhance
          </button>
        </div>
      </div>
    </ModalBackdrop>
  );
}
