const MIN_VISIBLE_MS = 350;

/**
 * Dismisses the startup splash (see index.html). Idempotent and safe to call
 * when the element is already gone. Enforces a minimum visible time so a fast
 * load doesn't flash the splash, then fades it out and removes the node.
 */
export function hideSplash(): void {
  const el = document.getElementById("splash");
  if (!el || el.classList.contains("hidden")) return;

  const wait = Math.max(0, MIN_VISIBLE_MS - performance.now());
  window.setTimeout(() => {
    el.classList.add("hidden");
    const remove = () => el.remove();
    el.addEventListener("transitionend", remove, { once: true });
    // Fallback if the transition never fires (element detached, reduced motion).
    window.setTimeout(remove, 400);
  }, wait);
}
