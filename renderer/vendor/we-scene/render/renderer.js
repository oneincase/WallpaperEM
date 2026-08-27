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

  // ---------- 合成（层 → 画布） ----------
  function compositeLayer(prog, inputTex, color4, layer, cam, viewProj, width, height) {
    const a = ALIGN[layer.alignment] || [0.5, 0.5]
    const w = layer.size[0] * layer.scale[0]
    const h = layer.size[1] * layer.scale[1]
    let m = mat4Identity()
    m = mat4Translate(m, layer.origin[0], cam.projH - layer.origin[1], layer.origin[2])
    // 旋转：参考实现 y-up 空间 rotate(-angle)，等效 y-down 屏幕 rotate(-angle)（正角度=屏幕逆时针）
    m = mat4RotateZ(m, -layer.angles[2])
    m = mat4Scale(m, w, h, 1)
    // 对象级视差（parallaxDepth）：近景 depth 正值随鼠标位移放大、远景负值反向
    if (layer.parallaxDepth && (layerParallaxScaleX !== 0 || layerParallaxScaleY !== 0)) {
      m = mat4Translate(m, layer.parallaxDepth[0] * layerParallaxScaleX, layer.parallaxDepth[1] * layerParallaxScaleY, 0)
    }
    const mvp = mat4Multiply(viewProj, m)
    const uni = prog === compProg ? compUni : copyUni
    gl.useProgram(prog)
    setBlend('translucent')
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

  // ---------- 渲染入口 ----------
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
      const parOffX = parallaxState.sx * strength * cam.projW * 0.12
      const parOffY = parallaxState.sy * strength * cam.projH * 0.12
      if (parOffX !== 0 || parOffY !== 0) {
        viewProj = mat4Multiply(mat4Translate(mat4Identity(), parOffX, parOffY, 0), viewProj)
      }
      // 对象级视差基准：depth 1.0 的层位移约等于场景级位移
      layerParallaxScaleX = parallaxState.sx * strength * cam.projW * 0.12
      layerParallaxScaleY = parallaxState.sy * strength * cam.projH * 0.12
    }

    for (const layer of scene.layers) {
      if (!layer.visible) continue
      if (layer.isContainer) continue
      if (layer.particle) {
        // [we-scene patch] 粒子图层：由外部 ParticleSystem 模拟+渲染（见 renderer.renderParticles）
        continue
      }
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
    const w = Math.max(1, texObj ? texObj.width : 1)
    const h = Math.max(1, texObj ? texObj.height : 1)
    const color4 = [layer.color[0] * layer.brightness, layer.color[1] * layer.brightness, layer.color[2] * layer.brightness, layer.alpha]
    const effects = (layer.effects || []).filter((e) => e.visible)

    // 效果降采样（性能档位）：fboCapFactor > 0 时效果链 FBO 上限 = 屏幕占比 × 系数（0=全质量）
    let fboW = w
    let fboH = h
    if (fboCapFactor > 0 && cam.projW > 0 && cam.projH > 0) {
      const screenW = layer.size[0] * layer.scale[0] * (width / cam.projW)
      const screenH = layer.size[1] * layer.scale[1] * (height / cam.projH)
      if (screenW > 0 && screenH > 0) {
        const capW = Math.max(64, Math.round(screenW * fboCapFactor))
        const capH = Math.max(64, Math.round(screenH * fboCapFactor))
        if (capW < w) fboW = capW
        if (capH < h) fboH = capH
      }
    }

    // 无效果：直接合成
    if (effects.length === 0) {
      compositeLayer(copyProg, srcTex, color4, layer, cam, viewProj, width, height)
      return
    }

    // copy pass → FBO A（乒乓 A/B 必须独立实例）
    const fboA = getFBO(fboW, fboH, 'ping')
    const fboB = getFBO(fboW, fboH, 'pong')
    const layerOrtho = mat4Ortho(0, fboW, 0, fboH, -10000, 10000)
    gl.useProgram(copyProg)
    setBlend('normal')
    gl.bindFramebuffer(gl.FRAMEBUFFER, fboA.fbo)
    gl.viewport(0, 0, fboW, fboH)
    gl.bindVertexArray(vao)
    uploadQuad('layer' + fboW + 'x' + fboH, layerQuad(fboW, fboH))
    gl.activeTexture(gl.TEXTURE0)
    gl.bindTexture(gl.TEXTURE_2D, srcTex)
    gl.uniform1i(copyUni.tex, 0)
    gl.uniform4f(copyUni.color, color4[0], color4[1], color4[2], color4[3])
    gl.uniformMatrix4fv(copyUni.mvp, false, layerOrtho)
    gl.drawArrays(gl.TRIANGLES, 0, 6)

    // 效果链
    let curInput = fboA // 当前主 FBO（asInput）
    let curDraw = fboB // 乒乓目标
    const effectFBOs = new Map()
    let inTargetSeq = false
    let seqInput = fboA
    const flatPasses = []
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
      const combos = { ...(mp.combos || {}), ...((ov && ov.combos) || {}) }
      // 本 pass 提供的纹理（material + scene override 合并，用于纹理关联 combo）
      const mpT = mp.textures || []
      const ovT = (ov && ov.textures) || []
      const mergedTex = []
      for (let i = 0; i < Math.max(mpT.length, ovT.length); i++) {
        if (ovT[i] !== undefined && ovT[i] !== null) mergedTex[i] = ovT[i]
        else mergedTex[i] = mpT[i] !== undefined ? mpT[i] : null
      }
      // [we-scene patch] 单个效果 pass 编译失败（如缺失公共头/不支持的组合）时跳过该 pass，
      // 而不是让整帧渲染崩溃——主体画面仍可渲染，仅该效果缺失。
      let progEntry
      try {
        progEntry = await getEffectProgram(mp.shader, combos, mergedTex)
      } catch (e) {
        console.warn('[we-scene] 跳过效果 pass:', mp.shader, (e && e.message) || e)
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
        // binds 覆盖
        for (const b of eff.binds || []) {
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
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RG8, lv.width, lv.height, 0, gl.RG, gl.UNSIGNED_BYTE, rg)
  } else {
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, lv.width, lv.height, 0, gl.RGBA, gl.UNSIGNED_BYTE, lv.rgba)
  }
  gl.generateMipmap(gl.TEXTURE_2D)
  return tex
}
