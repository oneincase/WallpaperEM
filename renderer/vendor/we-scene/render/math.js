// 最小 4x4 矩阵库（列主序，与 WebGL 一致）
export function mat4Identity() {
  return new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1])
}

export function mat4Multiply(a, b) {
  const out = new Float32Array(16)
  for (let c = 0; c < 4; c++) {
    for (let r = 0; r < 4; r++) {
      let s = 0
      for (let k = 0; k < 4; k++) s += a[k * 4 + r] * b[c * 4 + k]
      out[c * 4 + r] = s
    }
  }
  return out
}

// 正交投影（世界 y 向下：top=0, bottom=height）
export function mat4Ortho(left, right, top, bottom, near, far) {
  const out = mat4Identity()
  out[0] = 2 / (right - left)
  out[5] = 2 / (bottom - top)
  out[10] = -2 / (far - near)
  out[12] = -(right + left) / (right - left)
  out[13] = -(bottom + top) / (bottom - top)
  out[14] = -(far + near) / (far - near)
  return out
}

export function mat4Translate(m, x, y, z) {
  return mat4Multiply(m, new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, x, y, z, 1]))
}

export function mat4Scale(m, sx, sy, sz) {
  return mat4Multiply(m, new Float32Array([sx, 0, 0, 0, 0, sy, 0, 0, 0, 0, sz, 0, 0, 0, 0, 1]))
}

export function mat4RotateZ(m, rad) {
  const c = Math.cos(rad)
  const s = Math.sin(rad)
  return mat4Multiply(m, new Float32Array([c, s, 0, 0, -s, c, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]))
}

export function mat4LookAt(eye, center, up) {
  const z = normalize([eye[0] - center[0], eye[1] - center[1], eye[2] - center[2]])
  const x = normalize(cross(up, z))
  const y = cross(z, x)
  const out = new Float32Array([x[0], y[0], z[0], 0, x[1], y[1], z[1], 0, x[2], y[2], z[2], 0, 0, 0, 0, 1])
  out[12] = -(x[0] * eye[0] + x[1] * eye[1] + x[2] * eye[2])
  out[13] = -(y[0] * eye[0] + y[1] * eye[1] + y[2] * eye[2])
  out[14] = -(z[0] * eye[0] + z[1] * eye[1] + z[2] * eye[2])
  return out
}

export function mat4TransformPoint(m, x, y, z) {
  const w = m[3] * x + m[7] * y + m[11] * z + m[15]
  return [
    (m[0] * x + m[4] * y + m[8] * z + m[12]) / w,
    (m[1] * x + m[5] * y + m[9] * z + m[13]) / w,
    (m[2] * x + m[6] * y + m[10] * z + m[14]) / w,
  ]
}

// 场景默认相机（WE 2D 约定：世界坐标 = 像素，y 向下）
// 注意：2D 正交场景渲染时忽略 scene.json 的 eye/center/up（那是编辑器最后保存的相机状态，运行时不用），使用固定相机
// 根据显示模式计算「可见窗口」：把场景设计尺寸（projW×projH）映射到屏幕（width×height）时，
// 用统一的等比缩放，避免 16:9 内容在 16:10 屏幕上被拉伸。
//   cover():  以较大缩放铺满并居中裁切溢出（无黑边、不变形，桌面壁纸默认）。
//   contain():以较小缩放完整显示并居中留边（黑边由 clearColor 填充）。
//   stretch()/其他：保持原行为，忽略宽高比铺满（会拉伸变形）。
export function fitWindow(fit, projW, projH, width, height) {
  if (fit === 'cover') {
    const scale = Math.max(width / projW, height / projH)
    const viewW = width / scale
    const viewH = height / scale
    return { offX: (projW - viewW) / 2, offY: (projH - viewH) / 2, viewW, viewH }
  }
  if (fit === 'contain') {
    const screenAspect = width / height
    const contentAspect = projW / projH
    if (contentAspect > screenAspect) {
      // 内容更宽：宽对齐、加高窗口以完整显示场景（上下留边）
      const viewW = projW
      const viewH = projW / screenAspect
      return { offX: 0, offY: (projH - viewH) / 2, viewW, viewH }
    }
    // 内容更高：高对齐、加宽窗口以完整显示场景（左右留边）
    const viewH = projH
    const viewW = projH * screenAspect
    return { offY: 0, offX: (projW - viewW) / 2, viewW, viewH }
  }
  // stretch（或未识别模式）：显示整个场景，交给 viewport 拉伸
  return { offX: 0, offY: 0, viewW: projW, viewH: projH }
}

export function buildCamera(scene, width, height, fit) {
  const general = scene.general
  const cam = scene.camera
  const eyeV = cam && cam.eye ? parseVec(cam.eye) : [0, 0, 0]
  const centerV = cam && cam.center ? parseVec(cam.center) : [0, 0, -1]
  const upV = cam && cam.up ? parseVec(cam.up) : [0, 1, 0]
  const isOrtho = !general || general.orthogonalprojection || !general.fov
  const view = isOrtho ? mat4Identity() : mat4LookAt(eyeV, centerV, upV)
  const projW = general && general.orthogonalprojection ? general.orthogonalprojection.width || width : width
  const projH = general && general.orthogonalprojection ? general.orthogonalprojection.height || height : height
  // 世界 y 向下：y=0 → NDC +1（屏幕顶）。ortho(left, right, top, bottom)
  const win = fitWindow(fit, projW, projH, width, height)
  const projection = mat4Ortho(win.offX, win.offX + win.viewW, win.offY + win.viewH, win.offY, -10000, 10000)
  return { view, projection, eye: eyeV, projW, projH, offX: win.offX, offY: win.offY, viewW: win.viewW, viewH: win.viewH }
}

function parseVec(s) {
  return String(s).trim().split(/\s+/).map(Number)
}

function normalize(v) {
  const l = Math.hypot(v[0], v[1], v[2]) || 1
  return [v[0] / l, v[1] / l, v[2] / l]
}

function cross(a, b) {
  return [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
