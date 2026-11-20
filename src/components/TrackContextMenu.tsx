import { useEffect, useLayoutEffect, useRef, useState } from "react";

export interface CtxItem {
  label: string;
  disabled?: boolean;
  danger?: boolean;
  action?: () => void;
  submenu?: { key: number; label: string; hint?: string; action: () => void }[];
}

interface Props {
  x: number;
  y: number;
  items: CtxItem[];
  onClose: () => void;
}

/** Fixed-position context menu. Closes on click-away, Escape, scroll, or any
 *  action. Clamped to the viewport; submenu opens on hover. */
export default function TrackContextMenu({ x, y, items, onClose }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const [sub, setSub] = useState<number | null>(null);
  const [pos, setPos] = useState({ x, y });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos({
      x: Math.max(4, Math.min(x, window.innerWidth - r.width - 4)),
      y: Math.max(4, Math.min(y, window.innerHeight - r.height - 4)),
    });
  }, [x, y]);

  useEffect(() => {
    const down = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    const scroll = (e: Event) => {
      if (ref.current && e.target instanceof Node && ref.current.contains(e.target)) return;
      onClose();
    };
    window.addEventListener("mousedown", down);
    window.addEventListener("keydown", key);
    window.addEventListener("scroll", scroll, true);
    return () => {
      window.removeEventListener("mousedown", down);
      window.removeEventListener("keydown", key);
      window.removeEventListener("scroll", scroll, true);
    };
  }, [onClose]);

  return (
    <div
      ref={ref}
      className="ctx-menu"
      style={{ left: pos.x, top: pos.y }}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((it, i) => (
        <div
          key={it.label}
          className={`ctx-item ${it.disabled ? "disabled" : ""} ${it.danger ? "danger" : ""}`}
          onMouseEnter={() => setSub(it.submenu ? i : null)}
          onClick={() => {
            if (it.disabled || !it.action) return;
            onClose();
            it.action();
          }}
        >
          {it.label}
          {it.submenu && <span className="ctx-arrow">▸</span>}
          {it.submenu && sub === i && !it.disabled && (
            <div className="ctx-sub">
              {it.submenu.map((s) => (
                <div
                  key={s.key}
                  className="ctx-item"
                  onClick={(e) => {
                    e.stopPropagation();
                    onClose();
                    s.action();
                  }}
                >
                  {s.label}
                  {s.hint && <span className="mono dim ctx-hint">{s.hint}</span>}
                </div>
              ))}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
