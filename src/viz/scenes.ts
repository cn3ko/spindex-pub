// Fragment shaders for the visualizer. Each scene body is appended to
// COMMON_HEADER by the renderer. Every scene must end with
// `outColor = vec4(color, u_alpha);` so crossfades can alpha-blend.

export const COMMON_HEADER = `#version 300 es
precision highp float;
uniform float u_time;     // seconds since viz opened
uniform float u_bass;     // 0..1
uniform float u_mid;      // 0..1
uniform float u_treble;   // 0..1
uniform float u_level;    // 0..1
uniform float u_beat;     // beat phase 0..1
uniform float u_beats;    // absolute beat count (fractional)
uniform float u_progress; // playback position 0..1
uniform float u_alpha;    // crossfade weight
uniform vec2 u_res;
uniform vec3 u_col1;      // key base color
uniform vec3 u_col2;      // brighter shifted
uniform vec3 u_col3;      // deeper shifted
uniform sampler2D u_spectrum; // 128x1, low->high frequency
uniform sampler2D u_peaks;    // 512x1, whole-track waveform peaks
out vec4 outColor;
`;

const TUNNEL = `
void main() {
  vec2 uv = (gl_FragCoord.xy * 2.0 - u_res) / u_res.y;
  float r = length(uv) + 1e-4;
  float a = atan(uv.y, uv.x);
  float flash = pow(1.0 - u_beat, 4.0);
  float depth = 1.0 / r + u_time * (1.4 + u_bass * 1.2);
  float rings = sin(depth * 3.0) * 0.5 + 0.5;
  float spokes = sin(a * 8.0 + u_time * 0.5 + u_beats * 0.2) * 0.5 + 0.5;
  float glow = smoothstep(1.2, 0.0, r);
  vec3 col = u_col3 * 0.30;
  col += u_col1 * rings * (0.35 + u_bass * 0.4) * min(r, 1.0);
  col += u_col2 * spokes * rings * 0.22 * min(r, 1.0);
  col += u_col1 * flash * 0.12 * glow;
  col *= smoothstep(0.0, 0.22, r);
  col += u_col2 * smoothstep(0.10, 0.0, r) * (0.55 + flash * 0.25);
  outColor = vec4(col, u_alpha);
}
`;

const HALO = `
void main() {
  vec2 uv = (gl_FragCoord.xy * 2.0 - u_res) / u_res.y;
  float r = length(uv);
  float ang = atan(uv.y, uv.x) + u_beats * 0.15;
  float t = abs(fract(ang / 6.2831853) * 2.0 - 1.0);
  float amp = texture(u_spectrum, vec2(t * 0.9 + 0.02, 0.5)).r;
  float ring = 0.55 + amp * 0.35 + u_bass * 0.05;
  float d = abs(r - ring);
  float line = exp(-d * 40.0);
  float inner = exp(-abs(r - 0.35 - u_bass * 0.10) * 30.0);
  vec3 col = u_col3 * 0.25 * smoothstep(1.4, 0.2, r);
  col += u_col1 * line * (0.6 + amp * 1.5);
  col += u_col2 * inner * (0.3 + u_mid);
  col += u_col2 * exp(-d * 8.0) * 0.15;
  col += u_col1 * pow(1.0 - u_beat, 5.0) * 0.04 * smoothstep(1.2, 0.0, r);
  outColor = vec4(col, u_alpha);
}
`;

const STARFIELD = `
float hash(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
void main() {
  vec2 uv = (gl_FragCoord.xy * 2.0 - u_res) / u_res.y;
  float speed = u_time * (0.25 + u_level * 0.7);
  vec3 col = u_col3 * 0.12;
  for (int i = 0; i < 3; i++) {
    float fi = float(i);
    float z = fract(speed * (0.3 + fi * 0.2) + fi * 0.37);
    float scale = mix(18.0, 2.0, z);
    vec2 grid = uv * scale;
    vec2 cell = floor(grid);
    vec2 f = fract(grid) - 0.5;
    float h = hash(cell + fi * 7.0);
    vec2 off = vec2(hash(cell + 1.3), hash(cell + 4.7)) - 0.5;
    float d = length(f - off * 0.7);
    float star = exp(-d * d * 60.0) * step(0.75, h);
    float tw = 0.85 + 0.15 * sin(u_time * 1.6 + h * 40.0);
    col += mix(u_col1, u_col2, h) * star * tw * (0.4 + z) * (0.55 + u_treble * 0.5);
  }
  col += u_col1 * pow(1.0 - u_beat, 5.0) * u_bass * 0.05;
  outColor = vec4(col, u_alpha);
}
`;

const TERRAIN = `
void main() {
  vec2 uv = gl_FragCoord.xy / u_res;
  // window of the track's own waveform around the playhead (center = now)
  float x = fract(u_progress + (uv.x - 0.5) * 0.15 + 1.0);
  float pk = texture(u_peaks, vec2(x, 0.5)).r;
  float horizon = 0.28 + pk * 0.45 * (0.9 + u_bass * 0.15);
  float m = smoothstep(horizon, horizon - 0.006, uv.y);
  float light = exp(-abs(uv.x - 0.5) * 8.0);
  vec3 sky = mix(u_col3 * 0.20, u_col3 * 0.55, uv.y);
  sky += u_col2 * exp(-abs(uv.y - horizon) * 18.0) * (0.35 + u_mid * 0.6);
  vec3 ground = mix(u_col3 * 0.12, u_col1 * 0.55, light * (0.5 + u_bass));
  vec3 col = mix(sky, ground, m);
  col += u_col1 * light * exp(-abs(uv.y - horizon) * 10.0)
       * (0.45 + pow(1.0 - u_beat, 4.0) * 0.2);
  outColor = vec4(col, u_alpha);
}
`;

const BARS = `
void main() {
  vec2 uv = gl_FragCoord.xy / u_res;
  float mirror = abs(uv.x * 2.0 - 1.0);
  float colIdx = floor(mirror * 48.0) / 48.0;
  float amp = texture(u_spectrum, vec2(colIdx * 0.92 + 0.02, 0.5)).r;
  float flash = pow(1.0 - u_beat, 4.0);
  float base = 0.28;
  float h = amp * (0.55 + u_bass * 0.10);
  float gap = smoothstep(0.46, 0.36, abs(fract(mirror * 48.0) - 0.5));
  float above = step(base, uv.y) * step(uv.y, base + h);
  float yrel = clamp((uv.y - base) / max(h, 1e-4), 0.0, 1.0);
  vec3 barCol = mix(u_col1, u_col2, yrel);
  vec3 col = u_col3 * 0.10;
  col += barCol * above * gap * (0.55 + amp * 0.6);
  col += u_col2 * gap * smoothstep(0.012, 0.0, abs(uv.y - (base + h)));
  float ry = (base - uv.y) / max(h, 1e-4);
  float refl = step(0.0, ry) * step(ry, 1.0) * (1.0 - ry);
  col += barCol * gap * refl * 0.18;
  col += u_col1 * exp(-abs(uv.y - base) * 60.0) * (0.25 + flash * 0.35);
  outColor = vec4(col, u_alpha);
}
`;

const SCOPE = `
void main() {
  vec2 uv = gl_FragCoord.xy / u_res;
  vec3 col = u_col3 * 0.10;
  float amp = texture(u_spectrum, vec2(uv.x * 0.9 + 0.02, 0.5)).r;
  for (int i = 0; i < 3; i++) {
    float fi = float(i);
    float y = 0.5 + (fi - 1.0) * 0.16
      + sin(uv.x * (6.0 + fi * 3.0) + u_time * (1.0 + fi * 0.6) + u_beats * 0.5) * 0.05
      + (amp - 0.35) * (0.22 + fi * 0.06);
    float d = abs(uv.y - y);
    float w = 0.5 + u_bass * (1.0 - fi * 0.3);
    vec3 c = i == 0 ? u_col1 : (i == 1 ? u_col2 : mix(u_col1, u_col2, 0.5));
    col += c * exp(-d * (140.0 - fi * 30.0)) * w;
    col += c * exp(-d * 18.0) * 0.10;
  }
  float shimmer = 0.5 + 0.5 * sin(uv.x * 200.0 + u_time * 30.0);
  col += u_col2 * u_treble * 0.12 * shimmer * exp(-abs(uv.y - 0.5) * 6.0);
  outColor = vec4(col, u_alpha);
}
`;

const METABALLS = `
float smin(float a, float b, float k) {
  float h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
  return mix(b, a, h) - k * h * (1.0 - h);
}
float map(vec3 p) {
  float t = u_time * 0.6;
  float r = 0.55 + u_bass * 0.25;
  float k = 0.35 + pow(1.0 - u_beat, 3.0) * 0.25;
  float d = length(p - vec3(sin(t) * r, cos(t * 1.3) * r * 0.6, sin(t * 0.7) * r)) - 0.45;
  d = smin(d, length(p - vec3(cos(t * 0.9) * r, sin(t * 1.1) * r, cos(t * 1.7) * r * 0.5)) - 0.38, k);
  d = smin(d, length(p - vec3(sin(t * 1.4 + 2.0) * r, cos(t * 0.8 + 1.0) * r, sin(t * 1.2 + 4.0) * r * 0.7)) - 0.33, k);
  d = smin(d, length(p) - (0.35 + u_bass * 0.12), k);
  return d;
}
void main() {
  vec2 uv = (gl_FragCoord.xy * 2.0 - u_res) / u_res.y;
  vec3 ro = vec3(0.0, 0.0, -2.6);
  vec3 rd = normalize(vec3(uv, 1.6));
  float t = 0.0;
  vec3 p = ro;
  bool hit = false;
  for (int i = 0; i < 48; i++) {
    p = ro + rd * t;
    float d = map(p);
    if (d < 0.002) { hit = true; break; }
    t += d;
    if (t > 6.0) break;
  }
  vec3 col = u_col3 * (0.15 + 0.10 * uv.y);
  if (hit) {
    vec2 e = vec2(0.004, 0.0);
    vec3 n = normalize(vec3(map(p + e.xyy) - map(p - e.xyy),
                            map(p + e.yxy) - map(p - e.yxy),
                            map(p + e.yyx) - map(p - e.yyx)));
    float diff = max(dot(n, normalize(vec3(0.6, 0.8, -0.5))), 0.0);
    float rim = pow(1.0 - max(dot(n, -rd), 0.0), 3.0);
    col = u_col3 * 0.25 + u_col1 * diff * 0.8 + u_col2 * rim * (0.6 + u_mid * 0.6);
    col *= exp(-t * 0.18);
  }
  col += u_col1 * pow(1.0 - u_beat, 5.0) * 0.05;
  outColor = vec4(col, u_alpha);
}
`;

const GRIDBOX = `
float boxd(vec3 p, vec3 b) {
  vec3 q = abs(p) - b;
  return length(max(q, 0.0)) + min(max(q.x, max(q.y, q.z)), 0.0);
}
float map(vec3 p) {
  vec2 cell = floor(p.xz / 1.2);
  vec2 lp = mod(p.xz, 1.2) - 0.6;
  float fx = fract(cell.x * 0.061 + 0.5);
  float amp = textureLod(u_spectrum, vec2(fx * 0.9 + 0.02, 0.5), 0.0).r;
  float h = 0.15 + amp * 1.2;
  return boxd(vec3(lp.x, p.y + 1.2, lp.y), vec3(0.42, h, 0.42));
}
void main() {
  vec2 uv = (gl_FragCoord.xy * 2.0 - u_res) / u_res.y;
  float fwd = u_time * 2.5 + u_level * 1.5;
  vec3 ro = vec3(0.0, 0.4 + sin(u_beats * 3.14159) * 0.03, fwd);
  vec3 rd = normalize(vec3(uv.x, uv.y - 0.25, 1.4));
  float t = 0.0;
  vec3 p = ro;
  bool hit = false;
  for (int i = 0; i < 56; i++) {
    p = ro + rd * t;
    float d = map(p);
    if (d < 0.004) { hit = true; break; }
    t += max(d, 0.015);
    if (t > 24.0) break;
  }
  float flash = pow(1.0 - u_beat, 4.0);
  vec3 fog = u_col3 * (0.18 + flash * 0.10);
  vec3 col = fog;
  if (hit) {
    vec2 e = vec2(0.01, 0.0);
    vec3 n = normalize(vec3(map(p + e.xyy) - map(p - e.xyy),
                            map(p + e.yxy) - map(p - e.yxy),
                            map(p + e.yyx) - map(p - e.yyx)));
    float diff = max(dot(n, normalize(vec3(0.4, 0.9, -0.3))), 0.0);
    float top = smoothstep(0.7, 1.0, n.y);
    vec3 m = mix(u_col1 * 0.7, u_col2, top);
    col = m * (0.25 + diff * 0.75);
    col = mix(col, fog, smoothstep(4.0, 22.0, t));
  }
  outColor = vec4(col, u_alpha);
}
`;

const AURORA = `
float hash21(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
float vnoise(vec2 p) {
  vec2 i = floor(p);
  vec2 f = fract(p);
  f = f * f * (3.0 - 2.0 * f);
  return mix(mix(hash21(i), hash21(i + vec2(1.0, 0.0)), f.x),
             mix(hash21(i + vec2(0.0, 1.0)), hash21(i + vec2(1.0, 1.0)), f.x), f.y);
}
float fbm(vec2 p) {
  float v = 0.0;
  float a = 0.5;
  for (int i = 0; i < 4; i++) { v += a * vnoise(p); p *= 2.03; a *= 0.5; }
  return v;
}
void main() {
  vec2 uv = gl_FragCoord.xy / u_res;
  float flow = u_time * 0.18 + u_mid * 0.4;
  vec3 col = mix(u_col3 * 0.08, u_col3 * 0.22, uv.y);
  for (int i = 0; i < 3; i++) {
    float fi = float(i);
    float band = fbm(vec2(uv.x * (2.0 + fi) + flow * (1.0 + fi * 0.4), fi * 7.0 + flow * 0.3));
    float center = 0.55 + fi * 0.12 + (band - 0.5) * 0.35;
    float d = abs(uv.y - center);
    float glow = exp(-d * (10.0 + fi * 6.0)) * (0.35 + u_mid * 0.5);
    vec3 c = i == 0 ? u_col1 : (i == 1 ? u_col2 : mix(u_col2, u_col1, 0.5));
    col += c * glow * band;
  }
  vec2 g = uv * vec2(90.0, 50.0);
  float h = hash21(floor(g));
  float star = step(0.97, h) * exp(-length(fract(g) - 0.5) * 8.0);
  col += mix(u_col1, u_col2, h) * star * (0.3 + u_treble * 0.5) * smoothstep(0.6, 0.2, uv.y);
  col += u_col1 * pow(1.0 - u_beat, 5.0) * 0.03;
  outColor = vec4(col, u_alpha);
}
`;

const SWARM = `
float hash11(float n) { return fract(sin(n * 127.1) * 43758.5453); }
void main() {
  vec2 uv = (gl_FragCoord.xy * 2.0 - u_res) / u_res.y;
  vec3 col = u_col3 * 0.10;
  float tighten = 0.85 - u_bass * 0.45;
  for (int i = 0; i < 64; i++) {
    float fi = float(i);
    float h1 = hash11(fi + 1.0);
    float h2 = hash11(fi + 57.0);
    float h3 = hash11(fi + 113.0);
    float dir = h3 > 0.5 ? 1.0 : -1.0;
    float ang = h1 * 6.2831853 + u_time * (0.15 + h2 * 0.5) * dir;
    float rad = (0.15 + h2 * 0.85) * tighten + u_treble * 0.15 * sin(u_time * 3.0 + fi);
    vec2 p = vec2(cos(ang), sin(ang) * 0.8) * rad;
    float d = length(uv - p);
    float size = 60.0 + h3 * 120.0;
    col += mix(u_col1, u_col2, h2) * exp(-d * d * size * 20.0) * (0.5 + h1 * 0.5);
  }
  float ring = abs(length(uv) - u_beat * 1.4);
  col += u_col2 * exp(-ring * 30.0) * pow(1.0 - u_beat, 2.0) * 0.5;
  outColor = vec4(col, u_alpha);
}
`;

export const SCENES: { name: string; frag: string }[] = [
  { name: "Tunnel", frag: TUNNEL },
  { name: "Halo", frag: HALO },
  { name: "Starfield", frag: STARFIELD },
  { name: "Terrain", frag: TERRAIN },
  { name: "Bars", frag: BARS },
  { name: "Scope", frag: SCOPE },
  { name: "Metaballs", frag: METABALLS },
  { name: "Gridbox", frag: GRIDBOX },
  { name: "Aurora", frag: AURORA },
  { name: "Swarm", frag: SWARM },
];
