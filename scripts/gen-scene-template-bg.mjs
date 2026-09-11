// 生成 scene 模板的背景贴图 src-tauri/templates/scene/materials/bg.png
//
// 为什么用脚本生成而不是塞一张现成图：模板要进 Git，二进制体积与来源都得可控。
// 这里手写 PNG 编码（zlib deflate + CRC32），零依赖、可复现：
//   node scripts/gen-scene-template-bg.mjs
//
// 画面：深夜渐变底 + 极光光带 + 稀疏星点。是"占位但好看"的默认背景，
// agent 生成自己的工程时应整张替换掉（见 docs/mcp-authoring-scene.md）。

import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";

const W = 1280;
const H = 720;
const OUT = path.resolve(
  path.dirname(new URL(import.meta.url).pathname),
  "../src-tauri/templates/scene/materials/bg.png",
);

// ---- 确定性噪声（无随机种子依赖，重跑产物逐字节一致） ----
function hash2(x, y) {
  const n = Math.sin(x * 127.1 + y * 311.7) * 43758.5453123;
  return n - Math.floor(n);
}
function valueNoise(x, y) {
  const xi = Math.floor(x);
  const yi = Math.floor(y);
  const xf = x - xi;
  const yf = y - yi;
  const u = xf * xf * (3 - 2 * xf);
  const v = yf * yf * (3 - 2 * yf);
  const a = hash2(xi, yi);
  const b = hash2(xi + 1, yi);
  const c = hash2(xi, yi + 1);
  const d = hash2(xi + 1, yi + 1);
  return a * (1 - u) * (1 - v) + b * u * (1 - v) + c * (1 - u) * v + d * u * v;
}
function fbm(x, y) {
  let sum = 0;
  let amp = 0.5;
  let f = 1;
  for (let i = 0; i < 5; i++) {
    sum += amp * valueNoise(x * f, y * f);
    f *= 2.03;
    amp *= 0.5;
  }
  return sum;
}
const smoothstep = (a, b, x) => {
  const t = Math.min(1, Math.max(0, (x - a) / (b - a)));
  return t * t * (3 - 2 * t);
};
const mix = (a, b, t) => a + (b - a) * t;

const raw = Buffer.alloc(W * H * 3);
for (let y = 0; y < H; y++) {
  const v = y / (H - 1);
  for (let x = 0; x < W; x++) {
    const u = x / (W - 1);
    // 底色：深靛蓝 → 近黑
    let r = mix(7, 4, v);
    let g = mix(11, 7, v);
    let b = mix(30, 20, v);

    // 极光光带：低频 fbm 沿 y 衰减，两条错开的绿色/紫色带
    const band = fbm(u * 2.2, v * 3.4 + 0.7);
    const bandShape = Math.exp(-Math.pow((v - 0.30 - band * 0.16) * 5.6, 2));
    const green = bandShape * 0.85;
    const purpleBand = fbm(u * 1.7 + 5.3, v * 2.9 + 2.1);
    const purpleShape = Math.exp(-Math.pow((v - 0.60 - purpleBand * 0.12) * 6.4, 2));
    const purple = purpleShape * 0.55;

    r += green * 34 + purple * 62;
    g += green * 128 + purple * 30;
    b += green * 96 + purple * 132;

    // 地平线辉光（下方偏冷）
    const horizon = smoothstep(0.55, 1.05, v);
    r += horizon * 6;
    g += horizon * 16;
    b += horizon * 30;

    // 星点：高频噪声阈值化，只在画面上半部可见
    const star = hash2(Math.floor(x * 1.7) + 0.5, Math.floor(y * 1.7) + 9.5);
    const starMask = star > 0.9986 ? (1 - v * 0.6) * (star - 0.9986) * 1400 : 0;
    r += starMask;
    g += starMask;
    b += starMask * 1.05;

    const i = (y * W + x) * 3;
    raw[i] = Math.max(0, Math.min(255, Math.round(r)));
    raw[i + 1] = Math.max(0, Math.min(255, Math.round(g)));
    raw[i + 2] = Math.max(0, Math.min(255, Math.round(b)));
  }
}

// ---- PNG 编码（filter type 0 逐行） ----
const CRC_TABLE = (() => {
  const t = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c;
  }
  return t;
})();
function crc32(buf) {
  let c = -1;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([len, body, crc]);
}
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(W, 0);
ihdr.writeUInt32BE(H, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 2; // truecolor
const scanlines = Buffer.alloc(H * (1 + W * 3));
for (let y = 0; y < H; y++) {
  scanlines[y * (1 + W * 3)] = 0;
  raw.copy(scanlines, y * (1 + W * 3) + 1, y * W * 3, (y + 1) * W * 3);
}
const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", zlib.deflateSync(scanlines, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);

fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, png);
console.log(`wrote ${OUT} (${W}x${H}, ${png.length} bytes)`);
