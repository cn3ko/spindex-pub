// Singleton Web Audio tap on the deck's <audio> element.
// createMediaElementSource may only be called once per element, so the graph
// is built lazily on first use and kept for the app's lifetime. Once built,
// all deck audio routes through it (analyser -> destination), viz open or not.
let el: HTMLAudioElement | null = null;
let ctx: AudioContext | null = null;
let analyser: AnalyserNode | null = null;
let failed = false;

export function registerDeckAudio(audio: HTMLAudioElement): void {
  el = audio;
}

/** The deck's audio element, for reading currentTime/duration/paused. */
export function getDeckAudio(): HTMLAudioElement | null {
  return el;
}

export function getAudioTap(): { ctx: AudioContext; analyser: AnalyserNode } | null {
  if (failed) return null;
  if (!el) return null;
  // Never build the graph on a non-CORS element: a cross-origin no-CORS
  // media source makes MediaElementSource output silence permanently
  // (all element audio routes through the graph once created). Without
  // the attribute the viz degrades to peaks mode instead.
  if (el.crossOrigin !== "anonymous") return null;
  if (!ctx) {
    try {
      ctx = new AudioContext();
      const src = ctx.createMediaElementSource(el);
      analyser = ctx.createAnalyser();
      analyser.fftSize = 2048;
      analyser.smoothingTimeConstant = 0.85;
      src.connect(analyser);
      analyser.connect(ctx.destination);
    } catch {
      // If source creation failed the element still plays normally.
      failed = true;
      ctx = null;
      analyser = null;
      return null;
    }
  }
  if (ctx.state === "suspended") ctx.resume().catch(() => {});
  return analyser ? { ctx, analyser } : null;
}
