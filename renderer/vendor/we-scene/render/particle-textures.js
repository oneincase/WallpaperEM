// [we-scene patch] WE 内置粒子贴图的程序化重建。
//
// 为什么需要这个文件：粒子材质引用的贴图（particle/halo、particle/drop、
// particle/fog/fog1 …）**不在 scene.pkg 里** —— 它们是 Wallpaper Engine 安装目录
// 自带的公共资源，作者的 pkg 只存自己上传的工坊贴图。全库实测 33 张被引用的粒子贴图
// 里 24 张属于内置资源，缺一张整个粒子系统就没东西可画。
//
// 没有 WE 安装目录可回退时，唯一可行的复刻路径是按名字语义程序化生成近似素材。
// 这些内置贴图都是结构极简的灰度/彩色素材（径向光晕、拉长的雨滴、柔和雾团、
// 光束梯度），用 canvas 2D 重建在观感上足够接近。
//
// 约定：所有生成函数返回 { width, height, rgba: Uint8Array }，直接喂 texImage2D。
// 素材一律「预乘前」的直通 RGBA，alpha 携带形状，rgb 为白（粒子颜色由顶点色调制）。

const cache = new Map()

function makeCanvas(w, h) {
  if (typeof document === 'undefined') return null
  const c = document.createElement('canvas')
  c.width = w
  c.height = h
  return c
}

function readback(canvas) {
  const ctx = canvas.getContext('2d')
  const id = ctx.getImageData(0, 0, canvas.width, canvas.height)
  return { width: canvas.width, height: canvas.height, rgba: new Uint8Array(id.data.buffer.slice(0)) }
}

// canvas 绘制的素材（气泡/花瓣/叶片/星）统一后处理，保证与纯算术素材同样安全：
//   1. 边缘羽化：贴图四边必须收敛到 alpha 0，否则精灵 quad 会露出一条硬直边
//      （实测 bubble3 的边缘 alpha 有 0.067，放大后就是可见的矩形边框）；
//   2. 能量归一：把平均 alpha 压到 targetAvg 附近。这些素材实心面积大，
//      canvas 版平均 alpha 可达 0.27–0.35，在 additive 场景里几层叠加就过曝，
//      而无 canvas 时的算术兜底只有 0.08 量级 —— 两条路径必须给出一致的观感。
function conditionTexture(tex, targetAvg) {
  const { width: W, height: H, rgba } = tex
  // 边缘羽化：距边界 feather 像素内按线性斜坡衰减到 0
  const feather = Math.max(2, Math.round(Math.min(W, H) * 0.04))
  for (let y = 0; y < H; y++) {
    for (let x = 0; x < W; x++) {
      const dEdge = Math.min(x, y, W - 1 - x, H - 1 - y)
      if (dEdge >= feather) continue
      const k = dEdge / feather
      const o = (y * W + x) * 4 + 3
      rgba[o] = Math.round(rgba[o] * k)
    }
  }
  if (targetAvg > 0) {
    let sum = 0
    const N = W * H
    for (let i = 0; i < N; i++) sum += rgba[i * 4 + 3]
    const avg = sum / N / 255
    if (avg > targetAvg && avg > 0) {
      const k = targetAvg / avg
      for (let i = 0; i < N; i++) rgba[i * 4 + 3] = Math.round(rgba[i * 4 + 3] * k)
    }
  }
  return tex
}

// ---------- 基元 ----------

// 径向渐变光晕：exp 控制衰减陡度（大 = 芯更紧、边缘更快消失），coreStop 之前为实心芯。
// 纯算术实现（不依赖 canvas）：这类素材是最常用的粒子贴图（halo_* 全库 60+ 次引用），
// 走 canvas 的话在无 document 环境会退化成同一张兜底图，浏览器与离线校验结果不一致。
function radialGlow(size, exp, coreStop) {
  const rgba = new Uint8Array(size * size * 4)
  const half = size / 2
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      // +0.5 取像素中心，避免中心点落在四像素缝隙上导致芯部偏移
      const dx = (x + 0.5 - half) / half
      const dy = (y + 0.5 - half) / half
      const d = Math.hypot(dx, dy)
      let a = d >= 1 ? 0 : Math.pow(1 - d, exp)
      if (coreStop > 0 && d < coreStop) a = 1
      const o = (y * size + x) * 4
      rgba[o] = 255
      rgba[o + 1] = 255
      rgba[o + 2] = 255
      rgba[o + 3] = Math.round(Math.min(1, Math.max(0, a)) * 255)
    }
  }
  return { width: size, height: size, rgba }
}

// 椭圆软斑：用于雨滴/花瓣/碎屑这类拉长的素材（rx/ry 为归一化半径）。
// 同样是纯算术实现，理由与 radialGlow 一致（雨滴是常用素材，需在两种环境下一致）。
function softEllipse(size, rx, ry, exp, rotate) {
  const rgba = new Uint8Array(size * size * 4)
  const half = size / 2
  const ca = Math.cos(rotate || 0)
  const sa = Math.sin(rotate || 0)
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const px = (x + 0.5 - half) / half
      const py = (y + 0.5 - half) / half
      // 逆旋转到椭圆自身坐标系，再按 rx/ry 归一化
      const ux = (px * ca + py * sa) / (rx || 1)
      const uy = (-px * sa + py * ca) / (ry || 1)
      const d = Math.hypot(ux, uy)
      const a = d >= 1 ? 0 : Math.pow(1 - d, exp)
      const o = (y * size + x) * 4
      rgba[o] = 255
      rgba[o + 1] = 255
      rgba[o + 2] = 255
      rgba[o + 3] = Math.round(Math.min(1, Math.max(0, a)) * 255)
    }
  }
  return { width: size, height: size, rgba }
}

// 非方形画布上的软椭圆：用于雨丝这类**贴图本身就是长条**的素材。
// 为什么不能用方图 + 让 rx 变窄：精灵 quad 会按贴图宽高比成形（见 particles.js 的
// texAspectX/Y），方图意味着 quad 是正方形。实测 sizerandom 可达 1600
// （1444077782 的 Rain downpour），方形 quad 让单个雨滴占到约 40% 屏宽，
// 几十个 additive 叠加直接把画面冲白；做成 64×256 的竖长条才是细雨丝。
// rx/ry 为归一化半径（相对各自半边长），exp 控制边缘衰减陡度。
function softEllipseRect(w, h, rx, ry, exp) {
  const rgba = new Uint8Array(w * h * 4)
  const hw = w / 2
  const hh = h / 2
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const ux = (x + 0.5 - hw) / hw / (rx || 1)
      const uy = (y + 0.5 - hh) / hh / (ry || 1)
      const d = Math.hypot(ux, uy)
      const a = d >= 1 ? 0 : Math.pow(1 - d, exp)
      const o = (y * w + x) * 4
      rgba[o] = 255
      rgba[o + 1] = 255
      rgba[o + 2] = 255
      rgba[o + 3] = Math.round(Math.min(1, Math.max(0, a)) * 255)
    }
  }
  return { width: w, height: h, rgba }
}

// 值噪声 + fbm（雾/烟用；WE 的 fog/smoke 贴图是低频噪声团乘径向衰减）
function hash2(x, y) {
  const n = Math.sin(x * 127.1 + y * 311.7) * 43758.5453
  return n - Math.floor(n)
}
function vnoise(x, y) {
  const ix = Math.floor(x)
  const iy = Math.floor(y)
  const fx = x - ix
  const fy = y - iy
  const sx = fx * fx * (3 - 2 * fx)
  const sy = fy * fy * (3 - 2 * fy)
  return (
    hash2(ix, iy) * (1 - sx) * (1 - sy) +
    hash2(ix + 1, iy) * sx * (1 - sy) +
    hash2(ix, iy + 1) * (1 - sx) * sy +
    hash2(ix + 1, iy + 1) * sx * sy
  )
}
function fbm2(x, y, oct) {
  let v = 0
  let amp = 0.5
  let f = 1
  let norm = 0
  for (let i = 0; i < oct; i++) {
    v += amp * vnoise(x * f, y * f)
    norm += amp
    amp *= 0.5
    f *= 2
  }
  return v / (norm || 1)
}

// 噪声云团：低频 fbm × 径向衰减，边缘干净收敛到 0
function noiseBlob(size, freq, oct, contrast, edgeExp) {
  const rgba = new Uint8Array(size * size * 4)
  const half = size / 2
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const nx = (x / size) * freq
      const ny = (y / size) * freq
      let n = fbm2(nx, ny, oct)
      n = Math.min(1, Math.max(0, (n - 0.5) * contrast + 0.5))
      const dx = (x - half) / half
      const dy = (y - half) / half
      const d = Math.min(1, Math.hypot(dx, dy))
      const falloff = Math.pow(Math.max(0, 1 - d), edgeExp)
      const a = Math.round(Math.min(1, n * falloff) * 255)
      const o = (y * size + x) * 4
      rgba[o] = 255
      rgba[o + 1] = 255
      rgba[o + 2] = 255
      rgba[o + 3] = a
    }
  }
  return { width: size, height: size, rgba }
}

// 光束/光轴：纵向长条，中间亮两端渐隐（WE 的 beam_* / light_shafts_* 都是这类）。
// fadeBoth=false 时只在一端渐隐（beam_2_fade 语义）。
//
// 亮度分布很关键：这类精灵在场景里被放大到几千像素、十几个叠加、且走 additive 混合。
// WE 的真实素材是「窄亮芯 + 大片近全透明」，平均 alpha 很低；若按普通高斯生成，
// 平均 alpha 会到 0.2–0.3，十几层 additive 叠加直接把画面冲成白屏（实测 snowflat
// 场景 26% 像素过曝）。故这里额外做两件事：
//   1. 横向用高次幂收窄（芯细、肩部快速掉到 0）；
//   2. 整体乘一个峰值系数，把平均 alpha 压到 ~0.05 量级。
function beam(w, h, coreWidth, fadeBoth) {
  const rgba = new Uint8Array(w * h * 4)
  for (let y = 0; y < h; y++) {
    const ty = y / (h - 1)
    // 纵向包络。fadeBoth=false（beam_2_fade）只朝一端渐隐，但**起始端仍须收敛到 0**：
    // 若起始行留着满 alpha，quad 上边就是一条硬直边，精灵看起来是个矩形而非光束。
    // 故单端渐隐用「短促起振 + 长距衰减」，两端都归零。
    const vy = fadeBoth
      ? Math.sin(ty * Math.PI)
      : Math.min(1, ty / 0.08) * (1 - ty)
    for (let x = 0; x < w; x++) {
      const tx = (x / (w - 1)) * 2 - 1 // -1..1
      // 横向：高斯再取 4 次幂 → 芯窄、边缘干净（避免宽肩部堆积能量）
      const g = Math.exp(-(tx * tx) / (2 * coreWidth * coreWidth))
      const vx = g * g * g * g
      // 纵向包络也提幂，缩短高亮段
      const a = Math.min(1, Math.max(0, vx * Math.pow(vy, 1.6) * 0.55))
      const o = (y * w + x) * 4
      rgba[o] = 255
      rgba[o + 1] = 255
      rgba[o + 2] = 255
      rgba[o + 3] = Math.round(a * 255)
    }
  }
  return { width: w, height: h, rgba }
}

// 环形波（particle/misc/wave）：圆环峰值，内外都衰减
function ring(size, radius, thickness) {
  const rgba = new Uint8Array(size * size * 4)
  const half = size / 2
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const d = Math.hypot((x - half) / half, (y - half) / half)
      const k = (d - radius) / thickness
      const a = Math.round(Math.exp(-k * k) * Math.max(0, 1 - d) * 255)
      const o = (y * size + x) * 4
      rgba[o] = 255
      rgba[o + 1] = 255
      rgba[o + 2] = 255
      rgba[o + 3] = a
    }
  }
  return { width: size, height: size, rgba }
}

// 气泡：亮环 + 弱内芯 + 高光点
function bubble(size) {
  const c = makeCanvas(size, size)
  if (!c) return ring(size, 0.78, 0.1)
  const ctx = c.getContext('2d')
  ctx.clearRect(0, 0, size, size)
  const r = size / 2
  // 外环
  const g = ctx.createRadialGradient(r, r, 0, r, r, r)
  g.addColorStop(0, 'rgba(255,255,255,0.06)')
  g.addColorStop(0.62, 'rgba(255,255,255,0.10)')
  g.addColorStop(0.86, 'rgba(255,255,255,0.85)')
  g.addColorStop(0.97, 'rgba(255,255,255,0.25)')
  g.addColorStop(1, 'rgba(255,255,255,0)')
  ctx.fillStyle = g
  ctx.beginPath()
  ctx.arc(r, r, r, 0, Math.PI * 2)
  ctx.fill()
  // 左上高光
  const hx = r * 0.68
  const hy = r * 0.62
  const hg = ctx.createRadialGradient(hx, hy, 0, hx, hy, r * 0.3)
  hg.addColorStop(0, 'rgba(255,255,255,0.9)')
  hg.addColorStop(1, 'rgba(255,255,255,0)')
  ctx.fillStyle = hg
  ctx.beginPath()
  ctx.arc(hx, hy, r * 0.3, 0, Math.PI * 2)
  ctx.fill()
  return conditionTexture(readback(c), 0.06)
}

// 花瓣（particle/nature/rosepetals）：WE 原图是 sprite sheet，这里生成 2x2 四帧
// 不同朝向的水滴形花瓣，配合 sequencemultiplier / randomframe 使用。
function petals(size) {
  const c = makeCanvas(size, size)
  if (!c) return softEllipse(size, 1, 0.55, 1.6, 0.5)
  const ctx = c.getContext('2d')
  ctx.clearRect(0, 0, size, size)
  const cell = size / 2
  const drawPetal = (cx, cy, s, rot, curl) => {
    ctx.save()
    ctx.translate(cx, cy)
    ctx.rotate(rot)
    const g = ctx.createLinearGradient(0, -s * 0.5, 0, s * 0.5)
    g.addColorStop(0, 'rgba(255,255,255,0.55)')
    g.addColorStop(0.45, 'rgba(255,255,255,1)')
    g.addColorStop(1, 'rgba(255,255,255,0.75)')
    ctx.fillStyle = g
    ctx.beginPath()
    // 水滴/花瓣轮廓：两段贝塞尔，curl 控制弯曲程度（模拟翻卷的花瓣）
    ctx.moveTo(0, -s * 0.5)
    ctx.bezierCurveTo(s * 0.52, -s * 0.34 + curl * s * 0.1, s * 0.44, s * 0.3, 0, s * 0.5)
    ctx.bezierCurveTo(-s * 0.44 + curl * s * 0.16, s * 0.3, -s * 0.52, -s * 0.34, 0, -s * 0.5)
    ctx.closePath()
    ctx.fill()
    ctx.restore()
  }
  const cfg = [
    [0.32, 0.0],
    [1.1, 0.35],
    [2.2, -0.3],
    [-0.7, 0.2],
  ]
  for (let i = 0; i < 4; i++) {
    const cx = (i % 2) * cell + cell / 2
    const cy = ((i / 2) | 0) * cell + cell / 2
    drawPetal(cx, cy, cell * 0.78, cfg[i][0], cfg[i][1])
  }
  return conditionTexture(readback(c), 0.12)
}

// 叶片（leaves*）：单帧细长叶形，带中脉暗线
function leaf(size) {
  const c = makeCanvas(size, size)
  if (!c) return softEllipse(size, 0.5, 1, 1.4, 0)
  const ctx = c.getContext('2d')
  ctx.clearRect(0, 0, size, size)
  const cx = size / 2
  const cy = size / 2
  const s = size * 0.86
  const g = ctx.createLinearGradient(cx, cy - s / 2, cx, cy + s / 2)
  g.addColorStop(0, 'rgba(255,255,255,0.6)')
  g.addColorStop(0.5, 'rgba(255,255,255,1)')
  g.addColorStop(1, 'rgba(255,255,255,0.55)')
  ctx.fillStyle = g
  ctx.beginPath()
  ctx.moveTo(cx, cy - s / 2)
  ctx.quadraticCurveTo(cx + s * 0.3, cy, cx, cy + s / 2)
  ctx.quadraticCurveTo(cx - s * 0.3, cy, cx, cy - s / 2)
  ctx.closePath()
  ctx.fill()
  // 中脉：略暗，让叶片在放大时不显得是纯色块
  ctx.strokeStyle = 'rgba(255,255,255,0.35)'
  ctx.lineWidth = Math.max(1, size / 64)
  ctx.beginPath()
  ctx.moveTo(cx, cy - s * 0.46)
  ctx.lineTo(cx, cy + s * 0.46)
  ctx.stroke()
  return conditionTexture(readback(c), 0.12)
}

// 五角星（Star_0x 类工坊贴图缺失时的兜底）
function star(size, points, innerRatio) {
  const c = makeCanvas(size, size)
  if (!c) return radialGlow(size, 3, 0)
  const ctx = c.getContext('2d')
  ctx.clearRect(0, 0, size, size)
  const cx = size / 2
  const cy = size / 2
  const R = size * 0.48
  ctx.fillStyle = 'rgba(255,255,255,1)'
  ctx.beginPath()
  for (let i = 0; i < points * 2; i++) {
    const a = (i / (points * 2)) * Math.PI * 2 - Math.PI / 2
    const r = i % 2 === 0 ? R : R * innerRatio
    const x = cx + Math.cos(a) * r
    const y = cy + Math.sin(a) * r
    if (i === 0) ctx.moveTo(x, y)
    else ctx.lineTo(x, y)
  }
  ctx.closePath()
  ctx.fill()
  // 叠一层柔光，避免尖角在小尺寸下锯齿明显
  const g = ctx.createRadialGradient(cx, cy, 0, cx, cy, R)
  g.addColorStop(0, 'rgba(255,255,255,0.7)')
  g.addColorStop(1, 'rgba(255,255,255,0)')
  ctx.globalCompositeOperation = 'lighter'
  ctx.fillStyle = g
  ctx.fillRect(0, 0, size, size)
  return conditionTexture(readback(c), 0.10)
}

// 平坦法线贴图（法线类贴图缺失时的中性回退：(0.5,0.5,1) = 不扰动）
function flatNormal(size) {
  const rgba = new Uint8Array(size * size * 4)
  for (let i = 0; i < size * size; i++) {
    rgba[i * 4] = 128
    rgba[i * 4 + 1] = 128
    rgba[i * 4 + 2] = 255
    rgba[i * 4 + 3] = 255
  }
  return { width: size, height: size, rgba }
}

// ---------- 名称 → 生成器映射 ----------
//
// 键为 WE 材质里的纹理名（去掉 materials/ 前缀与 .tex 后缀）。
// 未命中的名字走 fallbackFor() 的模糊匹配，最后兜底为通用光晕。

const BUILDERS = {
  // 光晕系：halo 是最常用的通用圆形粒子（火花/尘埃/萤火虫）。
  // 编号差异在 WE 里主要是芯的紧实度与边缘硬度，这里用衰减指数区分。
  'particle/halo': () => radialGlow(128, 3.0, 0),
  'particle/halo_1': () => radialGlow(128, 2.4, 0),
  'particle/halo_2': () => radialGlow(128, 4.5, 0),
  'particle/halo_3': () => radialGlow(128, 1.8, 0.06),
  'particle/halo_4': () => radialGlow(128, 6.0, 0),
  'particle/halo_5': () => radialGlow(128, 2.0, 0.12),
  'particle/halo_6': () => radialGlow(128, 3.6, 0.03),
  'particle/chromaticdot': () => radialGlow(64, 5.0, 0.1),
  'particle/light/flare_1': () => radialGlow(256, 2.2, 0),

  // 雨滴：贴图本身就是竖长条（64×256），精灵 quad 会按此比例成形 → 细雨丝
  'particle/drop': () => softEllipseRect(64, 256, 0.5, 1.0, 1.6),
  'particle/nature/rain1': () => softEllipseRect(64, 256, 0.42, 1.0, 1.7),
  'particle/nature/rain2': () => softEllipseRect(64, 256, 0.42, 1.0, 1.7),
  'particle/normal_splash': () => flatNormal(64),
  'particle/drop_normal': () => flatNormal(64),
  'particle/normal_ring_smooth': () => flatNormal(64),
  'particle/water/rain_drops_sheet': () => softEllipseRect(128, 256, 0.62, 1.0, 1.5),
  'particle/water/rain_drops_sheet_normal': () => flatNormal(256),

  // 雾/烟：低频噪声团
  'particle/fog/fog1': () => noiseBlob(256, 2.6, 4, 1.5, 1.5),
  'particle/fog/fog2': () => noiseBlob(256, 3.2, 4, 1.4, 1.7),
  'particle/fog/fog3': () => noiseBlob(256, 2.0, 5, 1.6, 1.3),
  'particle/smoke/smoke1': () => noiseBlob(256, 3.0, 5, 1.7, 1.6),
  'particle/smoke/smoke2': () => noiseBlob(256, 3.6, 5, 1.8, 1.5),

  // 光束 / 光轴：纵向长条渐变
  'particle/beam/beam_1': () => beam(64, 256, 0.34, true),
  'particle/beam/beam_2': () => beam(64, 256, 0.22, true),
  'particle/beam/beam_2_fade': () => beam(64, 256, 0.24, false),
  'particle/light/light_shafts_0': () => beam(64, 256, 0.42, true),
  'particle/light/light_shafts_1': () => beam(64, 256, 0.3, true),
  'particle/light/light_shafts_6': () => beam(64, 256, 0.2, true),

  // 自然物
  'particle/nature/rosepetals': () => petals(256),
  'particle/nature/leaves': () => leaf(128),
  'particle/debris/debris1': () => softEllipse(64, 0.7, 0.45, 2.2, 0.6),

  // 其他
  'particle/misc/wave': () => ring(256, 0.72, 0.13),
  'particle/bubbles/bubble1': () => bubble(128),
  'particle/bubbles/bubble2': () => bubble(128),
  'particle/bubbles/bubble3': () => bubble(128),
}

// 名字里的语义关键词 → 生成器（工坊自定义名与未列出的内置名都走这里）
function fallbackFor(name) {
  const n = String(name).toLowerCase()
  if (/normal/.test(n)) return () => flatNormal(64)
  if (/rosepetal|petal|sakura|blossom/.test(n)) return () => petals(256)
  if (/leaf|leaves|foliage/.test(n)) return () => leaf(128)
  if (/fog|cloud|mist|vapor/.test(n)) return () => noiseBlob(256, 2.6, 4, 1.5, 1.5)
  if (/smoke/.test(n)) return () => noiseBlob(256, 3.4, 5, 1.7, 1.6)
  if (/beam|shaft|ray|godray/.test(n)) return () => beam(64, 256, 0.3, true)
  if (/bubble/.test(n)) return () => bubble(128)
  if (/ring|wave/.test(n)) return () => ring(256, 0.72, 0.13)
  if (/star|sparkle|snowflake|雪花|星/.test(n)) return () => star(128, 5, 0.42)
  if (/drop|rain/.test(n)) return () => softEllipseRect(64, 256, 0.45, 1.0, 1.7)
  if (/debris|dirt|rock/.test(n)) return () => softEllipse(64, 0.7, 0.45, 2.2, 0.6)
  if (/note|music/.test(n)) return () => star(128, 4, 0.5)
  if (/trail|streak/.test(n)) return () => beam(64, 128, 0.4, true)
  // 兜底：通用柔和光晕。绝大多数 halo_* / 未知圆形粒子都能用它顶上。
  return () => radialGlow(128, 3.0, 0)
}

// 生成（带缓存）内置粒子贴图的像素数据。
// 返回 { width, height, rgba } 或 null（无 document 环境）。
export function buildBuiltinParticleTexture(name) {
  if (cache.has(name)) return cache.get(name)
  const builder = BUILDERS[name] || fallbackFor(name)
  let out = null
  try {
    out = builder()
  } catch (e) {
    out = null
  }
  // 生成器依赖 canvas 时可能返回 null，退化为纯程序化的光晕（不依赖 document）
  if (!out) out = proceduralGlowFallback(128, 3.0)
  cache.set(name, out)
  return out
}

// 不依赖 canvas 的光晕（headless / canvas 不可用时）
function proceduralGlowFallback(size, exp) {
  const rgba = new Uint8Array(size * size * 4)
  const half = size / 2
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const d = Math.min(1, Math.hypot((x - half) / half, (y - half) / half))
      const a = Math.round(Math.pow(Math.max(0, 1 - d), exp) * 255)
      const o = (y * size + x) * 4
      rgba[o] = 255
      rgba[o + 1] = 255
      rgba[o + 2] = 255
      rgba[o + 3] = a
    }
  }
  return { width: size, height: size, rgba }
}

// 该名字是否是「WE 内置」粒子贴图（用于区分工坊贴图缺失 vs 内置缺失，仅诊断用）
export function isBuiltinParticleTextureName(name) {
  return typeof name === 'string' && name.indexOf('particle/') === 0
}
