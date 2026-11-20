import { useEffect, useState } from "react";
import * as api from "../api";
import ModalBackdrop from "./ModalBackdrop";

interface Props {
  tracks: api.TrackRow[];
  onClose: () => void;
  onApply: (update: api.TagUpdate, apply: string[]) => void;
  onError: (message: string) => void;
}

type FieldName =
  | "title" | "artist" | "album" | "genre" | "label" | "year" | "bpm" | "key" | "comment" | "rating";

const FIELDS: { name: FieldName; label: string; get: (t: api.TrackRow) => string; numeric?: boolean }[] = [
  { name: "title", label: "Title", get: (t) => t.title ?? "" },
  { name: "artist", label: "Artist", get: (t) => t.artist ?? "" },
  { name: "album", label: "Album", get: (t) => t.album ?? "" },
  { name: "genre", label: "Genre", get: (t) => t.genre ?? "" },
  { name: "label", label: "Label", get: (t) => t.label ?? "" },
  { name: "year", label: "Year", get: (t) => (t.year != null ? String(t.year) : ""), numeric: true },
  { name: "bpm", label: "BPM", get: (t) => (t.bpm != null ? String(t.bpm) : ""), numeric: true },
  { name: "key", label: "Key", get: (t) => t.key_camelot ?? "" },
  { name: "comment", label: "Comment", get: (t) => t.comment ?? "" },
  { name: "rating", label: "Rating 1-5", get: (t) => (t.rating != null ? String(t.rating) : ""), numeric: true },
];

/** Batch tag editor: only ticked fields are written to every selected track;
 *  a ticked-but-empty field clears that tag on all of them. */
export default function BatchMetadataEditor({ tracks, onClose, onApply, onError }: Props) {
  // common value per field, and whether the selection disagrees
  const initial = FIELDS.map((f) => {
    const vals = tracks.map((t) => f.get(t));
    const first = vals[0] ?? "";
    const mixed = !vals.every((v) => v === first);
    return { mixed, value: mixed ? "" : first };
  });

  const [enabled, setEnabled] = useState<Record<string, boolean>>({});
  const [values, setValues] = useState<Record<string, string>>(
    () => Object.fromEntries(FIELDS.map((f, i) => [f.name, initial[i].value])),
  );

  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [onClose]);

  const apply = () => {
    const applyNames = FIELDS.filter((f) => enabled[f.name]).map((f) => f.name);
    if (applyNames.length === 0) {
      onError("Tick at least one field to apply");
      return;
    }
    const yearStr = values.year?.trim() ?? "";
    const bpmStr = values.bpm?.trim() ?? "";
    const yearNum = yearStr === "" ? null : Number(yearStr);
    const bpmNum = bpmStr === "" ? null : Number(bpmStr);
    if (enabled.year && yearNum !== null && (!Number.isInteger(yearNum) || yearNum < 0)) {
      onError("Year must be a whole number");
      return;
    }
    if (enabled.bpm && bpmNum !== null && (!Number.isFinite(bpmNum) || bpmNum <= 0)) {
      onError("BPM must be a positive number");
      return;
    }
    const ratingStr = values.rating?.trim() ?? "";
    const ratingNum = ratingStr === "" ? null : Number(ratingStr);
    if (enabled.rating && ratingNum !== null && !(Number.isInteger(ratingNum) && ratingNum >= 1 && ratingNum <= 5)) {
      onError("Rating must be a whole number from 1 to 5");
      return;
    }
    const v = (n: FieldName) => (values[n]?.trim() ? values[n].trim() : null);
    const update: api.TagUpdate = {
      title: v("title"),
      artist: v("artist"),
      album: v("album"),
      genre: v("genre"),
      label: v("label"),
      year: yearNum,
      bpm: bpmNum,
      key: v("key"),
      comment: v("comment"),
      rating: ratingNum,
    };
    onApply(update, applyNames);
    onClose();
  };

  return (
    <ModalBackdrop onClose={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title">Edit metadata — {tracks.length} tracks</div>
        <div className="modal-note">
          Only ticked fields are written. A ticked field left empty clears that
          tag on all selected tracks.
        </div>
        <div className="batch-grid">
          {FIELDS.map((f, i) => (
            <label className="batch-row" key={f.name}>
              <input
                type="checkbox"
                checked={!!enabled[f.name]}
                onChange={(e) =>
                  setEnabled((prev) => ({ ...prev, [f.name]: e.target.checked }))
                }
              />
              <span className="batch-label">{f.label}</span>
              <input
                className="batch-input"
                value={values[f.name] ?? ""}
                placeholder={initial[i].mixed ? "(multiple values)" : ""}
                disabled={!enabled[f.name]}
                spellCheck={false}
                inputMode={f.numeric ? "decimal" : undefined}
                onChange={(e) =>
                  setValues((prev) => ({ ...prev, [f.name]: e.target.value }))
                }
              />
            </label>
          ))}
        </div>
        <div className="modal-actions">
          <button className="btn" onClick={onClose}>Cancel</button>
          <button className="btn primary" onClick={apply}>Apply to {tracks.length}</button>
        </div>
      </div>
    </ModalBackdrop>
  );
}
