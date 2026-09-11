/*
 * WallpaperEM 网页壁纸模板
 *
 * 这是「最小但完整的」网页壁纸示例，展示了壁纸可以拿到哪些宿主数据：
 *   1) window.wallpaperPropertyListener.applyUserProperties —— project.json
 *      general.properties 里的用户属性（缩放面板里改值实时回调）
 *   2) window.wallpaperRegisterAudioListener —— 系统音频频谱（左右各 64 段，0..1）
 *   3) 普通 DOM/Canvas/WebGL 随便用，页面就在壁纸窗口里铺满显示
 *
 * 改这个文件即可做自己的动态壁纸；不需要的就删掉，别留下的坑：
 *   - 不要用 localStorage（壁纸页可能被反复重载）
 *   - 不要请求网络（离线环境拿不到数据，页面会卡在等待）
 */
(() => {
  "use strict";

  const canvas = document.getElementById("stage");
  const ctx = canvas.getContext("2d");
  const clockEl = document.getElementById("clock");

  // wallpaperPropertyListener 的初值：用 project.json 里的默认值，
  // 避免宿主还没回调时先用错颜色画一帧
  const params = {
    schemecolor: "0.28 0.62 1",
    speed: 0.5,
    mode: "aurora",
    showclock: true,
  };

  /** "r g b"（0..1 浮点）→ css rgb() */
  function colorOf(v, alpha) {
    const p = String(v).trim().split(/\s+/).map(Number);
    const r = Math.round((p[0] || 0) * 255);
    const g = Math.round((p[1] || 0) * 255);
    const b = Math.round((p[2] || 0) * 255);
    return alpha === undefined ? `rgb(${r},${g},${b})` : `rgba(${r},${g},${b},${alpha})`;
  }

  // ---- 宿主属性回调（WE 兼容接口） ----
  window.wallpaperPropertyListener = {
    applyUserProperties(props) {
      if (!props) return;
      if (props.schemecolor) params.schemecolor = props.schemecolor.value;
      if (props.speed) params.speed = Number(props.speed.value);
      if (props.mode) params.mode = props.mode.value;
      if (props.showclock) params.showclock = !!props.showclock.value;
      syncClock();
    },
  };

  // ---- 音频频谱（可选；没有音频捕获时数组恒为 0） ----
  let audio = new Array(128).fill(0);
  const HALO = new Array(64).fill(0);
  if (typeof window.wallpaperRegisterAudioListener === "function") {
    window.wallpaperRegisterAudioListener((data) => {
      if (data && data.length) audio = data;
    });
  }

  function syncClock() {
    if (!clockEl) return;
    clockEl.hidden = !params.showclock;
  }
  syncClock();

  // ---- 尺寸：跟随窗口（壁纸窗口会随显示器分辨率/缩放变化） ----
  let w = 0;
  let h = 0;
  function resize() {
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    w = canvas.clientWidth;
    h = canvas.clientHeight;
    canvas.width = Math.max(1, Math.round(w * dpr));
    canvas.height = Math.max(1, Math.round(h * dpr));
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }
  window.addEventListener("resize", resize);
  resize();

  // ---- 画面：两种形态共用一套「背景 + 光斑」绘制 ----
  function drawBackground(t) {
    const g = ctx.createLinearGradient(0, 0, 0, h);
    g.addColorStop(0, "#05070f");
    g.addColorStop(0.55, "#0a1020");
    g.addColorStop(1, "#070b16");
    ctx.fillStyle = g;
    ctx.fillRect(0, 0, w, h);

    // 缓慢漂移的极光光带（schemecolor 决定主色）
    ctx.globalCompositeOperation = "lighter";
    for (let i = 0; i < 3; i++) {
      const phase = t * (0.05 + i * 0.017) * (0.4 + params.speed);
      const x = w * (0.5 + 0.38 * Math.sin(phase + i * 2.1));
      const y = h * (0.62 - 0.18 * i + 0.05 * Math.cos(phase * 1.7));
      const r = Math.max(w, h) * (0.32 + 0.06 * Math.sin(phase * 1.3));
      const blob = ctx.createRadialGradient(x, y, 0, x, y, r);
      blob.addColorStop(0, colorOf(params.schemecolor, 0.22 - i * 0.05));
      blob.addColorStop(0.6, colorOf(params.schemecolor, 0.07 - i * 0.02));
      blob.addColorStop(1, "rgba(0,0,0,0)");
      ctx.fillStyle = blob;
      ctx.fillRect(0, 0, w, h);
    }
    ctx.globalCompositeOperation = "source-over";
  }

  function drawRipples() {
    // 音频 → 圆环半径；无音频时也用时间驱动，保证画面在动
    const cx = w * 0.5;
    const cy = h * 0.52;
    ctx.globalCompositeOperation = "lighter";
    for (let i = 0; i < 64; i++) {
      const v = audio[i] || 0;
      const a = Math.max(v, HALO[i] * 0.86);
      HALO[i] = a;
      if (a < 0.02) continue;
      const r = (i / 64) * Math.min(w, h) * 0.7;
      ctx.beginPath();
      ctx.arc(cx, cy, r + a * 40, 0, Math.PI * 2);
      ctx.strokeStyle = colorOf(params.schemecolor, Math.min(0.5, a * 0.6));
      ctx.lineWidth = 1 + a * 3;
      ctx.stroke();
    }
    ctx.globalCompositeOperation = "source-over";
  }

  function drawAudioBars() {
    if (params.mode !== "aurora") return;
    const bars = 64;
    const bw = w / bars;
    for (let i = 0; i < bars; i++) {
      const v = audio[i] || 0;
      const bh = v * h * 0.16;
      if (bh < 1) continue;
      ctx.fillStyle = colorOf(params.schemecolor, 0.16 + v * 0.3);
      ctx.fillRect(i * bw, h - bh, Math.max(1, bw * 0.72), bh);
    }
  }

  let lastClock = "";
  function tickClock() {
    if (!params.showclock) return;
    const d = new Date();
    const s = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
    if (s !== lastClock) {
      lastClock = s;
      if (clockEl) clockEl.textContent = s;
    }
  }

  const start = performance.now();
  function frame(now) {
    const t = (now - start) / 1000;
    drawBackground(t);
    if (params.mode === "ripple") drawRipples();
    else drawAudioBars();
    tickClock();
    requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);
})();
