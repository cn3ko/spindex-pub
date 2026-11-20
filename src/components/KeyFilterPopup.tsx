import { useEffect, useRef } from "react";
import { keyBadgeColor } from "../theme";

/** Musical name of each Camelot code, as the wheel prints it. */
const MUSICAL: Record<string, string> = {
  "1A": "A♭m", "1B": "B",
  "2A": "E♭m", "2B": "F♯",
  "3A": "B♭m", "3B": "D♭",
  "4A": "Fm", "4B": "A♭",
  "5A": "Cm", "5B": "E♭",
  "6A": "Gm", "6B": "B♭",
  "7A": "Dm", "7B": "F",
  "8A": "Am", "8B": "C",
  "9A": "Em", "9B": "G",
  "10A": "Bm", "10B": "D",
  "11A": "F♯m", "11B": "A",
  "12A": "D♭m", "12B": "E",
};

/** Camelot codes in wheel order, for a stable chip / request order. */
export const ALL_KEYS: string[] = Array.from({ length: 12 }, (_, i) => i + 1).flatMap(
  (n) => [`${n}A`, `${n}B`],
);

/** Sort codes the way the wheel reads: 1A, 1B, 2A, … */
export function sortKeys(keys: Iterable<string>): string[] {
  const set = new Set(keys);
  return ALL_KEYS.filter((k) => set.has(k));
}

/** The keys that mix harmonically with `code`: itself, one step either way
 *  around the wheel, and its relative major/minor. */
export function harmonicNeighbours(code: string): string[] {
  const m = code.match(/^(\d+)([AB])$/);
  if (!m) return [];
  const n = Number(m[1]);
  const l = m[2];
  const step = (d: number) => ((n - 1 + d + 12) % 12) + 1;
  return [`${n}${l}`, `${step(-1)}${l}`, `${step(1)}${l}`, `${n}${l === "A" ? "B" : "A"}`];
}

const SIZE = 280;
const C = SIZE / 2;
const R_OUT = 134; // outer edge of the major (B) ring
const R_MID = 94; // boundary between the rings
const R_IN = 56; // inner edge of the minor (A) ring

const polar = (r: number, deg: number) => {
  const a = ((deg - 90) * Math.PI) / 180;
  return [C + r * Math.cos(a), C + r * Math.sin(a)];
};

/** Annular sector for wheel number n between radii r0 < r1. */
function slicePath(n: number, r0: number, r1: number): string {
  const a0 = n * 30 - 15 + 0.6;
  const a1 = n * 30 + 15 - 0.6;
  const [x0, y0] = polar(r1, a0);
  const [x1, y1] = polar(r1, a1);
  const [x2, y2] = polar(r0, a1);
  const [x3, y3] = polar(r0, a0);
  return `M${x0},${y0} A${r1},${r1} 0 0 1 ${x1},${y1} L${x2},${y2} A${r0},${r0} 0 0 0 ${x3},${y3} Z`;
}

interface Props {
  selected: Set<string>;
  onChange: (next: Set<string>) => void;
  onClose: () => void;
}

/** A Camelot wheel: click a key to show only its tracks, click more keys to
 *  add them, Shift+click to add a key with every key that mixes with it. */
export default function KeyFilterPopup({ selected, onChange, onClose }: Props) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const down = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    // capture: the button that opened the popup toggles it itself
    document.addEventListener("mousedown", down, true);
    window.addEventListener("keydown", key, true);
    return () => {
      document.removeEventListener("mousedown", down, true);
      window.removeEventListener("keydown", key, true);
    };
  }, [onClose]);

  const toggle = (code: string, withNeighbours: boolean) => {
    const next = new Set(selected);
    if (withNeighbours) {
      harmonicNeighbours(code).forEach((k) => next.add(k));
    } else if (next.has(code)) {
      next.delete(code);
    } else {
      next.add(code);
    }
    onChange(next);
  };

  const any = selected.size > 0;

  const slice = (n: number, letter: "A" | "B") => {
    const code = `${n}${letter}`;
    const on = selected.has(code);
    const [r0, r1] = letter === "B" ? [R_MID, R_OUT] : [R_IN, R_MID];
    const [tx, ty] = polar((r0 + r1) / 2, n * 30);
    return (
      <g
        key={code}
        className={`kw-slice${on ? " on" : ""}`}
        onClick={(e) => toggle(code, e.shiftKey)}
      >
        <title>
          {`${code} · ${MUSICAL[code]}${letter === "A" ? " (minor)" : " (major)"}\nClick to ${on ? "remove" : "add"} · Shift+click adds every key that mixes with it`}
        </title>
        <path
          d={slicePath(n, r0, r1)}
          fill={keyBadgeColor(code)}
          opacity={on ? 1 : any ? 0.28 : 0.82}
          stroke={on ? "#e8eaee" : "#0d0f12"}
          strokeWidth={on ? 2 : 1}
        />
        <text x={tx} y={ty - 3} className="kw-code">
          {code}
        </text>
        <text x={tx} y={ty + 9} className="kw-name">
          {MUSICAL[code]}
        </text>
      </g>
    );
  };

  return (
    <div className="menu key-filter-pop" ref={ref}>
      <div className="menu-label">Filter by key</div>
      <svg width={SIZE} height={SIZE} viewBox={`0 0 ${SIZE} ${SIZE}`} className="key-wheel">
        {Array.from({ length: 12 }, (_, i) => i + 1).map((n) => slice(n, "B"))}
        {Array.from({ length: 12 }, (_, i) => i + 1).map((n) => slice(n, "A"))}
        <g
          className={`kw-center${any ? " active" : ""}`}
          onClick={() => any && onChange(new Set())}
        >
          <title>{any ? "Show every key again" : "Click keys around the wheel"}</title>
          <circle cx={C} cy={C} r={R_IN - 4} />
          <text x={C} y={C - 4} className="kw-count">
            {any ? selected.size : "All"}
          </text>
          <text x={C} y={C + 12} className="kw-hint">
            {any ? "clear" : "keys"}
          </text>
        </g>
      </svg>
      <div className="kw-foot">
        <span>Outer ring major · inner ring minor</span>
        <span>Shift+click: add harmonic mixes</span>
      </div>
    </div>
  );
}
