import { useEffect, useState } from "react";
import * as api from "../api";
import ModalBackdrop from "./ModalBackdrop";

interface Props {
  letter: string;
  label: string;
  /** Export only this Serato crate; omitted = every crate on the stick. */
  crate?: { file: string; name: string };
  onClose: () => void;
}

/** Exports one stick's Serato crates and DJ data as rekordbox.xml. The
 *  progress pill appears on the first sync:progress event, never from here:
 *  a fast export can finish before this promise settles. */
export default function RekordboxExportDialog({ letter, label, crate, onClose }: Props) {
  const [preview, setPreview] = useState<api.SyncPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const crateFile = crate?.file ?? null;

  useEffect(() => {
    let cancelled = false;
    api
      .syncPreview(letter, crateFile ? [crateFile] : null)
      .then((p) => {
        if (!cancelled) setPreview(p);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [letter, crateFile]);

  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [onClose]);

  const start = () => {
    api
      .syncToRekordbox(letter, crateFile ? [crateFile] : null)
      .then(() => onClose())
      .catch((e) => setError(String(e)));
  };

  return (
    <ModalBackdrop onClose={onClose}>
      <div className="modal enhance-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title-row">
          <div className="modal-title-main">
            <div className="modal-title">
              {crate ? `Export “${crate.name}” to Rekordbox` : "Export to Rekordbox"}
            </div>
            <div className="modal-sub">{crate ? `${label} · one crate` : `${label} · all crates`}</div>
          </div>
        </div>
        {error && <p className="modal-note serato-note">{error}</p>}
        {crate && (
          <p className="modal-note serato-note">
            The stick's Rekordbox file then holds only this crate. Run the export from the
            Serato crates heading to put every crate back in.
          </p>
        )}
        {preview && (
          <>
            <p className="modal-note">
              {preview.tracks} tracks ·{" "}
              {preview.crates === 1
                ? "1 Serato crate becomes a playlist"
                : `${preview.crates} Serato crates become playlists`}
              , with
              grids, hot cues, loops, key and colour read from the files.
            </p>
            {preview.capability_losses.length > 0 && (
              <div className="field">
                <span>Not carried over</span>
                <ul className="modal-note">
                  {preview.capability_losses.map((l) => (
                    <li key={l}>{l}</li>
                  ))}
                </ul>
              </div>
            )}
            <div className="field">
              <span>Saved to</span>
              <div className="mono modal-note">{preview.out_path}</div>
            </div>
            <div className="field">
              <span>In Rekordbox</span>
              <ol className="modal-note rbx-steps">
                <li>
                  Preferences → Advanced → Database → rekordbox xml → Imported Library: Browse to
                  this file. Only once, or again when the stick gets a different drive letter.
                </li>
                <li>Preferences → View → Layout: tick “rekordbox xml” so it shows in the tree.</li>
                <li>
                  In the tree, open rekordbox xml → Playlists, right-click a playlist → Import
                  Playlist. After a new export, refresh that tree entry first.
                </li>
                <li>Export to the stick as usual.</li>
              </ol>
            </div>
            <p className="modal-note">
              File → Import is not the way in: it only takes playlist files like .m3u. The export
              writes only this one file to the stick; no audio file and no Serato or Rekordbox data
              is touched. Its playlists show under “Prepared for Rekordbox” in the sidebar until
              Rekordbox has them.
            </p>
          </>
        )}
        <div className="modal-actions">
          {preview && (
            <button className="btn" onClick={() => api.revealPath(preview.out_path).catch(() => {})}>
              Show file
            </button>
          )}
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn primary" disabled={!preview} onClick={start}>
            Export
          </button>
        </div>
      </div>
    </ModalBackdrop>
  );
}
