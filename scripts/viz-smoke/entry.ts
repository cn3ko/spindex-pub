// Headless smoke test for the visualizer: compiles every scene, renders one
// frame each into a contact sheet, and reports VIZ-OK/VIZ-FAIL in #status.
import { VizRenderer, type FrameUniforms } from "../../src/viz/renderer";
import { SCENES } from "../../src/viz/scenes";

const TILE_W = 480;
const TILE_H = 270;

function fakeUniforms(): FrameUniforms {
  return {
    time: 12.3,
    bass: 0.7,
    mid: 0.5,
    treble: 0.4,
    level: 0.6,
    beat: 0.25,
    beats: 24.25,
    progress: 0.4,
    col1: [0.22, 0.84, 0.77],
    col2: [0.45, 0.9, 0.6],
    col3: [0.1, 0.3, 0.5],
  };
}

function run(): void {
  const status = document.getElementById("status") as HTMLHeadingElement;
  try {
    const gl = document.createElement("canvas");
    gl.width = TILE_W;
    gl.height = TILE_H;
    const renderer = new VizRenderer(gl); // throws if ANY scene fails to compile
    renderer.resize(TILE_W, TILE_H);

    const spectrum = new Uint8Array(1024);
    for (let i = 0; i < spectrum.length; i++) {
      spectrum[i] = Math.round(200 * Math.exp(-i / 200) * (0.6 + 0.4 * Math.sin(i / 7)));
    }
    renderer.setSpectrum(spectrum);
    renderer.setPeaks(Array.from({ length: 300 }, (_, i) => 0.3 + 0.7 * Math.abs(Math.sin(i / 9))));

    const cols = 2;
    const rows = Math.ceil(SCENES.length / cols);
    const sheet = document.getElementById("sheet") as HTMLCanvasElement;
    sheet.width = cols * TILE_W;
    sheet.height = rows * TILE_H;
    const ctx2d = sheet.getContext("2d");
    if (!ctx2d) throw new Error("2d context unavailable");

    const u = fakeUniforms();
    for (let i = 0; i < SCENES.length; i++) {
      renderer.draw(i, i, 0, u);
      const x = (i % cols) * TILE_W;
      const y = Math.floor(i / cols) * TILE_H;
      ctx2d.drawImage(gl, x, y);
      ctx2d.fillStyle = "#fff";
      ctx2d.font = "16px monospace";
      ctx2d.fillText(SCENES[i].name, x + 10, y + 24);
    }
    status.textContent = `VIZ-OK ${SCENES.length}`;
  } catch (err) {
    status.textContent = `VIZ-FAIL: ${err instanceof Error ? err.message : String(err)}`;
  }
}

run();
