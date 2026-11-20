import { useEffect, useLayoutEffect, useRef, useState } from "react";

type Anchor = { left: number; top: number; bottom: number; text: string };

/**
 * Single styled tooltip for the whole app. Any element with a `data-tip`
 * attribute gets it — no props to thread through, and no native `title`
 * popups (grey, delayed, OS-styled) anywhere in the UI.
 */
export default function HoverTip() {
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onOver = (e: MouseEvent) => {
      const target = e.target as HTMLElement | null;
      const el = target?.closest?.("[data-tip]") as HTMLElement | null;
      const text = el?.dataset.tip;
      if (!el || !text) {
        setAnchor(null);
        return;
      }
      const r = el.getBoundingClientRect();
      setAnchor({ left: r.left, top: r.top, bottom: r.bottom, text });
    };
    const hide = () => setAnchor(null);
    document.addEventListener("mouseover", onOver);
    document.addEventListener("mouseleave", hide);
    document.addEventListener("mousedown", hide);
    window.addEventListener("scroll", hide, true);
    window.addEventListener("blur", hide);
    return () => {
      document.removeEventListener("mouseover", onOver);
      document.removeEventListener("mouseleave", hide);
      document.removeEventListener("mousedown", hide);
      window.removeEventListener("scroll", hide, true);
      window.removeEventListener("blur", hide);
    };
  }, []);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el || !anchor) return;
    const r = el.getBoundingClientRect();
    const left = Math.max(8, Math.min(anchor.left, window.innerWidth - r.width - 8));
    const below = anchor.bottom + 6;
    const top = below + r.height > window.innerHeight - 8 ? anchor.top - r.height - 6 : below;
    el.style.left = `${left}px`;
    el.style.top = `${Math.max(8, top)}px`;
  }, [anchor]);

  if (!anchor) return null;
  return (
    <div className="hover-tip" ref={ref}>
      {anchor.text}
    </div>
  );
}
