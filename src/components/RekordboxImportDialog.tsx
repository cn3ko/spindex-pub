import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import * as api from "../api";
import ModalBackdrop from "./ModalBackdrop";

interface Props {
  letter: string;
  label: string;
  /** Called right before the import is requested, so the pill shows while
   *  the stick is read; `onStartFailed` undoes it if the request is refused.
   *  Set before the call so a fast rbimport:done can never be overtaken. */
  onStarted: () => void;
  onStartFailed: () => void;
  onClose: () => void;
}

/** Imports a Rekordbox XML onto one stick for Serato: grids, hot cues, loops
 *  and colours into the files (only where Serato has nothing), playlists as
 *  crates. Shows the preview first; the progress pill appears on the first
 *  rbimport:progress event. */
function gb(bytes: number): string {
  return `${(bytes / 1e9).toFixed(1)} GB`;
}

export default function RekordboxImportDialog({
  letter,
  label,
  onStarted,
  onStartFailed,
  onClose,
}: Props) {
  const [xmlPath, setXmlPath] = useState<string | null>(null);
  const [preview, setPreview] = useState<api.RekordboxImportPreview | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [onClose]);

  useEffect(() => {
    if (!xmlPath) return;
    let cancelled = false;
    setLoading(true);
    setPreview(null);
    setError(null);
    api
      .rekordboxImportPreview(letter, xmlPath)
      .then((p) => {
        if (!cancelled) setPreview(p);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [letter, xmlPath]);

  const pick = async () => {
    const chosen = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "Rekordbox XML", extensions: ["xml"] }],
    });
    if (typeof chosen === "string") setXmlPath(chosen);
  };

  const start = () => {
    if (!xmlPath) return;
    onStarted();
    api
      .rekordboxImport(letter, xmlPath)
      .then(() => onClose())
      .catch((e) => {
        onStartFailed();
        setError(String(e));
      });
  };

  const tooFull =
    !!preview && preview.free_space !== null && preview.space_needed > preview.free_space;

  const nothingToDo =
    !!preview &&
    preview.grids + preview.cues + preview.loops + preview.colours + preview.links_added === 0 &&
    preview.crates_new.length === 0;

  return (
    <ModalBackdrop onClose={onClose}>
      <div className="modal enhance-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title-row">
          <div className="modal-title-main">
            <div className="modal-title">Import from Rekordbox</div>
            <div className="modal-sub">{label} · into Serato</div>
          </div>
        </div>
        <p className="modal-note">
          In Rekordbox: File → Export Collection in xml format. Then pick that file here. SpinDex
          adds grids, hot cues, loops and colours only where Serato has nothing yet, and turns
          playlists into crates on this stick. Nothing Serato already has is changed.
        </p>
        <div className="field">
          <span>Rekordbox XML</span>
          <div className="rbx-import-pick">
            <button className="btn" onClick={() => pick().catch((e) => setError(String(e)))}>
              Choose XML…
            </button>
            <span className="mono modal-note">{xmlPath ?? "no file chosen"}</span>
          </div>
        </div>
        {error && <p className="modal-note serato-note">{error}</p>}
        {loading && <p className="modal-note">Reading the XML and the stick…</p>}
        {preview && (
          <>
            <p className="modal-note">
              {preview.matched} of {preview.tracks_in_xml} Rekordbox tracks are on this stick
              {preview.not_on_stick > 0 && ` (${preview.not_on_stick} are not, and are skipped)`}.
            </p>
            <div className="field">
              <span>Will be added</span>
              <ul className="modal-note">
                <li>{preview.grids} beat grids</li>
                <li>{preview.cues} hot cues</li>
                <li>{preview.loops} loops</li>
                <li>{preview.colours} track colours</li>
                <li>
                  {preview.crates_new.length} new crates
                  {preview.crates_new.length > 0 && `: ${preview.crates_new.join(", ")}`}
                </li>
                {preview.crates_merged.length > 0 && (
                  <li>tracks added to existing crates: {preview.crates_merged.join(", ")}</li>
                )}
              </ul>
            </div>
            {(preview.cues_skipped > 0 ||
              preview.loops_skipped > 0 ||
              preview.memory_cues_dropped > 0 ||
              preview.playlist_entries_missing > 0 ||
              preview.unreadable > 0 ||
              preview.unsupported > 0) && (
              <div className="field">
                <span>Left out</span>
                <ul className="modal-note">
                  {preview.cues_skipped > 0 && (
                    <li>{preview.cues_skipped} hot cues whose Serato slot is already used</li>
                  )}
                  {preview.loops_skipped > 0 && (
                    <li>{preview.loops_skipped} loops with no free Serato loop slot</li>
                  )}
                  {preview.memory_cues_dropped > 0 && (
                    <li>{preview.memory_cues_dropped} memory cues (Serato has none)</li>
                  )}
                  {preview.playlist_entries_missing > 0 && (
                    <li>
                      {preview.playlist_entries_missing} playlist entries whose track is not on this stick — those
                      crates come out shorter
                    </li>
                  )}
                  {preview.unreadable > 0 && (
                    <li>{preview.unreadable} files whose Serato data could not be read — left untouched</li>
                  )}
                  {preview.unsupported > 0 && (
                    <li>{preview.unsupported} files in a format SpinDex cannot write cues into (m4a, aac)</li>
                  )}
                </ul>
              </div>
            )}
            {preview.space_needed > 0 && (
              <p className={"modal-note" + (tooFull ? " serato-note" : "")}>
                Needs about {gb(preview.space_needed)} on the stick for backups
                {preview.free_space !== null && `; ${gb(preview.free_space)} free`}
                {tooFull && ". Not enough space — free some room first."}
              </p>
            )}
            <p className="modal-note serato-note">
              Close Serato first. Each file is backed up once before its first change; nothing is
              deleted.
            </p>
          </>
        )}
        <div className="modal-actions">
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn primary" disabled={!preview || nothingToDo || tooFull} onClick={start}>
            {nothingToDo ? "Nothing to import" : "Import"}
          </button>
        </div>
      </div>
    </ModalBackdrop>
  );
}
