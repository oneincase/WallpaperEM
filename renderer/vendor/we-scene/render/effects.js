// WE 效果链的纯计算实现（CPU 光栅器与数值验证共用）。
// 逐行翻译自 WE shader 原文，约定见 docs/WE_RENDER_CONVENTIONS.md。
// 空间：显示空间 v-down（v=0 = 图层画面顶部）；效果输入一律采样原始 uv (u0, v0)；
// 位移只累加到采样坐标；waterflow/颜色类在采样后处理。

export const M_2PI = 6.28318530718

const WHITE = { width: 1, height: 1, rgba: new Uint8Array([255, 255, 255, 255]), rg88: false }

// ---------- 采样 ----------

// 双线性采样（与 WE 纹理线性过滤一致；高频纹理如噪声必须双线性）
export function sampleTex(tex, u, v) {
  return sampleRgba(tex.rgba, tex.width, tex.height, u, v)
}

// 带 mip 的采样（WE .tex 带 mip 链 + LINEAR_MIPMAP_LINEAR：缩小采样自动平滑）
// level 由调用方按缩放因子给出（近似三线性：级别内双线性）
export function sampleTexLod(tex, u, v, level = 0) {
  if (tex.mips && tex.mips.length > 1) {
    const lv = Math.max(0, Math.min(tex.mips.length - 1, level))
    const m = tex.mips[lv]
    return sampleRgba(m.rgba, m.width, m.height, u, v)
  }
  return sampleTex(tex, u, v)
}

// GL_LINEAR_MIPMAP_LINEAR 的级别估算：采样区域 ≈ texW*scale 像素 → level = -log2(scale)
export function mipLevelForScale(scale, texWidth) {
  if (texWidth <= 1 || scale >= 1) return 0
  return Math.max(0, Math.round(-Math.log2(scale)))
}

function sampleRgba(rgba, w, h, u, v) {
  const x = u * w - 0.5
  const y = v * h - 0.5
  const x0 = Math.floor(x)
  const y0 = Math.floor(y)
  const fx = x - x0
  const fy = y - y0
  const cx = (xi) => (xi < 0 ? 0 : xi >= w ? w - 1 : xi)
  const cy = (yi) => (yi < 0 ? 0 : yi >= h ? h - 1 : yi)
  const o00 = (cy(y0) * w + cx(x0)) * 4
  const o10 = (cy(y0) * w + cx(x0 + 1)) * 4
  const o01 = (cy(y0 + 1) * w + cx(x0)) * 4
  const o11 = (cy(y0 + 1) * w + cx(x0 + 1)) * 4
  const out = [0, 0, 0, 0]
  for (let c = 0; c < 4; c++) {
    const a = rgba[o00 + c] * (1 - fx) + rgba[o10 + c] * fx
    const b = rgba[o01 + c] * (1 - fx) + rgba[o11 + c] * fx
    out[c] = a * (1 - fy) + b * fy
  }
  return out
}

// 流向通道：RG88 → (原始R, 原始G)；普通纹理 → (r, g)
export function flowChannels(tex, f) {
  return tex.rg88 ? [f[3] / 255, f[0] / 255] : [f[0] / 255, f[1] / 255]
}

// 不透明蒙版通道：RG88 → 原始R；普通 → r
export function maskChannel(tex, f) {
  return tex.rg88 ? f[3] / 255 : f[0] / 255
}

// ---------- 位移类效果（采样坐标） ----------

export function rotate2(v, a) {
  const c = Math.cos(a)
  const s = Math.sin(a)
  return [v[0] * c - v[1] * s, v[0] * s + v[1] * c]
}

export function frac(x) {
  return x - Math.floor(x)
}

// items: 效果参数列表（见 parse 代码）。返回最终采样坐标 { su, sv }。
// scroll 替换坐标：frac((uv0 + scroll) * repeat)（scroll.vert:18-20 / scroll.frag:10）
// 其余位移累加；各效果输入（蒙版/噪声）均采样原始 uv（WE 各 pass 四边形 uv 恒为原始 uv）。
export function applyDisplacements(items, u0, v0, tex, textures, time) {
  let su = u0
  let sv = v0
  for (const it of items) {
    if (it.type === 'scroll') {
      const ox = Math.sign(it.sx) * it.sx * it.sx * time
      const oy = Math.sign(it.sy) * it.sy * it.sy * time
      su = frac((u0 + ox) * it.rx)
      sv = frac((v0 + oy) * it.ry)
    } else if (it.type === 'shake') {
      // shake.frag:28-79
      const phaseTex = textures.get(it.phase) || WHITE
      const pf = sampleTex(phaseTex, u0, v0)
      const flowPhase = (pf[0] / 255) * M_2PI // .r
      const m = textures.get(it.flow) || WHITE
      const f = sampleTex(m, u0, v0)
      const flow = flowChannels(m, f)
      const flowMask = [(flow[0] - 0.498) * 2, (flow[1] - 0.498) * 2]
      const t2 = it.speed * time + flowPhase
      // M_PI_2 = 2π：sin(frac(t/2π)·2π) = sin(t mod 2π)，平滑正弦
      let off = Math.sin(frac(t2 / M_2PI) * M_2PI)
      off = off * 0.498 + 0.5
      const base = Math.cos(t2) >= 0 ? 1 : 0
      off = base === 1 ? Math.pow(off, it.fy) : 1 - Math.pow(1 - off, it.fx)
      off = Math.min(1, Math.max(0, (off - it.bounds[0]) * (1 / (it.bounds[1] - it.bounds[0]))))
      if (it.direction === 0) off = off * 2 - 1
      else if (it.direction === 2) off = off - 1
      // direction === 1：保持 0..1
      const amp2 = it.amp * it.amp
      su += off * amp2 * flowMask[0]
      sv += off * amp2 * flowMask[1]
    } else if (it.type === 'waves') {
      // waterwaves.frag:15-23
      const m = textures.get(it.mask) || WHITE
      const f = sampleTex(m, u0, v0)
      const mask = maskChannel(m, f)
      const dir = rotate2([0, 1], it.direction) // (-sin a, cos a)
      const pos = Math.abs((u0 - 0.5) * dir[0] + (v0 - 0.5) * dir[1])
      const dist = time * it.speed + (u0 * dir[0] + v0 * dir[1]) * (it.scale + it.perspective * pos)
      const s = Math.sin(dist) * (it.strength * it.strength + it.perspective * pos) * mask
      su += dir[1] * s
      sv += -dir[0] * s // shader: offset = (dir.y, -dir.x)
    } else if (it.type === 'sway') {
      // foliagesway.vert:44-50 + frag:24-46
      const noise = textures.get(it.noise) || WHITE
      // WE 的 noise 纹理带 mip 链（LINEAR_MIPMAP_LINEAR）：noiseScale<1 时采低 mip = 平滑位移场
      const n = sampleTexLod(noise, u0 * it.noiseScale, v0 * it.noiseScale, mipLevelForScale(it.noiseScale, noise.width))
      const aspect = (tex.width / tex.height) * it.ratio
      const zw = rotate2([1 / aspect, aspect], it.direction)
      const params = rotate2([u0, v0], it.direction)
      let amp = it.strength * it.strength * 0.005 // v_Params.z
      if (it.masked && it.mask) {
        const swayMask = textures.get(it.mask)
        if (swayMask) amp *= maskChannel(swayMask, sampleTex(swayMask, u0, v0))
      }
      const phase = (n[1] / 255 * M_2PI + params[0] * 10 + params[1] * 5) * it.phase
      const ks = [1, -0.16161616, 0.0083333, -0.00019841]
      const kc = [-0.5, 0.041666666, -0.0013888889, 0.000024801587]
      let sumA = 0
      let sumC = 0
      for (let i = 0; i < 4; i++) {
        let x = Math.sin(phase + it.speed * time * ks[i])
        sumA += Math.pow(Math.abs(x), it.power) * Math.sign(x)
        x = Math.sin(0.4 + phase + it.speed * time * kc[i])
        sumC += Math.pow(Math.abs(x), it.power) * Math.sign(x)
      }
      su += zw[0] * sumA * amp
      sv += zw[1] * sumC * amp
    }
  }
  return { su, sv }
}

// shake 的 MASK==1 路径（shake.frag:81-85）：mix(原图, 位移后, mask@位移后uv)。
// 仅在 shake 是该层唯一位移效果时严格等价；当前 6 个场景无 MASK==1 shake，保留公式完整性。
export function applyShakeMasks(items, u0, v0, su, sv, tex, textures, time, t) {
  for (const it of items) {
    if (it.type !== 'shake' || !it.masked) continue
    const maskTex = textures.get(it.mask) || WHITE
    const m = textures.get(it.flow) || WHITE
    const f = sampleTex(m, u0, v0)
    const flow = flowChannels(m, f)
    const flowMask = [(flow[0] - 0.498) * 2, (flow[1] - 0.498) * 2]
    // 位移量（与 applyDisplacements 中同式）
    const phaseTex = textures.get(it.phase) || WHITE
    const pf = sampleTex(phaseTex, u0, v0)
    const flowPhase = (pf[0] / 255) * M_2PI
    const t2 = it.speed * time + flowPhase
    let off = Math.sin(frac(t2 / M_2PI) * M_2PI) * 0.498 + 0.5
    const base = Math.cos(t2) >= 0 ? 1 : 0
    off = base === 1 ? Math.pow(off, it.fy) : 1 - Math.pow(1 - off, it.fx)
    off = Math.min(1, Math.max(0, (off - it.bounds[0]) * (1 / (it.bounds[1] - it.bounds[0]))))
    if (it.direction === 0) off = off * 2 - 1
    else if (it.direction === 2) off = off - 1
    const amp2 = it.amp * it.amp
    const mx = u0 + off * amp2 * flowMask[0]
    const my = v0 + off * amp2 * flowMask[1]
    const maskVal = maskChannel(maskTex, sampleTex(maskTex, mx, my))
    const orig = sampleTex(tex, u0, v0)
    for (let c = 0; c < 4; c++) t[c] = orig[c] * (1 - maskVal) + t[c] * maskVal
  }
  return t
}

// ---------- waterflow（waterflow.frag:16-45，全量字面翻译） ----------

export function applyFlowMix(items, u0, v0, su, sv, tex, textures, time, t) {
  for (const it of items) {
    if (it.type !== 'flow') continue
    const phaseTex = textures.get(it.phase) || WHITE
    const pf = sampleTex(phaseTex, u0 * it.phaseScale, v0 * it.phaseScale)
    const flowPhase = pf[0] / 255
    const m = textures.get(it.flow) || WHITE
    const f = sampleTex(m, u0, v0)
    const flow = flowChannels(m, f)
    const flowMask = [(flow[0] - 0.498) * 2, (flow[1] - 0.498) * 2]
    const amount = Math.hypot(flowMask[0], flowMask[1]) // 不截断：mix 允许外插
    const amp = it.strength * 0.1
    const cx = frac(time * it.speed)
    const cy = frac(time * it.speed + 0.5)
    const cz = frac(0.25 + time * it.speed)
    const cw = frac(0.25 + time * it.speed + 0.5)
    const cycles = [cx - 0.5, cy - 0.5, cz - 0.5, cw - 0.5]
    const blend = 2 * Math.abs(cx - 0.5)
    const blend2 = 2 * Math.abs(cz - 0.5)
    const oa = [flowMask[0] * amp * cycles[0], flowMask[1] * amp * cycles[0]]
    const ob = [flowMask[0] * amp * cycles[1], flowMask[1] * amp * cycles[1]]
    const oc = [flowMask[0] * amp * cycles[2], flowMask[1] * amp * cycles[2]]
    const od = [flowMask[0] * amp * cycles[3], flowMask[1] * amp * cycles[3]]
    const albedo = sampleTex(tex, su, sv)
    const fa = sampleTex(tex, su + oa[0], sv + oa[1])
    const fb = sampleTex(tex, su + ob[0], sv + ob[1])
    const fc = sampleTex(tex, su + oc[0], sv + oc[1])
    const fd = sampleTex(tex, su + od[0], sv + od[1])
    const flowA = mix4(fa, fb, blend)
    const flowB = mix4(fc, fd, blend2)
    const flowOut = mix4(flowA, flowB, smoothstep(0.2, 0.8, flowPhase))
    t = mix4(albedo, flowOut, amount)
  }
  return t
}

export function mix4(a, b, k) {
  return [a[0] * (1 - k) + b[0] * k, a[1] * (1 - k) + b[1] * k, a[2] * (1 - k) + b[2] * k, a[3] * (1 - k) + b[3] * k]
}

export function smoothstep(e0, e1, x) {
  const k = Math.min(1, Math.max(0, (x - e0) / (e1 - e0)))
  return k * k * (3 - 2 * k)
}

// ---------- 颜色类效果（按 effect 列表顺序作用） ----------

// WE 混合模式（common_blending.h），逐字实现
export function applyBlending(mode, A, B, opacity) {
  const per = (fn) => mix3(A, fn(A, B), opacity)
  switch (mode) {
    case 1: return per((a, b) => [Math.min(b[0], a[0]), Math.min(b[1], a[1]), Math.min(b[2], a[2])]) // Darken
    case 2: return per((a, b) => [a[0] * b[0], a[1] * b[1], a[2] * b[2]]) // Multiply
    case 3: return per((a, b) => [b[0] === 0 ? 0 : Math.max(1 - (1 - a[0]) / b[0], 0), b[1] === 0 ? 0 : Math.max(1 - (1 - a[1]) / b[1], 0), b[2] === 0 ? 0 : Math.max(1 - (1 - a[2]) / b[2], 0)]) // ColorBurn
    case 4: return per((a, b) => [Math.max(a[0] + b[0] - 1, 0), Math.max(a[1] + b[1] - 1, 0), Math.max(a[2] + b[2] - 1, 0)]) // Substract
    case 5: return [Math.min(A[0], B[0]), Math.min(A[1], B[1]), Math.min(A[2], B[2])]
    case 6: return per((a, b) => [Math.max(b[0], a[0]), Math.max(b[1], a[1]), Math.max(b[2], a[2])]) // Lighten
    case 7: return per((a, b) => [1 - (1 - a[0]) * (1 - b[0]), 1 - (1 - a[1]) * (1 - b[1]), 1 - (1 - a[2]) * (1 - b[2])]) // Screen
    case 8: return per((a, b) => [b[0] === 1 ? 1 : Math.min(a[0] / (1 - b[0]), 1), b[1] === 1 ? 1 : Math.min(a[1] / (1 - b[1]), 1), b[2] === 1 ? 1 : Math.min(a[2] / (1 - b[2]), 1)]) // ColorDodge
    case 9: return per((a, b) => [Math.min(a[0] + b[0], 1), Math.min(a[1] + b[1], 1), Math.min(a[2] + b[2], 1)]) // Add
    case 10: return [Math.max(A[0], B[0]), Math.max(A[1], B[1]), Math.max(A[2], B[2])]
    case 11: return per((a, b) => overlay3(a, b)) // Overlay
    case 12: return per((a, b) => softLight3(a, b)) // SoftLight
    case 13: return per((a, b) => overlay3(b, a)) // HardLight
    case 14: return per((a, b) => [vivid(a[0], b[0]), vivid(a[1], b[1]), vivid(a[2], b[2])]) // VividLight
    case 15: return per((a, b) => [b[0] < 0.5 ? Math.max(a[0] + 2 * b[0] - 1, 0) : Math.min(a[0] + 2 * (b[0] - 0.5), 1), b[1] < 0.5 ? Math.max(a[1] + 2 * b[1] - 1, 0) : Math.min(a[1] + 2 * (b[1] - 0.5), 1), b[2] < 0.5 ? Math.max(a[2] + 2 * b[2] - 1, 0) : Math.min(a[2] + 2 * (b[2] - 0.5), 1)]) // LinearLight
    case 16: return per((a, b) => [b[0] < 0.5 ? Math.min(a[0], 2 * b[0]) : Math.max(a[0], 2 * (b[0] - 0.5)), b[1] < 0.5 ? Math.min(a[1], 2 * b[1]) : Math.max(a[1], 2 * (b[1] - 0.5)), b[2] < 0.5 ? Math.min(a[2], 2 * b[2]) : Math.max(a[2], 2 * (b[2] - 0.5))]) // PinLight
    case 17: return per((a, b) => [vivid(a[0], b[0]) < 0.5 ? 0 : 1, vivid(a[1], b[1]) < 0.5 ? 0 : 1, vivid(a[2], b[2]) < 0.5 ? 0 : 1]) // HardMix
    case 18: return per((a, b) => [Math.abs(a[0] - b[0]), Math.abs(a[1] - b[1]), Math.abs(a[2] - b[2])]) // Difference
    case 19: return per((a, b) => [a[0] + b[0] - 2 * a[0] * b[0], a[1] + b[1] - 2 * a[1] * b[1], a[2] + b[2] - 2 * a[2] * b[2]]) // Exclusion
    case 20: return per((a, b) => [Math.max(a[0] + b[0] - 1, 0), Math.max(a[1] + b[1] - 1, 0), Math.max(a[2] + b[2] - 1, 0)]) // Substract(同4)
    case 21: return per((a, b) => [b[0] === 1 ? 1 : Math.min(a[0] * a[0] / (1 - b[0]), 1), b[1] === 1 ? 1 : Math.min(a[1] * a[1] / (1 - b[1]), 1), b[2] === 1 ? 1 : Math.min(a[2] * a[2] / (1 - b[2]), 1)]) // Reflect
    case 22: return per((a, b) => [a[0] === 1 ? 1 : Math.min(b[0] * b[0] / (1 - a[0]), 1), a[1] === 1 ? 1 : Math.min(b[1] * b[1] / (1 - a[1]), 1), a[2] === 1 ? 1 : Math.min(b[2] * b[2] / (1 - a[2]), 1)]) // Glow = Reflect(B,A)
    case 23: return per((a, b) => [Math.min(a[0], b[0]) - Math.max(a[0], b[0]) + 1, Math.min(a[1], b[1]) - Math.max(a[1], b[1]) + 1, Math.min(a[2], b[2]) - Math.max(a[2], b[2]) + 1]) // Phoenix
    case 24: return per((a, b) => [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2, (a[2] + b[2]) / 2]) // Average
    case 25: return per((a, b) => [1 - Math.abs(1 - a[0] - b[0]), 1 - Math.abs(1 - a[1] - b[1]), 1 - Math.abs(1 - a[2] - b[2])]) // Negation
    case 26: return per((a, b) => hueBlend(a, b)) // Hue
    case 27: return per((a, b) => satBlend(a, b)) // Saturation
    case 28: return per((a, b) => colorBlend(a, b)) // Color
    case 29: return per((a, b) => lumBlend(a, b)) // Luminosity
    case 30: { // Tint = max(A)*B
      const m = Math.max(A[0], Math.max(A[1], A[2]))
      return mix3(A, [m * B[0], m * B[1], m * B[2]], opacity)
    }
    case 31: return [A[0] + B[0] * opacity, A[1] + B[1] * opacity, A[2] + B[2] * opacity]
    case 32: return mix3(A, [A[0] + A[0] * B[0], A[1] + A[1] * B[1], A[2] + A[2] * B[2]], opacity)
    default: return mix3(A, B, opacity) // Normal
  }
}

function mix3(A, B, k) {
  return [A[0] * (1 - k) + B[0] * k, A[1] * (1 - k) + B[1] * k, A[2] * (1 - k) + B[2] * k]
}

function overlay3(a, b) {
  const f = (x, y) => (x < 0.5 ? 2 * x * y : 1 - 2 * (1 - x) * (1 - y))
  return [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])]
}

function softLight3(a, b) {
  const f = (x, y) => (y < 0.5 ? 2 * x * y + x * x * (1 - 2 * y) : Math.sqrt(x) * (2 * y - 1) + 2 * x * (1 - y))
  return [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])]
}

// VividLight(base, blend) = blend<0.5 ? ColorBurn(base, 2b) : ColorDodge(base, 2(b-0.5))
function vivid(a, b) {
  if (b < 0.5) {
    const bb = 2 * b
    return bb === 0 ? 0 : Math.max(1 - (1 - a) / bb, 0)
  }
  const bb = 2 * (b - 0.5)
  return bb === 1 ? 1 : Math.min(a / (1 - bb), 1)
}

// HSL 转换（common_blending.h 的 RGBToHSL/HSLToRGB 逐字）
function rgbToHsl(c) {
  const fmin = Math.min(c[0], Math.min(c[1], c[2]))
  const fmax = Math.max(c[0], Math.max(c[1], c[2]))
  const delta = fmax - fmin
  const h = [0, 0, 0]
  h[2] = (fmax + fmin) / 2
  if (delta === 0) {
    h[0] = 0
    h[1] = 0
  } else {
    h[1] = h[2] < 0.5 ? delta / (fmax + fmin) : delta / (2 - fmax - fmin)
    const deltaR = ((fmax - c[0]) / 6 + delta / 2) / delta
    const deltaG = ((fmax - c[1]) / 6 + delta / 2) / delta
    const deltaB = ((fmax - c[2]) / 6 + delta / 2) / delta
    if (c[0] === fmax) h[0] = deltaB - deltaG
    else if (c[1] === fmax) h[0] = 1 / 3 + deltaR - deltaB
    else if (c[2] === fmax) h[0] = 2 / 3 + deltaG - deltaR
    if (h[0] < 0) h[0] += 1
    else if (h[0] > 1) h[0] -= 1
  }
  return h
}

function hueToRgb(f1, f2, hue) {
  if (hue < 0) hue += 1
  else if (hue > 1) hue -= 1
  if (6 * hue < 1) return f1 + (f2 - f1) * 6 * hue
  if (2 * hue < 1) return f2
  if (3 * hue < 2) return f1 + (f2 - f1) * ((2 / 3 - hue) * 6)
  return f1
}

function hslToRgb(h) {
  if (h[1] === 0) return [h[2], h[2], h[2]]
  const f2 = h[2] < 0.5 ? h[2] * (1 + h[1]) : h[2] + h[1] - h[1] * h[2]
  const f1 = 2 * h[2] - f2
  return [hueToRgb(f1, f2, h[0] + 1 / 3), hueToRgb(f1, f2, h[0]), hueToRgb(f1, f2, h[0] - 1 / 3)]
}

function hueBlend(a, b) {
  const ah = rgbToHsl(a)
  return hslToRgb([rgbToHsl(b)[0], ah[1], ah[2]])
}
function satBlend(a, b) {
  const ah = rgbToHsl(a)
  return hslToRgb([ah[0], rgbToHsl(b)[1], ah[2]])
}
function colorBlend(a, b) {
  const bh = rgbToHsl(b)
  return hslToRgb([bh[0], bh[1], rgbToHsl(a)[2]])
}
function lumBlend(a, b) {
  const ah = rgbToHsl(a)
  return hslToRgb([ah[0], ah[1], rgbToHsl(b)[2]])
}

// colorItems: tint/pulse/colorkey 参数列表（按 effects 列表顺序）。
// t: rgba [0..255]；u0/v0 为原始 uv（蒙版采样用，见各 frag 的 v_TexCoord.xy）。
export function applyColorChain(colorItems, t, textures, time, u0 = 0.5, v0 = 0.5) {
  for (const it of colorItems) {
    if (it.type === 'tint') {
      // tint.frag:14-28
      let mask = it.alpha
      if (it.masked && it.mask) {
        const m = textures.get(it.mask)
        if (m) mask *= maskChannel(m, sampleTex(m, u0, v0))
      }
      const rgb = applyBlending(it.blendMode, [t[0] / 255, t[1] / 255, t[2] / 255], it.color, mask)
      t = [rgb[0] * 255, rgb[1] * 255, rgb[2] * 255, it.blendMode === 0 ? 255 : t[3]]
    } else if (it.type === 'pulse') {
      // pulse.frag:35-63（无音频路径；音频路径渲染器跳过）
      const sample = it.masked ? t.slice() : null
      let pulse = smoothstep(it.bounds[0], it.bounds[1], Math.sin(time * it.speed + it.phase) * 0.5 + 0.5) * it.amount
      if (it.noiseAmount > 0) {
        const noise = textures.get(it.noise) || WHITE
        const n = sampleTex(noise, time * it.noiseSpeed, time * 0.333 * it.noiseSpeed)
        pulse += (n[0] / 255) * it.noiseAmount
      }
      pulse = Math.pow(pulse, it.power)
      if (it.pulseColor) {
        const A = [t[0] / 255 * it.tintLow[0], t[1] / 255 * it.tintLow[1], t[2] / 255 * it.tintLow[2]]
        const B = [t[0] / 255 * it.tintHigh[0], t[1] / 255 * it.tintHigh[1], t[2] / 255 * it.tintHigh[2]]
        const rgb = applyBlending(it.blendMode, A, B, pulse)
        t = [rgb[0] * 255, rgb[1] * 255, rgb[2] * 255, t[3]]
      }
      if (it.pulseAlpha) t[3] *= pulse
      // pulse.frag:63：rgb 下限 0
      t[0] = Math.max(0, t[0])
      t[1] = Math.max(0, t[1])
      t[2] = Math.max(0, t[2])
      // pulse.frag:58-61：MASK 时 mix 回原采样
      if (sample && it.mask) {
        const m = textures.get(it.mask)
        if (m) {
          const maskVal = maskChannel(m, sampleTex(m, u0, v0))
          t = mix4(sample, t, maskVal)
        }
      }
    } else if (it.type === 'key') {
      // colorkey.frag:14-30
      const delta = Math.abs(it.key[0] - t[0] / 255) + Math.abs(it.key[1] - t[1] / 255) + Math.abs(it.key[2] - t[2] / 255)
      let blend = smoothstep(0.001, 0.002 + it.fuzz, delta - it.tol)
      if (it.invert) blend = 1 - blend
      t[3] *= it.keyAlpha * (1 - blend) + 1 * blend
      if (it.flatten) {
        t[0] *= t[3] / 255
        t[1] *= t[3] / 255
        t[2] *= t[3] / 255
      }
    }
  }
  return t
}
