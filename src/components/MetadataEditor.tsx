import { useCallback, useEffect, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import * as api from "../api";
import ModalBackdrop from "./ModalBackdrop";
import GenreField from "./GenreField";
import StarRating from "./StarRating";

interface Props {
  track: api.TrackRow;
  onClose: () => void;
  /** Called after a successful save so the parent can refresh. */
  onSaved: () => void;
  /** Called after artwork was embedded/removed (applies immediately). */
  onArtworkChanged: () => void;
  onError: (message: string) => void;
  /** Track lives on a stick with Serato crates: show the cache reminder. */
  seratoHint?: boolean;
}

/** Fallback for untagged files: guess artist/title from an "Artist - Title" filename. */
function filenameGuess(relPath: string): { artist: string; title: string } {
  const file = relPath.split("/").pop() ?? relPath;
  const stem = file.replace(/\.[^.]+$/, "");
  const parts = stem.split(" - ");
  if (parts.length >= 2) {
    return { artist: parts[0].trim(), title: parts.slice(1).join(" - ").trim() };
  }
  return { artist: "", title: stem.trim() };
}

export default function MetadataEditor({
  track,
  onClose,
  onSaved,
  onArtworkChanged,
  onError,
  seratoHint,
}: Props) {
  const guess = filenameGuess(track.rel_path);
  const [title, setTitle] = useState(track.title ?? guess.title);
  const [artist, setArtist] = useState(track.artist ?? guess.artist);
  const [album, setAlbum] = useState(track.album ?? "");
  const [genre, setGenre] = useState(track.genre ?? "");
  const [label, setLabel] = useState(track.label ?? "");
  const [year, setYear] = useState(track.year != null ? String(track.year) : "");
  const [bpm, setBpm] = useState(track.bpm != null ? String(track.bpm) : "");
  const [key, setKey] = useState(track.key_camelot ?? "");
  const candidates = api.parseKeyCandidates(track.key_candidates);
  const [comment, setComment] = useState(track.comment ?? "");
  const [rating, setRating] = useState<number | null>(track.rating);
  const [saving, setSaving] = useState(false);
  const [artUrl, setArtUrl] = useState<string | null>(null);
  const [artBusy, setArtBusy] = useState(false);
  const [artVersion, setArtVersion] = useState(0);
  const [fetching, setFetching] = useState(false);
  const [suggestion, setSuggestion] = useState<api.EnrichResult | null>(null);

  useEffect(() => {
    let stale = false;
    api
      .artworkPath(track.id)
      .then((p) => {
        if (!stale) setArtUrl(p ? `${convertFileSrc(p)}?v=${artVersion}` : null);
      })
      .catch(() => {});
    return () => {
      stale = true;
    };
  }, [track.id, artVersion]);

  const changeArtwork = useCallback(async () => {
    try {
      const file = await open({
        title: "Choose a cover image",
        filters: [{ name: "Images", extensions: ["jpg", "jpeg", "png"] }],
      });
      if (!file || Array.isArray(file)) return;
      setArtBusy(true);
      await api.setArtwork(track.id, file);
      setArtVersion((n) => n + 1);
      onArtworkChanged();
    } catch (e) {
      onError(String(e));
    } finally {
      setArtBusy(false);
    }
  }, [track.id, onArtworkChanged, onError]);

  const removeArtwork = useCallback(async () => {
    try {
      setArtBusy(true);
      await api.removeArtwork(track.id);
      setArtVersion((n) => n + 1);
      onArtworkChanged();
    } catch (e) {
      onError(String(e));
    } finally {
      setArtBusy(false);
    }
  }, [track.id, onArtworkChanged, onError]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onClose]);

  const fetchOnline = useCallback(async () => {
    setFetching(true);
    setSuggestion(null);
    try {
      const result = await api.fetchOnlineMetadata(track.id);
      setSuggestion(result);
    } catch (e) {
      onError(String(e));
    } finally {
      setFetching(false);
    }
  }, [track.id, onError]);

  // Copies one suggested field's value into the corresponding editable form field.
  const applySuggested = (field: keyof api.EnrichFields, value: string) => {
    switch (field) {
      case "title": setTitle(value); break;
      case "artist": setArtist(value); break;
      case "album": setAlbum(value); break;
      case "genre": setGenre(value); break;
      case "label": setLabel(value); break;
      case "year": setYear(value); break;
    }
  };

  const save = () => {
    const yearNum = year.trim() === "" ? null : Number(year);
    const bpmNum = bpm.trim() === "" ? null : Number(bpm);
    if (yearNum !== null && (!Number.isInteger(yearNum) || yearNum < 0)) {
      onError("Year must be a whole number");
      return;
    }
    if (bpmNum !== null && (!Number.isFinite(bpmNum) || bpmNum <= 0)) {
      onError("BPM must be a positive number");
      return;
    }
    setSaving(true);
    api
      .updateTrackMetadata(track.id, {
        title: title || null,
        artist: artist || null,
        album: album || null,
        genre: genre || null,
        label: label || null,
        year: yearNum,
        bpm: bpmNum,
        key: key || null,
        comment: comment || null,
        rating,
      })
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e) => onError(String(e)))
      .finally(() => setSaving(false));
  };

  const field = (
    label: string,
    value: string,
    set: (v: string) => void,
    props: React.InputHTMLAttributes<HTMLInputElement> = {},
  ) => (
    <label className="field">
      <span>{label}</span>
      <input
        value={value}
        onChange={(e) => set(e.target.value)}
        spellCheck={false}
        {...props}
      />
    </label>
  );

  return (
    <ModalBackdrop onClose={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title-row">
          <div className="modal-title-main">
            <div className="modal-title">Edit metadata</div>
            <div className="modal-sub">{track.rel_path}</div>
          </div>
          <button className="btn fetch-online" onClick={fetchOnline} disabled={fetching}>
            {fetching ? "Searching…" : "Fetch online"}
          </button>
        </div>
        <div className="edit-art">
          <div className="edit-art-preview">
            {artUrl ? (
              <img src={artUrl} alt="" draggable={false} />
            ) : (
              <svg viewBox="0 0 24 24">
                <rect x="3" y="3" width="18" height="18" rx="2" />
                <circle cx="9" cy="9" r="2" />
                <path d="M21 15l-5-5-9 9" />
              </svg>
            )}
          </div>
          <div className="edit-art-actions">
            <button className="btn" onClick={changeArtwork} disabled={artBusy}>
              {artUrl ? "Change picture…" : "Add picture…"}
            </button>
            {artUrl && (
              <button className="btn" onClick={removeArtwork} disabled={artBusy}>
                Remove
              </button>
            )}
            <div className="edit-art-note">
              jpg/png, written into the file immediately
            </div>
          </div>
        </div>
        <div className="field-grid">
          {field("Title", title, setTitle, { autoFocus: true })}
          {field("Artist", artist, setArtist)}
          {field("Album", album, setAlbum)}
          <GenreField value={genre} onChange={setGenre} />
          {field("Label", label, setLabel)}
          {field("Year", year, setYear, { inputMode: "numeric" })}
          {field("BPM", bpm, setBpm, { inputMode: "decimal" })}
          {field("Key", key, setKey, { placeholder: "8A" })}
          {field("Comment", comment, setComment)}
          <div className="field">
            <span>Rating</span>
            <div className="star-field">
              <StarRating value={rating} onChange={setRating} />
            </div>
          </div>
        </div>
        {candidates.length > 0 && (
          <div className="key-suggestions">
            <span>Analyzer suggests:</span>
            {candidates.map((cand) => (
              <button
                key={cand.key}
                type="button"
                className={`key-chip mono ${key === cand.key ? "active" : ""}`}
                onClick={() => setKey(cand.key)}
                data-tip={`Use ${cand.key} (${Math.round(cand.p * 100)}% vote probability)`}
              >
                {cand.key} {Math.round(cand.p * 100)}%
              </button>
            ))}
          </div>
        )}
        {suggestion && suggestion.source === "none" && (
          <div className="modal-note">No online match found.</div>
        )}
        {suggestion && suggestion.source !== "none" && (
          <div className="enrich-panel">
            <div className="enrich-head">
              <span>Text-search match</span>
              {suggestion.loose && <span className="enrich-loose">approximate</span>}
            </div>
            {(
              [
                ["title", title, suggestion.fields.title],
                ["artist", artist, suggestion.fields.artist],
                ["album", album, suggestion.fields.album],
                ["genre", genre, suggestion.fields.genre],
                ["label", label, suggestion.fields.label],
                ["year", year, suggestion.fields.year != null ? String(suggestion.fields.year) : null],
              ] as [keyof api.EnrichFields, string, string | null][]
            )
              .filter(([, , sugg]) => sugg && sugg !== "")
              .map(([sfield, current, sugg]) => (
                <div className="enrich-row" key={sfield}>
                  <span className="enrich-field">{sfield}</span>
                  <span className="enrich-current">{current || "—"}</span>
                  <span className="enrich-arrow">→</span>
                  <span className="enrich-sugg">{sugg}</span>
                  <button
                    className="btn"
                    onClick={() => applySuggested(sfield, sugg as string)}
                  >
                    Use
                  </button>
                </div>
              ))}
          </div>
        )}
        <div className="modal-note">
          Saved into the audio file's tags, then mirrored to the index. Empty
          fields clear the tag.
        </div>
        {seratoHint && (
          <div className="modal-note serato-note">
            Serato shows its own cached tags &amp; artwork — after saving, use
            “Rescan ID3 Tags” in Serato's Files panel (or drag the tracks onto
            that button). Close Serato while editing.
          </div>
        )}
        <div className="modal-actions">
          <button className="btn" onClick={onClose} disabled={saving}>
            Cancel
          </button>
          <button className="btn primary" onClick={save} disabled={saving}>
            {saving ? "Saving…" : "Save to file"}
          </button>
        </div>
      </div>
    </ModalBackdrop>
  );
}
