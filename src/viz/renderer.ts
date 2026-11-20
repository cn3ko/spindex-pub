import { COMMON_HEADER, SCENES } from "./scenes";
import type { Rgb } from "./engine";

export interface FrameUniforms {
  time: number;
  bass: number;
  mid: number;
  treble: number;
  level: number;
  beat: number;
  beats: number;
  progress: number;
  col1: Rgb;
  col2: Rgb;
  col3: Rgb;
}

const VERTEX_SRC = `#version 300 es
layout(location = 0) in vec2 pos;
void main() { gl_Position = vec4(pos, 0.0, 1.0); }
`;

const SPECTRUM_W = 128;
const PEAKS_W = 512;

const FLOAT_UNIFORMS = [
  "u_time",
  "u_bass",
  "u_mid",
  "u_treble",
  "u_level",
  "u_beat",
  "u_beats",
  "u_progress",
] as const;

type UniformMap = Record<string, WebGLUniformLocation | null>;

/**
 * Fullscreen-quad fragment-shader renderer. One program per scene; a
 * crossfade draws the old scene opaque and the new one alpha-blended on top.
 * The constructor throws when WebGL2 or shader compilation is unavailable —
 * the caller falls back to a 2D canvas scene.
 */
export class VizRenderer {
  private gl: WebGL2RenderingContext;
  private programs: WebGLProgram[] = [];
  private uniforms: UniformMap[] = [];
  private spectrumTex: WebGLTexture;
  private peaksTex: WebGLTexture;
  private spectrumBuf = new Uint8Array(SPECTRUM_W);
  private quad: WebGLBuffer;

  constructor(canvas: HTMLCanvasElement) {
    const gl = canvas.getContext("webgl2", { alpha: false, antialias: false });
    if (!gl) throw new Error("webgl2 unavailable");
    this.gl = gl;

    for (const scene of SCENES) {
      const prog = this.link(VERTEX_SRC, COMMON_HEADER + scene.frag);
      this.programs.push(prog);
      const locs: UniformMap = {};
      for (const name of [
        ...FLOAT_UNIFORMS,
        "u_alpha",
        "u_res",
        "u_col1",
        "u_col2",
        "u_col3",
        "u_spectrum",
        "u_peaks",
      ]) {
        locs[name] = gl.getUniformLocation(prog, name);
      }
      this.uniforms.push(locs);
    }

    const quad = gl.createBuffer();
    if (!quad) throw new Error("buffer alloc failed");
    this.quad = quad;
    gl.bindBuffer(gl.ARRAY_BUFFER, quad);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);

    this.spectrumTex = this.makeTex(SPECTRUM_W);
    this.peaksTex = this.makeTex(PEAKS_W);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
  }

  private makeTex(width: number): WebGLTexture {
    const gl = this.gl;
    const tex = gl.createTexture();
    if (!tex) throw new Error("texture alloc failed");
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8, width, 1, 0, gl.RED, gl.UNSIGNED_BYTE, null);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    return tex;
  }

  private compile(type: number, src: string): WebGLShader {
    const gl = this.gl;
    const shader = gl.createShader(type);
    if (!shader) throw new Error("shader alloc failed");
    gl.shaderSource(shader, src);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
      const log = gl.getShaderInfoLog(shader);
      gl.deleteShader(shader);
      console.warn("viz shader compile failed:", log);
      throw new Error("shader compile failed");
    }
    return shader;
  }

  private link(vertSrc: string, fragSrc: string): WebGLProgram {
    const gl = this.gl;
    const prog = gl.createProgram();
    if (!prog) throw new Error("program alloc failed");
    const vs = this.compile(gl.VERTEX_SHADER, vertSrc);
    const fs = this.compile(gl.FRAGMENT_SHADER, fragSrc);
    gl.attachShader(prog, vs);
    gl.attachShader(prog, fs);
    gl.linkProgram(prog);
    gl.deleteShader(vs);
    gl.deleteShader(fs);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
      const log = gl.getProgramInfoLog(prog);
      gl.deleteProgram(prog);
      console.warn("viz shader link failed:", log);
      throw new Error("shader link failed");
    }
    return prog;
  }

  resize(w: number, h: number): void {
    this.gl.viewport(0, 0, w, h);
  }

  /** Upload the whole-track waveform peaks, resampled to 512 buckets. */
  setPeaks(peaks: number[] | null): void {
    const buf = new Uint8Array(PEAKS_W);
    if (peaks && peaks.length > 0) {
      const max = Math.max(1e-6, ...peaks);
      for (let i = 0; i < PEAKS_W; i++) {
        const src = Math.min(peaks.length - 1, Math.floor((i / PEAKS_W) * peaks.length));
        buf[i] = Math.round((peaks[src] / max) * 255);
      }
    }
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.peaksTex);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, PEAKS_W, 1, gl.RED, gl.UNSIGNED_BYTE, buf);
  }

  /** Downsample analyser byte-frequency data into the 128-wide spectrum texture. */
  setSpectrum(freq: Uint8Array): void {
    const block = Math.max(1, Math.floor(freq.length / 2 / SPECTRUM_W));
    for (let i = 0; i < SPECTRUM_W; i++) {
      let s = 0;
      const base = i * block;
      for (let j = 0; j < block; j++) s += freq[Math.min(freq.length - 1, base + j)];
      this.spectrumBuf[i] = s / block;
    }
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.spectrumTex);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, SPECTRUM_W, 1, gl.RED, gl.UNSIGNED_BYTE, this.spectrumBuf);
  }

  /**
   * Draw scene `a`; when 0 < mix < 1 also draw scene `b` blended on top with
   * weight `mix` (mix 1 means fully scene b).
   */
  draw(a: number, b: number, mix: number, u: FrameUniforms): void {
    const gl = this.gl;
    if (mix >= 1) {
      this.drawScene(b, 1, u);
      return;
    }
    gl.disable(gl.BLEND);
    this.drawScene(a, 1, u);
    if (mix > 0 && b !== a) {
      gl.enable(gl.BLEND);
      gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
      this.drawScene(b, mix, u);
      gl.disable(gl.BLEND);
    }
  }

  private drawScene(idx: number, alpha: number, u: FrameUniforms): void {
    const gl = this.gl;
    const i = ((idx % this.programs.length) + this.programs.length) % this.programs.length;
    const prog = this.programs[i];
    const locs = this.uniforms[i];
    gl.useProgram(prog);
    const vals: Record<(typeof FLOAT_UNIFORMS)[number], number> = {
      u_time: u.time,
      u_bass: u.bass,
      u_mid: u.mid,
      u_treble: u.treble,
      u_level: u.level,
      u_beat: u.beat,
      u_beats: u.beats,
      u_progress: u.progress,
    };
    for (const name of FLOAT_UNIFORMS) gl.uniform1f(locs[name], vals[name]);
    gl.uniform1f(locs.u_alpha, alpha);
    gl.uniform2f(locs.u_res, gl.drawingBufferWidth, gl.drawingBufferHeight);
    gl.uniform3f(locs.u_col1, ...u.col1);
    gl.uniform3f(locs.u_col2, ...u.col2);
    gl.uniform3f(locs.u_col3, ...u.col3);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.spectrumTex);
    gl.uniform1i(locs.u_spectrum, 0);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, this.peaksTex);
    gl.uniform1i(locs.u_peaks, 1);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  }

  dispose(): void {
    const gl = this.gl;
    for (const p of this.programs) gl.deleteProgram(p);
    this.programs = [];
    gl.deleteBuffer(this.quad);
    gl.deleteTexture(this.spectrumTex);
    gl.deleteTexture(this.peaksTex);
    // Never loseContext() here: a canvas keeps its one webgl2 context, and
    // React StrictMode's dev double-mount re-creates the renderer on the
    // same canvas — a lost context would make that (and the 2D fallback)
    // permanently fail, leaving a black overlay.
  }
}
