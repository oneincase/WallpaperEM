import { mat4Identity, mat4Multiply, mat4Ortho, mat4RotateZ, mat4Translate, mat4Scale, buildCamera } from './math.js'
import { hlsl2glsl } from './hlsl2glsl.js'
// WebGL2 通用 pass 管线渲染器（移植 linux-wallpaperengine 架构）：
// 每层 copy pass → 效果链（WE shader 转译执行，FBO 乒乓）→ 合成到画布。
// copy/合成用自写 shader；效果 pass 用转译 WE shader（MVP=单位矩阵，mul 转置无影响）。
// 空间：层 FBO 内容正立（v-down 显示空间），与 WE 帧缓冲空间（v-up+倒置画面）数学等价（docs/WE_RENDER_CONVENTIONS.md §1）。

const ALIGN = {
  center: [0.5, 0.5],
  left: [0, 0.5],
  right: [1, 0.5],
  top: [0.5, 0],
  bottom: [0.5, 1],
  topleft: [0, 0],
  topright: [1, 0],
  bottomleft: [0, 1],
  bottomright: [1, 1],
}

const COPY_VERT = `#version 300 es
in vec3 a_Position;
in vec2 a_TexCoord;
uniform mat4 u_MVP;
out vec2 v_UV;
void main() {
  gl_Position = u_MVP * vec4(a_Position, 1.0);
  v_UV = a_TexCoord;
}`

const COPY_FRAG = `#version 300 es
precision mediump float;
in vec2 v_UV;
uniform sampler2D u_Tex;
uniform vec4 u_Color4;
out vec4 fragColor;
void main() {
  fragColor = texture(u_Tex, v_UV) * u_Color4;
}`

const COMPOSITE_FRAG = `#version 300 es
precision mediump float;
in vec2 v_UV;
uniform sampler2D u_Tex;
out vec4 fragColor;
void main() {
  fragColor = texture(u_Tex, v_UV);
}`

// quad 顶点（每顶点 5 float：x,y,z,u,v）
// WE 同款空间：层 FBO 内容倒置（FBO 顶=纹理底行），pass quad 顶 v=1（顶采顶直通），合成时再正过来。
function layerQuadVerts(w, h) {
  return new Float32Array([
    0, h, 0, 0, 1, // 层空间顶（y=h）采样 v=1（纹理底行）→ FBO 顶=纹理底（倒置，与 WE 一致）
    0, 0, 0, 0, 0,
    w, h, 0, 1, 1,
    w, h, 0, 1, 1,
    0, 0, 0, 0, 0,
    w, 0, 0, 1, 0,
  ])
}
function passQuadVerts() {
  return new Float32Array([
    -1, 1, 0, 0, 1, // NDC 顶 v=1（FBO 纹理 v=1=顶行，顶采顶直通）
    -1, -1, 0, 0, 0,
    1, 1, 0, 1, 1,
    1, 1, 0, 1, 1,
    -1, -1, 0, 0, 0,
    1, -1, 0, 1, 0,
  ])
}
function localQuadVerts() {
  return new Float32Array([
    -0.5, 0.5, 0, 0, 1, // local +y = 屏幕下方（y-down 世界）：屏幕底采样 v=1（FBO 顶=纹理底）→ 屏幕底=纹理底
    -0.5, -0.5, 0, 0, 0, // local -y = 屏幕上方：屏幕顶采样 v=0（FBO 底=纹理顶）→ 屏幕顶=纹理顶（正立）
    0.5, 0.5, 0, 1, 1,
    0.5, 0.5, 0, 1, 1,
    -0.5, -0.5, 0, 0, 0,
    0.5, -0.5, 0, 1, 0,
  ])
}

const GL_TYPES = {
  0x1406: 'float', // FLOAT
  0x8b50: 'vec2', // FLOAT_VEC2
  0x8b51: 'vec3', // FLOAT_VEC3
  0x8b52: 'vec4', // FLOAT_VEC4
  0x1404: 'int', // INT
  0x8b53: 'ivec2',
  0x8b54: 'ivec3',
  0x8b55: 'ivec4',
  0x8b56: 'bool',
  0x8b5c: 'mat4', // FLOAT_MAT4
  0x8b5b: 'mat3', // FLOAT_MAT3
}

export function createRenderer(canvas, opts = {}) {
  const gl = canvas.getContext('webgl2', { premultipliedAlpha: false, antialias: false, alpha: false, preserveDrawingBuffer: true })
  if (!gl) throw new Error('当前浏览器不支持 WebGL2')
  const shaderResolver = opts.shaderResolver || (async () => null)
  const diag = opts.diag || (() => {})
  // [we-scene patch] 视频帧中转离屏 canvas（video→GL 直传在部分 WebView 受限，用 drawImage 中转更稳）
  const videoCanvas = typeof document !== 'undefined' ? document.createElement('canvas') : null
  if (videoCanvas) {
    videoCanvas.style.cssText = 'position:fixed;left:-9999px;top:-9999px;width:2px;height:2px;opacity:0'
  }
  let videoCanvasReported = false
  // FBO 分辨率限幅系数：0 = 关闭（全质量）；>0 时效果链 FBO 上限 = 屏幕占比 × 系数
  let fboCapFactor = opts.fboCapFactor === undefined ? 0 : opts.fboCapFactor
  // [we-scene patch] 粒子系统渲染回调（由宿主注入，见 setParticleRenderer）
  let renderParticlesFn = null

  const copyProg = linkProgram(gl, COPY_VERT, COPY_FRAG)
  const compProg = linkProgram(gl, COPY_VERT, COMPOSITE_FRAG)

  const vao = gl.createVertexArray()
  gl.bindVertexArray(vao)
  const vbuf = gl.createBuffer()
  gl.bindBuffer(gl.ARRAY_BUFFER, vbuf)
  gl.enableVertexAttribArray(0)
  gl.vertexAttribPointer(0, 3, gl.FLOAT, false, 20, 0)
  gl.enableVertexAttribArray(1)
  gl.vertexAttribPointer(1, 2, gl.FLOAT, false, 20, 12)
  gl.bindVertexArray(null)

  // FBO 缓存（tag 区分用途：乒乓 A/B 必须是两个独立实例；同一 tag+尺寸复用）
  const fboCache = new Map()
  function getFBO(w, h, tag) {
    const key = (tag || '') + '|' + w + 'x' + h
    if (fboCache.has(key)) return fboCache.get(key)
    const fbo = gl.createFramebuffer()
    const tex = gl.createTexture()
    gl.bindTexture(gl.TEXTURE_2D, tex)
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, null)
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR)
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR)
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE)
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE)
    gl.bindFramebuffer(gl.FRAMEBUFFER, fbo)
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0)
    // 新建附件内容未定义。效果的中间 target FBO（_rt_*）可能因其写入 pass 编译失败而
    // 始终没被写过，后续 pass 采样到未定义内容会得到整屏白/花屏，故先清零。
    gl.clearColor(0, 0, 0, 0)
    gl.clear(gl.COLOR_BUFFER_BIT)
    gl.bindFramebuffer(gl.FRAMEBUFFER, null)
    const entry = { fbo, tex, width: w, height: h }
    fboCache.set(key, entry)
    return entry
  }

  // 效果 shader 缓存：key = shaderName + '|' + JSON.stringify(combos)
  const progCache = new Map()
  const includeCache = new Map()
  const shaderSrcCache = new Map()
  // 解析 material 元数据：uniform 声明行注释里的 {"material":"speedx","default":1} → { speedx: { uniform, default } }
  function parseMaterialMeta(src) {
    const meta = {}
    const re = /uniform\s+[A-Za-z0-9_]+\s+([A-Za-z_][A-Za-z0-9_]*)[^;]*;\s*\/\/([^\n]*)/g
    let m
    while ((m = re.exec(src)) !== null) {
      const uniformName = m[1]
      const comment = m[2]
      const mat = /"material"\s*:\s*"([^"]+)"/.exec(comment)
      if (!mat) continue
      const def = /"default"\s*:\s*("(?:[^"]*)"|-?\d+(?:\.\d+)?)/.exec(comment)
      meta[mat[1]] = { uniform: uniformName, default: def ? parseDefaultValue(def[1]) : undefined }
    }
    return meta
  }
  // 纹理关联 combo：sampler uniform 注释声明 combo，且该槽提供了纹理 → combo = 1（ShaderUnit.cpp:545-617）
  function parseTextureCombos(src) {
    const out = []
    const re = /uniform\s+sampler2D\s+(g_Texture(\d+))[^;]*;\s*\/\/([^\n]*)/g
    let m
    while ((m = re.exec(src)) !== null) {
      const combo = /"combo"\s*:\s*"([^"]+)"/.exec(m[3])
      if (combo) out.push({ slot: Number(m[2]), name: m[1], combo: combo[1] })
    }
    return out
  }
  function parseDefaultValue(s) {
    if (s.startsWith('"')) return s.slice(1, -1)
    const n = Number(s)
    return Number.isFinite(n) ? n : undefined
  }
  async function getEffectProgram(shaderName, combos, providedTextures) {
    // shader 源与纹理 combo 按名缓存：避免每帧每 pass 重新 fetch/正则
    let src = shaderSrcCache.get(shaderName)
    if (src === undefined) {
      const fragSrc = (await shaderResolver('shaders/' + shaderName + '.frag')) || ''
      const vertSrc = (await shaderResolver('shaders/' + shaderName + '.vert')) || ''
      src = { frag: fragSrc, vert: vertSrc, texCombos: parseTextureCombos(fragSrc) }
      shaderSrcCache.set(shaderName, src)
    }
    // 纹理关联 combo 并入 combos（有显式值则不覆盖）
    const effectiveCombos = { ...combos }
    for (const tc of src.texCombos) {
      if (providedTextures && providedTextures[tc.slot] && effectiveCombos[tc.combo] === undefined) {
        effectiveCombos[tc.combo] = 1
      }
    }
    const key = shaderName + '|' + JSON.stringify(effectiveCombos)
    if (progCache.has(key)) return progCache.get(key)
    // include 同步缓存：miss 时记录并补拉，重试转译
    for (let attempt = 0; attempt < 4; attempt++) {
      const missing = new Set()
      const resolver = (file) => {
        if (includeCache.has(file)) return includeCache.get(file)
        missing.add(file)
        return null
      }
      const fragGlsl = hlsl2glsl(src.frag, 'frag', effectiveCombos, resolver)
      const vertGlsl = hlsl2glsl(src.vert, 'vert', effectiveCombos, resolver)
      if (missing.size === 0) {
        let prog
        try {
          prog = linkProgram(gl, vertGlsl, fragGlsl)
        } catch (e) {
          throw new Error('shader=' + shaderName + ' ' + (e && e.message))
        }
        const uni = new Map()
        const n = gl.getProgramParameter(prog, gl.ACTIVE_UNIFORMS)
        for (let i = 0; i < n; i++) {
          const info = gl.getActiveUniform(prog, i)
          const base = info.name.replace(/\[0\]$/, '')
          uni.set(base, { loc: gl.getUniformLocation(prog, info.name), type: GL_TYPES[info.type] || 'unknown' })
        }
        const matMeta = { ...parseMaterialMeta(src.vert), ...parseMaterialMeta(src.frag) }
        const entry = { prog, uni, matMeta, fragGlsl, vertGlsl }
        progCache.set(key, entry)
        return entry
      }
      await Promise.all(Array.from(missing).map(async (f) => {
        includeCache.set(f, (await shaderResolver('shaders/' + f)) || '')
      }))
    }
    throw new Error('include 解析失败: ' + shaderName)
  }

  const whiteTex = makeTexture(gl, new Uint8Array([255, 255, 255, 255]), 1, 1)
  // 无纹理的非 solid 层（纯效果层/文字对象层）：WE 语义为空层内容透明（白会导致纯白方块）
  const transparentTex = makeTexture(gl, new Uint8Array([0, 0, 0, 0]), 1, 1)

  // ---- 视差（cameraparallax + 对象 parallaxDepth）----
  const parallaxState = { x: 0, y: 0, sx: 0, sy: 0 }
  let lastParallaxTime = 0
  let parallaxAttached = false
  function attachParallaxListener() {
    if (parallaxAttached || typeof window === 'undefined' || !window.addEventListener) return
    parallaxAttached = true
    window.addEventListener('mousemove', (ev) => {
      const w = window.innerWidth || 1
      const h = window.innerHeight || 1
      parallaxState.x = (ev.clientX / w) * 2 - 1
      parallaxState.y = (ev.clientY / h) * 2 - 1
    })
  }
  // 层视差缩放（每帧由 renderScene 更新）
  let layerParallaxScaleX = 0
  let layerParallaxScaleY = 0

  // ---------- uniform 设置 ----------
  function setVal(uni, name, setter) {
    const u = uni.get(name)
    if (u && u.loc !== null) setter(u.loc, u.type)
  }
  function parseVecValue(v) {
    if (typeof v === 'number') return [v, v, v, v]
    const p = String(v).trim().split(/\s+/).map(Number)
    return [p[0] || 0, p[1] || 0, p[2] || 0, p[3] || 0]
  }
  function setConstant(uni, name, value) {
    const u = uni.get(name)
    if (!u || u.loc === null) return
    const raw = value && value.value !== undefined ? value.value : value
    const arr = parseVecValue(raw)
    switch (u.type) {
      case 'float': gl.uniform1f(u.loc, arr[0]); break
      case 'int':
      case 'bool': gl.uniform1i(u.loc, raw === true || raw === 1 ? 1 : Math.round(arr[0])); break
      case 'vec2': gl.uniform2f(u.loc, arr[0], arr[1]); break
      case 'vec3': gl.uniform3f(u.loc, arr[0], arr[1], arr[2]); break
      case 'vec4': gl.uniform4f(u.loc, arr[0], arr[1], arr[2], arr[3]); break
      case 'mat4': gl.uniformMatrix4fv(u.loc, false, IDENT_M4); break
      case 'mat3': gl.uniformMatrix3fv(u.loc, false, IDENT_M3); break
      default: break
    }
  }
  const mat3Identity = () => new Float32Array([1, 0, 0, 0, 1, 0, 0, 0, 1])

  function bindSystemUniforms(uni, layer, time, projW, projH, mvp, modelM, viewProjM, resolutions) {
    setVal(uni, 'g_Time', (l) => gl.uniform1f(l, time))
    setVal(uni, 'g_Daytime', (l) => gl.uniform1f(l, 0))
    setVal(uni, 'g_ModelViewProjectionMatrix', (l) => gl.uniformMatrix4fv(l, false, mvp))
    setVal(uni, 'g_ModelMatrix', (l) => gl.uniformMatrix4fv(l, false, modelM))
    setVal(uni, 'g_ViewProjectionMatrix', (l) => gl.uniformMatrix4fv(l, false, viewProjM))
    setVal(uni, 'g_ModelViewProjectionMatrixInverse', (l) => gl.uniformMatrix4fv(l, false, IDENT_M4))
    setVal(uni, 'g_Brightness', (l) => gl.uniform1f(l, layer.brightness))
    setVal(uni, 'g_UserAlpha', (l) => gl.uniform1f(l, layer.alpha))
    setVal(uni, 'g_Alpha', (l) => gl.uniform1f(l, layer.alpha))
    setVal(uni, 'g_Color', (l) => gl.uniform3f(l, layer.color[0], layer.color[1], layer.color[2]))
    setVal(uni, 'g_Color4', (l) => gl.uniform4f(l, layer.color[0], layer.color[1], layer.color[2], 1))
    setVal(uni, 'g_CompositeColor', (l) => gl.uniform3f(l, layer.color[0], layer.color[1], layer.color[2]))
    setVal(uni, 'g_TexelSize', (l) => gl.uniform2f(l, 1 / projW, 1 / projH))
    setVal(uni, 'g_TexelSizeHalf', (l) => gl.uniform2f(l, 0.5 / projW, 0.5 / projH))
    setVal(uni, 'g_TextureReductionScale', (l) => gl.uniform1f(l, 1))
    setVal(uni, 'g_PointerPosition', (l) => gl.uniform2f(l, 0, 0))
    setVal(uni, 'g_PointerPositionLast', (l) => gl.uniform2f(l, 0, 0))
    for (let i = 0; i < 8; i++) {
      setVal(uni, 'g_Texture' + i, (l) => gl.uniform1i(l, i))
    }
    for (const [i, res] of resolutions) {
      setVal(uni, 'g_Texture' + i + 'Resolution', (l) => gl.uniform4f(l, res[0], res[1], res[2], res[3]))
    }
  }

  // constantshadervalues 的键是 material 名 → 经 matMeta 映射到 uniform 名并设值；缺省用注释 default
  function bindConstants(uni, constants, matMeta) {
    for (const [matKey, value] of Object.entries(constants || {})) {
      const entry = matMeta && matMeta[matKey]
      if (!entry) continue
      setConstant(uni, entry.uniform, value)
    }
    // 未提供的常数用 shader 注释里的 default
    for (const [matKey, entry] of Object.entries(matMeta || {})) {
      if (!constants || !(matKey in constants)) {
        if (entry.default !== undefined) setConstant(uni, entry.uniform, entry.default)
      }
    }
  }

  function resolveTextureName(name, inputFBO, effectFBOs, textures) {
    if (name === null || name === undefined || name === '') return null
    if (name.startsWith('_rt_')) {
      if (name.startsWith('_rt_imageLayerComposite')) return inputFBO
      if (effectFBOs.has(name)) return effectFBOs.get(name)
      return null
    }
    return textures.get(name) || null
  }

  // ---------- 绘制辅助 ----------
  // 静态 quad 单例 + 变更才上传：避免每帧每 pass 新建 Float32Array 与 bufferData
  const PASS_QUAD = passQuadVerts()
  const LOCAL_QUAD = localQuadVerts()
  const layerQuadCache = new Map()
  function layerQuad(w, h) {
    const key = w + 'x' + h
    let q = layerQuadCache.get(key)
    if (q === undefined) {
      q = layerQuadVerts(w, h)
      layerQuadCache.set(key, q)
    }
    return q
  }
  let currentQuadKey = null
  function uploadQuad(key, verts) {
    if (currentQuadKey === key) return
    gl.bindBuffer(gl.ARRAY_BUFFER, vbuf)
    gl.bufferData(gl.ARRAY_BUFFER, verts, gl.DYNAMIC_DRAW)
    currentQuadKey = key
  }

  // copy/composite 程序 uniform 位置缓存（每帧查找 → 一次初始化）
  const copyUni = {
    mvp: gl.getUniformLocation(copyProg, 'u_MVP'),
    tex: gl.getUniformLocation(copyProg, 'u_Tex'),
    color: gl.getUniformLocation(copyProg, 'u_Color4'),
  }
  const compUni = {
    mvp: gl.getUniformLocation(compProg, 'u_MVP'),
    tex: gl.getUniformLocation(compProg, 'u_Tex'),
  }
  const IDENT_M4 = mat4Identity()
  const IDENT_M3 = mat3Identity()
  function setBlend(mode) {
    if (mode === 'translucent') {
      gl.enable(gl.BLEND)
      gl.blendFuncSeparate(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA, gl.ONE, gl.ONE_MINUS_SRC_ALPHA)
    } else if (mode === 'additive') {
      gl.enable(gl.BLEND)
      gl.blendFuncSeparate(gl.SRC_ALPHA, gl.ONE, gl.ONE, gl.ONE)
    } else {
      gl.disable(gl.BLEND)
    }
  }

  // 图层的 colorBlendMode（scene.json 的 colorBlendMode，语义同 common_blending.h 的
  // blend mode 编号）→ 合成到画布时的 GL 混合模式。
  // 只映射能用固定管线表达的几种；其余（Overlay/SoftLight 等需要读回目标色）
  // 退回 translucent，与此前行为一致。
  //
  // 这一步不做的后果：像 3299228616 的 ripple1440p 水面层，贴图是一张几乎全黑、
  // 靠 Add 混合只贡献亮部高光的图（colorBlendMode=9），若按 translucent 合成，
  // 黑色像素会被当成不透明色直接糊住背景，看起来就是"一层黑色蒙版盖住了壁纸"。
  const COLOR_BLEND_GL = {
    2: 'multiply', // Multiply
    6: 'additive', // Lighten（近似）
    7: 'screen', // Screen
    9: 'additive', // Add
    31: 'additive', // Add(带 opacity 权重)
  }
  function setColorBlend(colorBlendMode) {
    const mode = COLOR_BLEND_GL[colorBlendMode]
    if (mode === 'additive') {
      gl.enable(gl.BLEND)
      gl.blendFuncSeparate(gl.SRC_ALPHA, gl.ONE, gl.ONE, gl.ONE)
    } else if (mode === 'multiply') {
      gl.enable(gl.BLEND)
      gl.blendFuncSeparate(gl.DST_COLOR, gl.ZERO, gl.ONE, gl.ONE_MINUS_SRC_ALPHA)
    } else if (mode === 'screen') {
      gl.enable(gl.BLEND)
      gl.blendFuncSeparate(gl.ONE, gl.ONE_MINUS_SRC_COLOR, gl.ONE, gl.ONE_MINUS_SRC_ALPHA)
    } else {
      setBlend('translucent')
    }
  }
  function drawQuad(prog, fbo, w, h, verts, mvp, blending) {
    gl.useProgram(prog)
    setBlend(blending)
    gl.bindFramebuffer(gl.FRAMEBUFFER, fbo ? fbo.fbo : null)
    gl.viewport(0, 0, w, h)
    gl.bindVertexArray(vao)
    uploadQuad('draw', verts)
    const loc = gl.getUniformLocation(prog, 'u_MVP')
    gl.uniformMatrix4fv(loc, false, mvp)
    gl.drawArrays(gl.TRIANGLES, 0, 6)
  }

  // ---------- 视锥裁剪 ----------
  // 图层的世界空间 AABB 与可见窗口是否完全不相交。
  // 世界坐标同 layerModelMatrix：y 已翻成 cam.projH - origin.y，可见窗口是
  // [offX, offX+viewW] × [offY, offY+viewH]（见 buildCamera 的 fitWindow）。
  function isLayerOffscreen(layer, cam) {
    const sw = layer.size[0] * layer.scale[0]
    const sh = layer.size[1] * layer.scale[1]
    // 尺寸未知（0）的层不裁：文字/声音/纯效果层的 size 常为 0，但仍可能有内容
    if (sw === 0 || sh === 0) return false
    // 负缩放（镜像）会让宽高为负，取绝对值才是真实包围盒
    let halfW = Math.abs(sw) / 2
    let halfH = Math.abs(sh) / 2
    // 旋转后的 AABB：用旋转矩阵作用于半宽半高向量，取绝对值之和
    const ang = layer.angles[2]
    if (ang !== 0) {
      const c = Math.abs(Math.cos(ang))
      const s = Math.abs(Math.sin(ang))
      const rw = halfW * c + halfH * s
      const rh = halfW * s + halfH * c
      halfW = rw
      halfH = rh
    }
    const cx = layer.origin[0]
    const cy = cam.projH - layer.origin[1]
    // 视差与相机抖动会让层在几十像素内浮动；留一档余量避免边缘层被误裁。
    // 对象级视差最大位移 = |parallaxDepth| × layerParallaxScale（已封顶到 60px）。
    let margin = 64
    if (layer.parallaxDepth) {
      margin += Math.abs(layer.parallaxDepth[0] * layerParallaxScaleX)
      margin += Math.abs(layer.parallaxDepth[1] * layerParallaxScaleY)
    }
    return (
      cx + halfW + margin < cam.offX ||
      cx - halfW - margin > cam.offX + cam.viewW ||
      cy + halfH + margin < cam.offY ||
      cy - halfH - margin > cam.offY + cam.viewH
    )
  }

  // ---------- 合成（层 → 画布） ----------
  // 图层局部变换（不含投影）：把 [-0.5,0.5] 的 local quad 映射到世界空间
  function layerModelMatrix(layer, cam) {
    const w = layer.size[0] * layer.scale[0]
    const h = layer.size[1] * layer.scale[1]
    let m = mat4Identity()
    m = mat4Translate(m, layer.origin[0], cam.projH - layer.origin[1], layer.origin[2])
    // 对象级视差（parallaxDepth）：近景 depth 正值随鼠标位移放大、远景负值反向。
    // 放在旋转之前 = 沿世界轴平移（视差是相机效果，不应被图层自身旋转带偏）。
    if (layer.parallaxDepth && (layerParallaxScaleX !== 0 || layerParallaxScaleY !== 0)) {
      m = mat4Translate(
        m,
        layer.parallaxDepth[0] * layerParallaxScaleX,
        layer.parallaxDepth[1] * layerParallaxScaleY,
        0,
      )
    }
    // 旋转：参考实现 y-up 空间 rotate(-angle)，等效 y-down 屏幕 rotate(-angle)（正角度=屏幕逆时针）
    m = mat4RotateZ(m, -layer.angles[2])
    return { m, w, h }
  }

  function compositeLayer(prog, inputTex, color4, layer, cam, viewProj, width, height) {
    const a = ALIGN[layer.alignment] || [0.5, 0.5]
    const base = layerModelMatrix(layer, cam)
    const m = mat4Scale(base.m, base.w, base.h, 1)
    void a
    const mvp = mat4Multiply(viewProj, m)
    const uni = prog === compProg ? compUni : copyUni
    gl.useProgram(prog)
    setColorBlend(layer.colorBlendMode)
    gl.bindFramebuffer(gl.FRAMEBUFFER, null)
    gl.viewport(0, 0, width, height)
    gl.bindVertexArray(vao)
    uploadQuad('local', LOCAL_QUAD)
    gl.activeTexture(gl.TEXTURE0)
    gl.bindTexture(gl.TEXTURE_2D, inputTex)
    gl.uniform1i(uni.tex, 0)
    gl.uniformMatrix4fv(uni.mvp, false, mvp)
    if (uni.color !== null && uni.color !== undefined) gl.uniform4f(uni.color, color4[0], color4[1], color4[2], color4[3])
    gl.drawArrays(gl.TRIANGLES, 0, 6)
  }

  // [we-scene patch] puppet 骨骼网格图层：由外部注入的 MDL 渲染器绘制（见 setPuppetRenderer）
  // 网格坐标 = 图层局部像素、y 轴朝上，故 model 矩阵在层变换后翻转 y。
  let puppetDrawFn = null
  function puppetModelMatrix(layer, cam) {
    const base = layerModelMatrix(layer, cam)
    // scale(sx, -sy)：网格 y-up → 场景 y-down；网格坐标已是像素，不再乘 size
    return mat4Scale(base.m, layer.scale[0], -layer.scale[1], 1)
  }

  // 直接绘制到画布（无效果链）
  function drawPuppetDirect(layer, cam, viewProj, width, height, time) {
    gl.bindFramebuffer(gl.FRAMEBUFFER, null)
    gl.viewport(0, 0, width, height)
    const mvp = mat4Multiply(viewProj, puppetModelMatrix(layer, cam))
    puppetDrawFn(layer, mvp, { time })
    currentQuadKey = null // MDL 渲染器换过 VAO/buffer，失效 quad 缓存
  }

  // 绘制到层 FBO（供效果链使用）：内容朝向必须与 copy pass 完全一致。
  // copy pass 的 quad 约定（layerQuadVerts）令「FBO 的 NDC 顶 ← 纹理 v=1 ← 图像底行」，
  // 即层 FBO 里的画面是上下倒置的。网格 y-up 且 v=(H/2-y)/H，
  // 故网格顶（v=0，图像顶行）必须落到 FBO 的 NDC 底 ⇒ 用 y-down 的正交投影
  // （mat4Ortho 的 top/bottom 传成 h/0）。注意只能翻几何、不能翻 UV：
  // 翻 UV 会把贴图镜像贴到未翻转的网格上，得到上下颠倒的人物。
  function drawPuppetToFBO(layer, fbo, fboW, fboH, time) {
    gl.bindFramebuffer(gl.FRAMEBUFFER, fbo.fbo)
    gl.viewport(0, 0, fboW, fboH)
    gl.clearColor(0, 0, 0, 0)
    gl.clear(gl.COLOR_BUFFER_BIT)
    const w = layer.size[0]
    const h = layer.size[1]
    let m = mat4Ortho(0, w, h, 0, -10000, 10000)
    m = mat4Translate(m, w / 2, h / 2, 0)
    puppetDrawFn(layer, m, { time })
    currentQuadKey = null
    gl.bindVertexArray(vao)
  }


  async function renderScene(scene, textures, width, height, time, fit) {
    gl.viewport(0, 0, width, height)
    const general = scene.general || {}
    if (general.clearenabled !== false) {
      const cc = parseVec3Local(general.clearcolor || '0 0 0')
      gl.clearColor(cc[0], cc[1], cc[2], 1)
    } else {
      gl.clearColor(0, 0, 0, 1)
    }
    gl.clear(gl.COLOR_BUFFER_BIT)
    const cam = buildCamera(scene, width, height, fit)
    let viewProj = mat4Multiply(cam.projection, cam.view)

    // ---- 场景级视差（cameraparallax）----
    const parRaw = general.cameraparallax
    const parEnabled = parRaw === true || (parRaw !== null && typeof parRaw === 'object' && parRaw.value === true)
    layerParallaxScaleX = 0
    layerParallaxScaleY = 0
    if (parEnabled && opts.parallax !== false) {
      attachParallaxListener()
      const amount = typeof general.cameraparallaxamount === 'number' ? general.cameraparallaxamount : 0
      const influence = typeof general.cameraparallaxmouseinfluence === 'number' ? general.cameraparallaxmouseinfluence : 1
      const delay = typeof general.cameraparallaxdelay === 'number' ? general.cameraparallaxdelay : 1
      const parDt = time - lastParallaxTime
      lastParallaxTime = time
      const parAlpha = parDt > 0 ? 1 - Math.exp(-parDt / Math.max(0.05, delay)) : 1
      parallaxState.sx += (parallaxState.x - parallaxState.sx) * parAlpha
      parallaxState.sy += (parallaxState.y - parallaxState.sy) * parAlpha
      const strength = amount * influence
      // WE 语义：amount 是「相机偏移占可见画面的比例」（鼠标居中 0、到边缘 ±1）。
      // 但 amount 只按比例换算会让「作者没动过的默认值」把画面整体推走：
      // 全库实测 amount=0.5 / mouseinfluence=0.5 就是编辑器默认值（13/21 个场景原样保留），
      // 真正调过视差的作者会显式改到 0.01~0.08。按纯比例算，默认值在 3840 宽的场景上
      // 得到 ±960px（约 1/4 屏）的相机位移 —— 表现为鼠标一动整个画面主体过度飘移。
      // WE 的实际观感是几十像素级的轻微浮动，故这里对相机位移取绝对上限封顶：
      // 既保留作者显式调小时的比例关系，也让默认值退化为可接受的轻微浮动。
      const PARALLAX_MAX_PX = 60
      const rawOffX = parallaxState.sx * strength * cam.viewW
      const rawOffY = parallaxState.sy * strength * cam.viewH
      // 按 x/y 里更大的超出比例统一缩放，避免单轴截断改变位移方向
      const over = Math.max(Math.abs(rawOffX), Math.abs(rawOffY)) / PARALLAX_MAX_PX
      const damp = over > 1 ? 1 / over : 1
      const parOffX = rawOffX * damp
      const parOffY = rawOffY * damp
      if (parOffX !== 0 || parOffY !== 0) {
        // 必须右乘：viewProj · translate = 在**世界空间**平移。
        // 若写成 translate · viewProj，平移会落在投影之后的 NDC 空间（全宽仅 2.0），
        // 几十像素的偏移被当成十几个屏幕宽 → 鼠标一动整个画面就跑飞。
        viewProj = mat4Multiply(viewProj, mat4Translate(mat4Identity(), parOffX, parOffY, 0))
      }
      // 对象级视差基准：depth 1.0 的层位移约等于场景级位移
      layerParallaxScaleX = parOffX
      layerParallaxScaleY = parOffY
    }

    for (const layer of scene.layers) {
      if (!layer.visible) continue
      if (layer.isContainer) continue
      // [we-scene patch] 全屏后期处理层（projectlayer / fullscreenlayer）：
      // 其内容应是「已渲染画面的副本」，尚未实现回读，先整层跳过。
      // 当普通空层渲染会被效果末尾的 compositecolor 铺成纯白，糊掉整个画面。
      if (layer.isPostProcess) continue
      if (layer.particle) {
        // [we-scene patch] 粒子图层：由外部 ParticleSystem 模拟+渲染（见 renderer.renderParticles）
        continue
      }
      // [we-scene patch] 视锥裁剪：完全落在可见窗口外的图层不必渲染。
      // 作者常放超出屏幕的大图供视差平移（3113287126 的背景层 quad 达 8289×1554，
      // 而屏幕只有 3840 宽），这些层的效果链 FBO 会按整层尺寸分配 —— 跳过屏外层
      // 既省显存与逐 pass 开销，也不会改变画面（屏外内容本就被裁掉）。
      if (isLayerOffscreen(layer, cam)) continue
      await renderLayer(layer, textures, cam, viewProj, width, height, time)
    }
    // [we-scene patch] 渲染叠加粒子系统（在图层之后，与场景同投影）
    if (renderParticlesFn) {
      try { await renderParticlesFn(cam, viewProj, width, height, time) }
      catch (e) { console.warn('[we-scene] 粒子渲染失败:', e && e.message) }
    }
    gl.bindVertexArray(null)
  }

  async function renderLayer(layer, textures, cam, viewProj, width, height, time) {
    // [we-scene patch] puppet 图层：几何由 MDL 网格提供，而非层 quad
    const isPuppet = !!(layer.puppet && puppetDrawFn)
    const texObj = !layer.solid && layer.textureName ? textures.get(layer.textureName) : null
    // 视频纹理层：把当前视频帧上传到 WebGL（帧时间戳变化才上传）
    if (texObj && texObj.video) {
      const v = texObj.video
      if (v.readyState >= 2 && v.currentTime !== texObj.lastUploaded) {
        gl.bindTexture(gl.TEXTURE_2D, texObj.glTex)
        try {
          // [we-scene patch] 用离屏 canvas 中转视频帧（video 直传 WebGL 在 WKWebView 可能失败）
          let vw = v.videoWidth || texObj.width
          let vh = v.videoHeight || texObj.height
          // 纹理尺寸上限：超过则等比缩放（避免 texImage2D 失败 / 每帧 4K 上传开销大）
          const MAX_DIM = 2048
          const maxTex = gl.getParameter(gl.MAX_TEXTURE_SIZE)
          const limit = maxTex > 0 && maxTex < 4096 ? Math.min(MAX_DIM, maxTex) : MAX_DIM
          let scale = 1
          if (Math.max(vw, vh) > limit) scale = limit / Math.max(vw, vh)
          const uw = Math.max(1, Math.round(vw * scale))
          const uh = Math.max(1, Math.round(vh * scale))
          let src = v
          if (videoCanvas && uw > 0 && uh > 0) {
            if (videoCanvas.width !== uw || videoCanvas.height !== uh) {
              videoCanvas.width = uw
              videoCanvas.height = uh
            }
            const vctx = videoCanvas.getContext('2d')
            if (vctx) {
              vctx.clearRect(0, 0, uw, uh)
              vctx.drawImage(v, 0, 0, uw, uh)
              src = videoCanvas
            }
          }
          gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, src)
          // canvas 直传失败（部分 WebView）：回退为像素数据上传
          if (gl.getError() !== gl.NO_ERROR && videoCanvas) {
            try {
              const vctx2 = videoCanvas.getContext('2d')
              const id = vctx2.getImageData(0, 0, videoCanvas.width, videoCanvas.height)
              gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, videoCanvas.width, videoCanvas.height, 0, gl.RGBA, gl.UNSIGNED_BYTE, new Uint8Array(id.data.buffer))
              while (gl.getError() !== gl.NO_ERROR) {}
            } catch (e) { /* ignore */ }
          }
          texObj.width = uw
          texObj.height = uh
          texObj.lastUploaded = v.currentTime
          if (!videoCanvasReported) {
            videoCanvasReported = true
            diag('video tex ready ' + uw + 'x' + uh + ' (src ' + vw + 'x' + vh + ')')
          }
        } catch (e) {
          if (!videoCanvasReported) {
            videoCanvasReported = true
            diag('video upload FAIL: ' + ((e && e.message) || e))
          }
          console.warn('[we-scene] 视频帧上传失败:', e && e.message, 'readyState=', v.readyState, 'cur=', v.currentTime)
          // 视频帧不可用（如跨域/解码中）：保留上一帧
        }
      }
    }
    const srcTex = texObj && texObj.glTex ? texObj.glTex : (layer.solid ? whiteTex : transparentTex)
    // puppet 层的层内容尺寸由 size 决定（网格坐标即层局部像素），而非贴图尺寸
    const w = Math.max(1, isPuppet ? Math.round(layer.size[0]) : texObj ? texObj.width : 1)
    const h = Math.max(1, isPuppet ? Math.round(layer.size[1]) : texObj ? texObj.height : 1)
    const color4 = [layer.color[0] * layer.brightness, layer.color[1] * layer.brightness, layer.color[2] * layer.brightness, layer.alpha]
    const effects = (layer.effects || []).filter((e) => e.visible)

    // 效果降采样（性能档位）：fboCapFactor > 0 时效果链 FBO 上限 = 屏幕占比 × 系数（0=全质量）。
    // 注意 copy pass 会把**整张贴图**铺满 fboW×fboH，所以任何缩小 FBO 的做法都是降质，
    // 不能以「屏外部分反正看不见」为理由单独裁小 —— 屏外内容同样占着贴图 UV 空间。
    // 巨型层（如 3113287126 的背景层 8289×1554）在全质量档确实按整层分配，
    // 这是全质量的既定代价；要省显存请调 fboCapFactor，而不是在这里做隐式缩减。
    let fboW = w
    let fboH = h
    if (fboCapFactor > 0 && cam.projW > 0 && cam.projH > 0) {
      // 层在屏幕上的实际占用；超出可见窗口的部分不必参与分辨率预算
      const visW = Math.min(Math.abs(layer.size[0] * layer.scale[0]), cam.viewW)
      const visH = Math.min(Math.abs(layer.size[1] * layer.scale[1]), cam.viewH)
      const screenW = visW * (width / cam.projW)
      const screenH = visH * (height / cam.projH)
      if (screenW > 0 && screenH > 0) {
        const capW = Math.max(64, Math.round(screenW * fboCapFactor))
        const capH = Math.max(64, Math.round(screenH * fboCapFactor))
        if (capW < w) fboW = capW
        if (capH < h) fboH = capH
      }
    }

    // 无效果：直接合成
    if (effects.length === 0) {
      if (isPuppet) drawPuppetDirect(layer, cam, viewProj, width, height, time)
      else compositeLayer(copyProg, srcTex, color4, layer, cam, viewProj, width, height)
      return
    }

    // copy pass → FBO A（乒乓 A/B 必须独立实例）
    const fboA = getFBO(fboW, fboH, 'ping')
    const fboB = getFBO(fboW, fboH, 'pong')
    const layerOrtho = mat4Ortho(0, fboW, 0, fboH, -10000, 10000)
    if (isPuppet) {
      // puppet 图层的「层内容」= 蒙皮网格，先渲进 FBO A 再走效果链
      drawPuppetToFBO(layer, fboA, fboW, fboH, time)
    } else {
      gl.useProgram(copyProg)
      setBlend('normal')
      gl.bindFramebuffer(gl.FRAMEBUFFER, fboA.fbo)
      gl.viewport(0, 0, fboW, fboH)
      // 先清成透明：fboA/fboB 按尺寸缓存、被所有图层共享，上一层的内容会残留。
      // waterwaves 这类效果做 UV 位移时会采样到 quad 之外，层 FBO 是 CLAMP_TO_EDGE，
      // 于是把残留像素（solid 层缺贴图时回退的 whiteTex 尤为明显）沿边缘拉出来 ——
      // 表现就是水面倾斜幅度稍大就在边上漏出白色底色。
      gl.clearColor(0, 0, 0, 0)
      gl.clear(gl.COLOR_BUFFER_BIT)
      gl.bindVertexArray(vao)
      uploadQuad('layer' + fboW + 'x' + fboH, layerQuad(fboW, fboH))
      gl.activeTexture(gl.TEXTURE0)
      gl.bindTexture(gl.TEXTURE_2D, srcTex)
      gl.uniform1i(copyUni.tex, 0)
      gl.uniform4f(copyUni.color, color4[0], color4[1], color4[2], color4[3])
      gl.uniformMatrix4fv(copyUni.mvp, false, layerOrtho)
      gl.drawArrays(gl.TRIANGLES, 0, 6)
    }

    // 效果链
    let curInput = fboA // 当前主 FBO（asInput）
    let curDraw = fboB // 乒乓目标
    const effectFBOs = new Map()
    let inTargetSeq = false
    let seqInput = fboA
    const flatPasses = []
    const failedEffects = new Set()
    for (const eff of effects) {
      for (const f of eff.fbos || []) {
        if (!effectFBOs.has(f.name)) {
          const scale = f.scale || 1
          effectFBOs.set(f.name, getFBO(Math.max(1, Math.round(fboW / scale)), Math.max(1, Math.round(fboH / scale)), f.name))
        }
      }
      const passes = eff.materialPasses || []
      for (let pi = 0; pi < passes.length; pi++) {
        flatPasses.push({ eff, mp: passes[pi], ov: eff.passes && eff.passes[pi] })
      }
    }
    for (let fi = 0; fi < flatPasses.length; fi++) {
      const { eff, mp, ov } = flatPasses[fi]
      if (failedEffects.has(eff)) continue
      const combos = { ...(mp.combos || {}), ...((ov && ov.combos) || {}) }
      // 本 pass 提供的纹理（material + scene override 合并，用于纹理关联 combo）
      const mpT = mp.textures || []
      const ovT = (ov && ov.textures) || []
      const mergedTex = []
      for (let i = 0; i < Math.max(mpT.length, ovT.length); i++) {
        if (ovT[i] !== undefined && ovT[i] !== null) mergedTex[i] = ovT[i]
        else mergedTex[i] = mpT[i] !== undefined ? mpT[i] : null
      }
      // [we-scene patch] pass 编译失败（缺失公共头/不支持的组合）时跳过**整个效果**，
      // 而不是只跳过这一个 pass。多 pass 效果的后续 pass 依赖前置 pass 写入的中间
      // target FBO（如 cursorripple 的 _rt_EightBuffer2）；只跳过失败的那个会让 combine
      // 之类的 pass 拿着没写过的 FBO 继续跑，把整层刷成纯白/花屏蒙版。
      let progEntry
      try {
        progEntry = await getEffectProgram(mp.shader, combos, mergedTex)
      } catch (e) {
        console.warn('[we-scene] 跳过效果（pass 编译失败）:', mp.shader, (e && e.message) || e)
        failedEffects.add(eff)
        continue
      }
      const prog = progEntry.prog
      const uni = progEntry.uni
      // 目标与输入
      let outFBO
      let passInput
      if (mp.target) {
        if (!inTargetSeq) {
          seqInput = curInput
          inTargetSeq = true
        }
        outFBO = effectFBOs.get(mp.target) || curInput
        passInput = seqInput
      } else {
        inTargetSeq = false
        outFBO = curDraw
        passInput = curInput
      }
      gl.useProgram(prog)
      setBlend(mp.blending || 'normal')
      gl.bindFramebuffer(gl.FRAMEBUFFER, outFBO.fbo)
      gl.viewport(0, 0, outFBO.width, outFBO.height)
      gl.bindVertexArray(vao)
      uploadQuad('pass', PASS_QUAD)
      // 纹理绑定
      const texNames = mp.textures || []
      const maxTex = Math.max(texNames.length, 8)
      const resolutions = new Map()
      const usedUnits = new Set()
      for (let ti = 0; ti < maxTex; ti++) {
        let name = ti < texNames.length ? texNames[ti] : null
        if (ov && ov.textures && ov.textures[ti] !== undefined && ov.textures[ti] !== null) name = ov.textures[ti]
        // bind 覆盖：定义在**每个 pass** 上（effect.json 的 passes[i].bind），
        // 由 effects-parse 存进 mp.binds。此前误读效果级的 eff.binds（恒为 undefined），
        // 使 cursorripple 这类多 pass 效果的 bind 全部失效：combine pass 的槽 1
        // 本该绑 previous（真实画面），落空后取到白纹理 → 整层被刷成纯白蒙版。
        for (const b of mp.binds || []) {
          if (b.index === ti) name = b.name
        }
        // WE 语义：槽 0 为空 = 当前输入 FBO（asInput）；'previous' 同义
        let entry
        if (ti === 0 && (name === null || name === undefined || name === '')) {
          entry = passInput
        } else {
          entry = resolveTextureName(name, passInput, effectFBOs, textures)
        }
        if (name === 'previous') entry = passInput
        if (entry === null) entry = { glTex: whiteTex, width: 1, height: 1, tex: whiteTex }
        const t = entry.fbo ? entry : { tex: entry.glTex || whiteTex, width: entry.width || 1, height: entry.height || 1 }
        gl.activeTexture(gl.TEXTURE0 + ti)
        gl.bindTexture(gl.TEXTURE_2D, t.tex)
        usedUnits.add(ti)
        resolutions.set(ti, [t.width, t.height, t.width, t.height])
      }
      // 系统 uniform
      bindSystemUniforms(uni, layer, time, cam.projW, cam.projH, IDENT_M4, layerOrtho, IDENT_M4, resolutions)
      // 常量（material 名 → uniform 映射）
      bindConstants(uni, { ...(mp.constants || {}), ...((ov && ov.constantshadervalues) || {}) }, progEntry.matMeta)
      gl.drawArrays(gl.TRIANGLES, 0, 6)
      // 更新乒乓
      if (!mp.target) {
        const tmp = curDraw
        curDraw = curInput
        curInput = outFBO
        void tmp
      }
    }
    // 合成
    compositeLayer(compProg, curInput.tex, [1, 1, 1, 1], layer, cam, viewProj, width, height)
  }

  return {
    gl,
    render: renderScene,
    getFBO,
    getEffectProgram,
    progCache,
    shaderResolver,
    whiteTex,
    // 场景切换时清空 shader 相关缓存（避免复用上一个场景的 shader 源/程序）
    resetShaderCaches: function () {
      progCache.clear()
      includeCache.clear()
      if (shaderSrcCache) shaderSrcCache.clear()
    },
    // 运行时切换效果降采样系数（性能档位：0=全质量，1=效果链 ≤ 屏幕尺寸）
    setFboCapFactor: function (v) {
      fboCapFactor = v
    },
    // [we-scene patch] 注入粒子渲染回调：fn(cam, viewProj, width, height, time)
    setParticleRenderer: function (fn) {
      renderParticlesFn = fn
    },
    // [we-scene patch] 注入 puppet 网格绘制回调：fn(layer, mvp, { time })
    // 由宿主用 MDL 渲染器实现；puppet 图层按自身 z 序参与图层循环与效果链。
    setPuppetRenderer: function (fn) {
      puppetDrawFn = fn
    },
    // 释放 WebGL 上下文（loseContext → 浏览器回收全部纹理/FBO/program/buffer）
    dispose: function () {
      try {
        const ext = gl.getExtension('WEBGL_lose_context')
        if (ext) ext.loseContext()
      } catch (e) { /* 忽略：无法强制释放时交给 GC 兜底 */ }
    },
  }
}

function linkProgram(gl, vsSrc, fsSrc) {
  const vs = compile(gl, gl.VERTEX_SHADER, vsSrc)
  const fs = compile(gl, gl.FRAGMENT_SHADER, fsSrc)
  const p = gl.createProgram()
  gl.attachShader(p, vs)
  gl.attachShader(p, fs)
  gl.bindAttribLocation(p, 0, 'a_Position')
  gl.bindAttribLocation(p, 1, 'a_TexCoord')
  gl.linkProgram(p)
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) {
    throw new Error('着色器链接失败: ' + gl.getProgramInfoLog(p))
  }
  return p
}

function compile(gl, type, src) {
  const s = gl.createShader(type)
  gl.shaderSource(s, src)
  gl.compileShader(s)
  if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
    throw new Error('着色器编译失败: ' + gl.getShaderInfoLog(s))
  }
  return s
}

function parseVec3Local(s) {
  const p = String(s).trim().split(/\s+/).map(Number)
  return [p[0] || 0, p[1] || 0, p[2] || 0]
}

export function makeTexture(gl, rgba, width, height, bitmap = null) {
  const tex = gl.createTexture()
  gl.bindTexture(gl.TEXTURE_2D, tex)
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE)
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE)
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR)
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR)
  if (bitmap) {
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, bitmap)
  } else {
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, width, height, 0, gl.RGBA, gl.UNSIGNED_BYTE, rgba)
  }
  return tex
}

export function makeTextureMip(gl, levels, rg88 = false) {
  const tex = gl.createTexture()
  gl.bindTexture(gl.TEXTURE_2D, tex)
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE)
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE)
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR)
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR)
  // 只上传基础级，其余 mip 用 generateMipmap 生成完整链：
  // WE 的 TEXI 容器可能只存部分 mip 级（如 5000×3000 仅 5 级），
  // 不完整的 mip 链在 WebGL 下纹理不完整 → 采样恒黑。
  const lv = levels[0]
  if (lv.bitmap) {
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, lv.bitmap)
  } else if (rg88 && lv.rgba) {
    // RGBA 解码（rgb=G, a=R）→ GL_RG 上传（原始 R,G；shader .r=原始R .g=原始G，与参考实现一致）
    const n = lv.width * lv.height
    const rg = new Uint8Array(n * 2)
    for (let p = 0; p < n; p++) {
      rg[p * 2] = lv.rgba[p * 4 + 3]
      rg[p * 2 + 1] = lv.rgba[p * 4]
    }
    // [we-scene patch] RG8 每像素 2 字节，宽度为奇数时行长不是 4 的倍数；
    // 默认 UNPACK_ALIGNMENT=4 会让 GL 按 4 字节对齐算行距而读越界 →
    // INVALID_OPERATION、纹理留空（该遮罩采样恒黑，效果失真）。改为按字节对齐上传。
    const prevAlign = gl.getParameter(gl.UNPACK_ALIGNMENT)
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1)
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RG8, lv.width, lv.height, 0, gl.RG, gl.UNSIGNED_BYTE, rg)
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, prevAlign)
  } else {
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, lv.width, lv.height, 0, gl.RGBA, gl.UNSIGNED_BYTE, lv.rgba)
  }
  gl.generateMipmap(gl.TEXTURE_2D)
  return tex
}
