import { useEffect, useState, type RefObject } from "react";

/**
 * The element's current CSS size, updated whenever it changes (window resize,
 * sidebar drag, the deck growing for grid mode). A canvas drawn at a fixed
 * pixel size has to redraw then, or it shows a stretched old bitmap.
 */
export function useElementSize(ref: RefObject<HTMLElement | null>): { w: number; h: number } {
  const [size, setSize] = useState({ w: 0, h: 0 });
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => {
      const { width, height } = entry.contentRect;
      setSize((s) => (s.w === width && s.h === height ? s : { w: width, h: height }));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [ref]);
  return size;
}
