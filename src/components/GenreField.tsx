import { useEffect, useMemo, useState } from "react";
import * as api from "../api";

const HIDDEN_KEY = "spindex.genres.hidden";

function loadHidden(): Set<string> {
  try {
    const raw = localStorage.getItem(HIDDEN_KEY);
    if (raw) return new Set<string>(JSON.parse(raw));
  } catch {
    // corrupted value: start fresh
  }
  return new Set();
}

function saveHidden(hidden: Set<string>) {
  localStorage.setItem(HIDDEN_KEY, JSON.stringify([...hidden]));
}

/** Curated fallback suggestions for genres not yet in the library. */
export const COMMON_GENRES = [
  "House", "Deep House", "Tech House", "Progressive House", "Afro House",
  "Electro House", "Techno", "Melodic Techno", "Minimal", "Trance",
  "Psytrance", "Drum & Bass", "Jungle", "Dubstep", "UK Garage", "Breaks",
  "Breakbeat", "Electro", "Ambient", "Downtempo", "Trip-Hop", "Disco",
  "Nu-Disco", "Funk", "Soul", "Hip-Hop", "R&B", "Reggae", "Dancehall",
  "Dance", "Pop", "Rock", "Jazz", "Latin",
];

interface Entry {
  genre: string;
  count: number | null; // null = curated (no library tracks)
}

interface Props {
  value: string;
  onChange: (v: string) => void;
}

/**
 * Genre input with a suggestion dropdown: the library's own genres (with
 * counts) first, then curated common genres. Purely an input aid — free
 * text always wins and saving is unchanged.
 */
export default function GenreField({ value, onChange }: Props) {
  const [libGenres, setLibGenres] = useState<api.GenreCount[]>([]);
  const [open, setOpen] = useState(false);
  const [highlight, setHighlight] = useState(-1);
  const [hidden, setHidden] = useState<Set<string>>(loadHidden);

  const hide = (genre: string) => {
    const next = new Set(hidden);
    next.add(genre.toLowerCase());
    saveHidden(next);
    setHidden(next);
    setHighlight(-1);
  };

  const resetHidden = () => {
    const next = new Set<string>();
    saveHidden(next);
    setHidden(next);
    setHighlight(-1);
  };

  useEffect(() => {
    // Failure degrades to curated-only suggestions; never blocks editing.
    api.listGenres().then(setLibGenres).catch(() => {});
  }, []);

  const groups = useMemo(() => {
    const q = value.trim().toLowerCase();
    const inLib = new Set(libGenres.map((g) => g.genre.toLowerCase()));
    const lib: Entry[] = libGenres
      .filter((g) => !hidden.has(g.genre.toLowerCase()) && g.genre.toLowerCase().includes(q))
      .map((g) => ({ genre: g.genre, count: g.count }));
    const common: Entry[] = COMMON_GENRES.filter(
      (g) =>
        !inLib.has(g.toLowerCase()) &&
        !hidden.has(g.toLowerCase()) &&
        g.toLowerCase().includes(q),
    ).map((g) => ({ genre: g, count: null }));
    return { lib, common, flat: [...lib, ...common] };
  }, [libGenres, value, hidden]);

  const pick = (genre: string) => {
    onChange(genre);
    setOpen(false);
    setHighlight(-1);
  };

  const handleKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (!open || (groups.flat.length === 0 && hidden.size === 0)) {
      if (e.key === "ArrowDown") setOpen(true);
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setHighlight((h) => Math.min(h + 1, groups.flat.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setHighlight((h) => Math.max(h - 1, -1));
    } else if (e.key === "Enter" && highlight >= 0 && highlight < groups.flat.length) {
      e.preventDefault();
      pick(groups.flat[highlight].genre);
    } else if (e.key === "Escape") {
      // Close only the dropdown; a second Escape still closes the modal.
      e.stopPropagation();
      setOpen(false);
      setHighlight(-1);
    }
  };

  const row = (entry: Entry, index: number) => (
    <button
      key={entry.genre}
      type="button"
      className={`genre-option ${index === highlight ? "active" : ""}`}
      // mousedown fires before the input's blur — preventDefault keeps focus
      // so the click lands before the dropdown unmounts.
      onMouseDown={(e) => e.preventDefault()}
      onClick={() => pick(entry.genre)}
    >
      <span>{entry.genre}</span>
      {entry.count != null && <span className="genre-count mono">{entry.count}</span>}
      <span
        className="genre-hide"
        title="Hide from this list (tracks keep their genre)"
        onMouseDown={(e) => {
          e.preventDefault();
          e.stopPropagation();
        }}
        onClick={(e) => {
          e.stopPropagation();
          hide(entry.genre);
        }}
      >
        ✕
      </span>
    </button>
  );

  return (
    <label className="field genre-field">
      <span>Genre</span>
      <input
        value={value}
        onChange={(e) => {
          onChange(e.target.value);
          setOpen(true);
          setHighlight(-1);
        }}
        onFocus={() => setOpen(true)}
        onBlur={() => {
          setOpen(false);
          setHighlight(-1);
        }}
        onKeyDown={handleKey}
        spellCheck={false}
      />
      {open && (groups.flat.length > 0 || hidden.size > 0) && (
        <div className="genre-dropdown" onMouseDown={(e) => e.preventDefault()}>
          {groups.lib.map((g, i) => row(g, i))}
          {groups.common.length > 0 && (
            <>
              {groups.lib.length > 0 && <div className="genre-divider" />}
              <div className="genre-group">common</div>
              {groups.common.map((g, i) => row(g, groups.lib.length + i))}
            </>
          )}
          {hidden.size > 0 && (
            <button
              type="button"
              className="genre-reset"
              onMouseDown={(e) => e.preventDefault()}
              onClick={resetHidden}
            >
              {hidden.size} hidden — reset
            </button>
          )}
        </div>
      )}
    </label>
  );
}
