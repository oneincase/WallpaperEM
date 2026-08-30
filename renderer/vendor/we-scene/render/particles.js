// [we-scene patch] 粒子系统：CPU 模拟 + WebGL 实例化 quad 渲染。
//
// 坐标约定：粒子在**图层局部空间**模拟（发射器 origin 即局部原点），渲染时经
// 图层变换（origin / scale / angles.z）落到世界空间；世界坐标 = 像素，y 向下。
//
// 为什么用实例化 quad 而不是 gl.POINTS：
//   1. gl_PointSize 有实现相关的硬上限（多数 GL 实现 64–255），而全库实测
//      sizerandom.max 达 2200（光轴 light_shafts / beam 类），点精灵会被硬截断；
//   2. 点精灵无法旋转，但 54/149 个系统用 rotationrandom、15 个用
//      angularvelocityrandom，不支持旋转会让花瓣/叶片/碎屑看起来是僵死的圆点；
//   3. 点精灵尺寸是屏幕像素，无法随场景缩放/视差正确变化。
//
// 覆盖的 WE 组件（按全库 149 个真实粒子系统的使用频次排序，见 emitter/initializer/
// operator 的实现分支）。未覆盖者静默忽略，不影响其余部分。

const TAU = Math.PI * 2

function rand(a, b) {
  if (a === undefined || a === null) return Math.random()
  if (b === undefined || b === null) return a
  return a + Math.random() * (b - a)
}

// WE 的 exponent：把均匀随机偏向区间下端（exponent>1 时小值更常见）。
// sizerandom 常带 exponent:3 —— 少数大颗粒 + 大量小颗粒，是火花/尘埃的典型分布。
function randExp(a, b, exponent) {
  if (a === undefined || a === null) return Math.random()
  if (b === undefined || b === null) return a
  const t = exponent && exponent !== 1 ? Math.pow(Math.random(), exponent) : Math.random()
  return a + t * (b - a)
}

function parseVec(s, dflt) {
  if (Array.isArray(s)) return [s[0] || 0, s[1] || 0, s[2] || 0]
  if (s === undefined || s === null) return dflt ? dflt.slice() : [0, 0, 0]
  if (typeof s === 'object') return parseVec(s.value, dflt)
  if (typeof s === 'number') return [s, s, s]
  const p = String(s).trim().split(/\s+/).map(Number)
  return [p[0] || 0, p[1] || 0, p[2] || 0]
}

// emitter 的 distancemin/max 既可能是标量（sphererandom 的半径）
// 也可能是向量（boxrandom 的半边长），统一成向量处理。
function parseDist(s) {
  if (s === undefined || s === null) return null
  if (typeof s === 'number') return [s, s, s]
  return parseVec(s)
}

function num(v, dflt) {
  if (v === undefined || v === null) return dflt
  if (typeof v === 'object') return num(v.value, dflt)
  const n = Number(v)
  return Number.isFinite(n) ? n : dflt
}

// ---------- 噪声（turbulence / turbulentvelocityrandom / remapvalue） ----------

function hash3(x, y, z) {
  const n = Math.sin(x * 127.1 + y * 311.7 + z * 74.7) * 43758.5453
  return n - Math.floor(n)
}
function vnoise3(x, y, z) {
  const ix = Math.floor(x)
  const iy = Math.floor(y)
  const iz = Math.floor(z)
  const fx = x - ix
  const fy = y - iy
  const fz = z - iz
  const sx = fx * fx * (3 - 2 * fx)
  const sy = fy * fy * (3 - 2 * fy)
  const sz = fz * fz * (3 - 2 * fz)
  const l = (a, b, t) => a + (b - a) * t
  const c00 = l(hash3(ix, iy, iz), hash3(ix + 1, iy, iz), sx)
  const c10 = l(hash3(ix, iy + 1, iz), hash3(ix + 1, iy + 1, iz), sx)
  const c01 = l(hash3(ix, iy, iz + 1), hash3(ix + 1, iy, iz + 1), sx)
  const c11 = l(hash3(ix, iy + 1, iz + 1), hash3(ix + 1, iy + 1, iz + 1), sx)
  return l(l(c00, c10, sy), l(c01, c11, sy), sz)
}
function fbm3(x, y, z, oct) {
  let v = 0
  let amp = 0.5
  let f = 1
  let norm = 0
  for (let i = 0; i < oct; i++) {
    v += amp * vnoise3(x * f, y * f, z * f)
    norm += amp
    amp *= 0.5
    f *= 2
  }
  return v / (norm || 1)
}
// 返回 -1..1 的三维噪声向量（各分量取不同偏移，互不相关）
function noiseVec3(x, y, z, oct) {
  return [
    fbm3(x, y, z, oct) * 2 - 1,
    fbm3(x + 31.7, y + 11.3, z + 57.1, oct) * 2 - 1,
    fbm3(x + 73.9, y + 92.1, z + 13.7, oct) * 2 - 1,
  ]
}

// ---------- 粒子 ----------

class Particle {
  constructor() {
    this.alive = false
    // 位置 / 速度（图层局部空间，像素）
    this.x = this.y = this.z = 0
    this.vx = this.vy = this.vz = 0
    // 振荡类算子需要「未受振荡影响的基准位置」，否则振荡会与运动互相累加发散
    this.bx = this.by = this.bz = 0
    this.age = 0
    this.life = 1
    this.size = 1
    this.baseSize = 1
    this.rot = 0
    this.rotVel = 0
    this.r = this.g = this.b = 1
    this.baseR = this.baseG = this.baseB = 1
    this.alpha = 1
    this.baseAlpha = 1
    this.seed = 0
    this.frame = 0
    // 每粒子固化的随机相位：振荡类算子的 min/max 必须在**生成时**取一次，
    // 若每帧重取，随机相位会让粒子每帧跳到不同位置（表现为剧烈抖动）。
    this.oscPhase = 0
    this.oscFreq = 0
    this.oscAmp = 0
    this.oscAPhase = 0
    this.oscAFreq = 0
    this.oscSPhase = 0
    this.oscSFreq = 0
    this.turbSpeed = 0
    this.turbPhase = 0
    // spritetrail / ropetrail 需要历史轨迹
    this.trail = null
  }
}

export class ParticleSystem {
  constructor(gl, model, override, layer) {
    this.gl = gl
    this.model = model || {}
    this.override = override || {}
    this.layer = layer || null

    // 图层变换：粒子在局部空间模拟，渲染时用它落到世界空间。
    // WE 语义：图层 origin 是发射器世界位置，scale 缩放整个系统（尺寸与速度同步缩放），
    // angles.z 旋转发射方向。忽略它们会让所有粒子堆在世界原点。
    const lo = layer && layer.origin ? layer.origin : [0, 0, 0]
    const ls = layer && layer.scale ? layer.scale : [1, 1, 1]
    const la = layer && layer.angles ? layer.angles : [0, 0, 0]
    this.originX = lo[0] || 0
    this.originY = lo[1] || 0
    this.originZ = lo[2] || 0
    this.scaleX = ls[0] === 0 ? 1 : ls[0]
    this.scaleY = ls[1] === 0 ? 1 : ls[1]
    this.angleZ = ((la[2] || 0) * Math.PI) / 180
    // 精灵的非等比拉伸：WE 里图层 scale 直接作用于精灵 quad，x/y 不等时精灵被拉长。
    // 全库 92 个带 scale 的粒子层有 36 个是非等比的（如 light_shafts_1 的 22.6/12.2、
    // fog1 的 10/1、Splash 的 1/0.15），它们靠拉伸把圆形贴图变成光柱/雨丝/横向雾带。
    // 若只取平均值做等比缩放，这些系统会变成巨大的圆斑糊住半个屏幕。
    // 故：位置用 scaleX/scaleY 各自缩放，精灵尺寸取较小轴为基准、较大轴作为拉伸比。
    const asx = Math.abs(this.scaleX)
    const asy = Math.abs(this.scaleY)
    this.sysScale = Math.min(asx, asy) || 1
    // 精灵 quad 的宽高拉伸倍率（相对 sysScale）
    this.spriteStretchX = asx / this.sysScale
    this.spriteStretchY = asy / this.sysScale

    this.maxCount = Math.max(1, Math.min(20000, num(this.model.maxcount, 100)))
    // 发射累加器改为按发射器各存一份（见 _step），此字段仅保留以防外部引用
    this.simTime = 0
    this.paused = false
    this.texture = null
    this.material = null
    this.blend = 'translucent'
    this.opacityMul = 1
    this.lifetimeMul = 1
    this.visible = true
    this._ready = false
    this._prog = null
    this._vbuf = null
    this._quadBuf = null
    this._vao = null
    this._data = null
    // 序列帧（sprite sheet）。优先用贴图 TEXS 段里的**真实帧矩形**；
    // 只有在贴图没有 TEXS 时才退回按 sequencemultiplier 猜 N×N 方格
    // （猜测对横排/竖排的 sheet 是错的，会采样到跨帧的错位图块）。
    this.sequenceMul = Math.max(1, Math.round(num(this.model.sequencemultiplier, 1)))
    this.animationMode = this.model.animationmode || null
    this.texFrames = null // 由 setTexture 填充
    this.frameCount = this.sequenceMul * this.sequenceMul
    // 精灵 quad 的贴图宽高比（setTexture 按实际贴图/单帧尺寸覆盖）
    this.texAspectX = 1
    this.texAspectY = 1
    // 控制点（controlpointattract / mapsequencearoundcontrolpoint 用）
    // locktopointer 的控制点跟随鼠标，由宿主每帧调 setPointer() 更新
    this.controlPoints = (this.model.controlpoint || []).map((cp) => ({
      id: num(cp.id, 0),
      lockToPointer: !!cp.locktopointer,
      offset: parseVec(cp.offset),
      // 世界空间的当前位置（局部空间）
      x: 0,
      y: 0,
      z: 0,
    }))
    this.pointer = null // {x,y} 局部空间鼠标位置

    this._ov = {}
    this._applyOverride()

    // starttime：WE 用它预热模拟，让首帧就有铺满的粒子（雪/雾不会从空屏渐入）
    this.startTime = Math.max(0, Math.min(30, num(this.model.starttime, 0)))
    this._warmed = false

    // 预解析算子/初始化器：每帧遍历 JSON 并做 name 比较在 100k 粒子下开销可观
    this._compile()
  }

  _applyOverride() {
    const ov = this.override || {}
    for (const k of Object.keys(ov)) {
      if (k === 'id') continue
      const raw = ov[k]
      const val = raw && typeof raw === 'object' && 'value' in raw ? raw.value : raw
      if (k === 'count') {
        // count 是**粒子数量倍率**：既缩上限，也按同比例缩发射率。
        // 只缩 maxcount 是错的 —— 稳态存活数 ≈ rate × lifetime，不动 rate 的话
        // 调小 count 只会让粒子更快被回收、密度几乎不变，与编辑器里"数量"滑块的语义相反。
        // 全库 91 个带 count override 的层实测：只缩上限有 46% 的层稳态超出 maxcount
        // 被硬截断（2370927443 的 Rain_secondary 需要 4500 颗却只有 1300 的上限，
        // 于是 1300 颗全挤在 6%×20% 的小窗格里，密度是主雨的 900 倍 ——
        // 表现为「画面正中一块过密的长方形雨」）；同时缩发射率则降到 13%。
        const base = num(this.model.maxcount, 100)
        const mul = Number(val)
        const m = Number.isFinite(mul) ? mul : 1
        this.maxCount = Math.max(1, Math.min(20000, Math.round(base * m)))
        this._ov.countMul = m
      } else if (k === 'alpha') {
        this.opacityMul = Number(val)
        if (!Number.isFinite(this.opacityMul)) this.opacityMul = 1
      } else if (k === 'color' || k === 'colorn') {
        // color 是 0–255，colorn 是 0–1 归一化。此前一律 /255 会把
        // colorn（如 "1 0.968 0.149"）压成近黑，粒子全部消失在暗背景里。
        const v = parseVec(val)
        this._ov.color = k === 'colorn' ? v : [v[0] / 255, v[1] / 255, v[2] / 255]
      } else if (k === 'size') {
        this._ov.size = Number(val) || 1
      } else if (k === 'rate') {
        // rate 是**倍率**而非绝对值（实测 32 处 override 全在 0.19–4.84 区间，
        // 且与 emitter.rate 一起出现：如 rate=250 的 emitter 配 override 0.32 → 80/s）。
        // 当成绝对值会把 250/s 的发射器压成 0.32/s，粒子几乎不出现。
        this._ov.rateMul = Number(val)
        if (!Number.isFinite(this._ov.rateMul)) this._ov.rateMul = 1
      } else if (k === 'speed') {
        this._ov.speed = Number(val) || 1
      } else if (k === 'lifetime') {
        this.lifetimeMul = Number(val) || 1
      } else if (k === 'brightness') {
        this._ov.brightness = Number(val) || 1
      } else {
        this._ov[k] = val
      }
    }
    this.pool = []
    for (let i = 0; i < this.maxCount; i++) this.pool.push(new Particle())
  }

  // 把 model 的 JSON 描述编译成扁平参数，避免每帧字符串比较
  _compile() {
    const m = this.model
    this.emitters = (m.emitter || []).map((e) => ({
      kind: e.name === 'boxrandom' ? 'box' : 'sphere',
      origin: parseVec(e.origin),
      directions: parseVec(e.directions, [1, 1, 1]),
      distanceMin: parseDist(e.distancemin),
      distanceMax: parseDist(e.distancemax),
      sign: e.sign ? parseVec(e.sign) : null,
      rate: num(e.rate, 0),
      // rate 与 instantaneous 都缺省 = 「维持池满」的常驻场（尘埃/灰烬/星空）。
      // 全库有 21 个这样的发射器（dust_motes_0 / ember_small / Shooting_Star_01）。
      // 若按 rate=0 处理，这些系统一颗粒子都不会出现（dust motes 整层消失）。
      //
      // 但带 audioprocessingmode 的发射器例外：它的发射量由系统音频驱动，
      // 本渲染器没有音频输入（/audio-stream 未复刻），静音时 WE 的发射量趋近 0。
      // 若也按「维持池满」处理，会把音频响应的星星填满并叠成一片过曝
      // （2419444134 的 reactive Stars 实测 15% 像素过曝）。故静音时按不发射处理。
      audioDriven: e.audioprocessingmode !== undefined,
      fill: e.rate === undefined && e.instantaneous === undefined && e.audioprocessingmode === undefined,
      instantaneous: num(e.instantaneous, 0),
      speedMin: num(e.speedmin, 0),
      speedMax: num(e.speedmax, 0),
      _burst: false,
    }))

    const init = m.initializer || []
    this.init = {
      life: null,
      size: null,
      color: null,
      alpha: null,
      velocity: null,
      rotation: null,
      angularVelocity: null,
      turbulentVelocity: null,
    }
    for (const z of init) {
      switch (z.name) {
        case 'lifetimerandom':
          this.init.life = { min: num(z.min, 1), max: num(z.max, 1), exp: num(z.exponent, 1) }
          break
        case 'sizerandom':
          this.init.size = { min: num(z.min, 1), max: num(z.max, 1), exp: num(z.exponent, 1) }
          break
        case 'colorrandom':
          // WE 的 colorrandom 是 0–255；min/max 不保证有序（实测大量 min>max）
          this.init.color = { min: parseVec(z.min, [255, 255, 255]), max: parseVec(z.max, [255, 255, 255]) }
          break
        case 'alpharandom':
          this.init.alpha = { min: num(z.min, 1), max: num(z.max, 1) }
          break
        case 'velocityrandom':
          this.init.velocity = { min: parseVec(z.min), max: parseVec(z.max) }
          break
        case 'rotationrandom':
          this.init.rotation = { min: parseVec(z.min), max: parseVec(z.max, [0, 0, 0]), exp: num(z.exponent, 1) }
          break
        case 'angularvelocityrandom':
          this.init.angularVelocity = { min: parseVec(z.min), max: parseVec(z.max), exp: num(z.exponent, 1) }
          break
        case 'turbulentvelocityrandom':
          // 用噪声场给初速度方向（雪/雾的自然扩散感来源）
          this.init.turbulentVelocity = {
            scale: num(z.scale, 0.1),
            offset: num(z.offset, 0),
            speedMin: num(z.speedmin, 0),
            speedMax: num(z.speedmax, 0),
            phaseMax: num(z.phasemax, 0),
            timeScale: num(z.timescale, 0),
          }
          break
        default:
          break
      }
    }

    const ops = m.operator || []
    this.ops = {
      movement: null,
      angularMovement: null,
      alphaFade: null,
      alphaChange: null,
      sizeChange: null,
      colorChange: null,
      turbulence: [],
      oscAlpha: null,
      oscSize: null,
      oscPos: null,
      attract: [],
      vortex: [],
      remap: [],
    }
    for (const o of ops) {
      switch (o.name) {
        case 'movement':
          this.ops.movement = { gravity: parseVec(o.gravity), drag: num(o.drag, 0) }
          break
        case 'angularmovement':
          this.ops.angularMovement = { force: parseVec(o.force), drag: num(o.drag, 0) }
          break
        case 'alphafade':
          // WE 语义：fadeintime / fadeouttime 是**生命周期比例**（0–1），不是秒
          this.ops.alphaFade = { fadeIn: num(o.fadeintime, 0), fadeOut: num(o.fadeouttime, 1) }
          break
        case 'alphachange':
          this.ops.alphaChange = {
            startTime: num(o.starttime, 0),
            endTime: num(o.endtime, 1),
            startValue: num(o.startvalue, 1),
            endValue: num(o.endvalue, 0),
          }
          break
        case 'sizechange':
          this.ops.sizeChange = {
            startTime: num(o.starttime, 0),
            endTime: num(o.endtime, 1),
            startValue: num(o.startvalue, 1),
            endValue: num(o.endvalue, 1),
          }
          break
        case 'colorchange':
          this.ops.colorChange = {
            startTime: num(o.starttime, 0),
            endTime: num(o.endtime, 1),
            startValue: o.startvalue !== undefined ? parseVec(o.startvalue) : null,
            endValue: parseVec(o.endvalue, [1, 1, 1]),
          }
          break
        case 'turbulence':
          this.ops.turbulence.push({
            scale: num(o.scale, 0.01),
            speedMin: num(o.speedmin, 0),
            speedMax: num(o.speedmax, 0),
            timeScale: num(o.timescale, 1),
            phaseMin: num(o.phasemin, 0),
            phaseMax: num(o.phasemax, 0),
            mask: o.mask !== undefined ? parseVec(o.mask, [1, 1, 1]) : [1, 1, 1],
          })
          break
        case 'oscillatealpha':
          this.ops.oscAlpha = {
            freqMin: num(o.frequencymin, num(o.frequencymax, 1)),
            freqMax: num(o.frequencymax, 1),
            scaleMin: num(o.scalemin, 0),
            scaleMax: num(o.scalemax, 1),
            phaseMax: num(o.phasemax, TAU),
          }
          break
        case 'oscillatesize':
          this.ops.oscSize = {
            freqMin: num(o.frequencymin, num(o.frequencymax, 1)),
            freqMax: num(o.frequencymax, num(o.frequencymin, 1)),
            scaleMin: num(o.scalemin, 1),
            scaleMax: num(o.scalemax, 1),
            phaseMax: num(o.phasemax, TAU),
          }
          break
        case 'oscillateposition':
          this.ops.oscPos = {
            freqMin: num(o.frequencymin, num(o.frequencymax, 0.5)),
            freqMax: num(o.frequencymax, 0.5),
            scaleMin: num(o.scalemin, 0),
            scaleMax: num(o.scalemax, 1),
            phaseMin: num(o.phasemin, 0),
            phaseMax: num(o.phasemax, TAU),
            mask: o.mask !== undefined ? parseVec(o.mask, [1, 1, 0]) : [1, 1, 0],
          }
          break
        case 'controlpointattract':
          // scale<0 = 排斥（实测 -10000 很常见，是"鼠标推开粒子"效果）
          this.ops.attract.push({
            cp: num(o.controlpoint, 0),
            origin: parseVec(o.origin),
            scale: num(o.scale, 0),
            threshold: num(o.threshold, 0),
          })
          break
        case 'vortex':
          this.ops.vortex.push({
            axis: parseVec(o.axis, [0, 0, 1]),
            offset: parseVec(o.offset),
            distanceInner: num(o.distanceinner, 0),
            distanceOuter: num(o.distanceouter, 0),
            speedInner: num(o.speedinner, 0),
            speedOuter: num(o.speedouter, 0),
          })
          break
        case 'remapvalue':
          this.ops.remap.push({
            output: o.output || 'velocity',
            rangeMin: parseVec(o.outputrangemin),
            rangeMax: parseVec(o.outputrangemax),
            fn: o.transformfunction || 'simplexnoise',
            inputScale: num(o.transforminputscale, 1),
          })
          break
        default:
          break
      }
    }

    // 渲染器：sprite（默认）/ spritetrail / rope / ropetrail
    const rlist = Array.isArray(this.model.renderer) ? this.model.renderer : this.model.renderer ? [this.model.renderer] : []
    this.renderers = rlist.map((r) => ({
      kind: (r && r.name) || 'sprite',
      length: num(r && r.length, 0),
      maxLength: num(r && r.maxlength, 0),
      minLength: num(r && r.minlength, 0),
      subdivision: num(r && r.subdivision, 1),
      orientation: (r && r.orientation) || null,
    }))
    if (!this.renderers.length) this.renderers = [{ kind: 'sprite', length: 0, maxLength: 0, minLength: 0, subdivision: 1, orientation: null }]
    // 轨迹类渲染器：粒子要记录历史位置
    const tr = this.renderers.find((r) => r.kind === 'spritetrail' || r.kind === 'ropetrail')
    this.trailCfg = tr || null
    if (this.trailCfg) {
      // length 是每段的生命比例，maxlength 上限段数；给个合理的采样点数
      const segs = Math.max(2, Math.min(16, Math.round(this.trailCfg.maxLength || 6)))
      this.trailSegments = segs
      for (const p of this.pool) p.trail = new Float32Array(segs * 3)
    }
  }

  setModel(model) {
    this.model = model
    this._compile()
  }

  setMaterial(mat) {
    this.material = mat
    const pass = mat && mat.passes && mat.passes[0]
    this.blend = (pass && pass.blending) || 'translucent'
  }

  // 注入贴图 {glTex, width, height, frames?}
  // frames 为 TEXS 序列帧表（[{x,y,width,height}]，像素坐标），有则据此切图。
  setTexture(tex) {
    this.texture = tex
    this._ready = !!tex
    const list = tex && tex.frames && tex.frames.length ? tex.frames : null
    if (list && tex.width > 0 && tex.height > 0) {
      // 帧矩形归一化为 uv（着色器按 uvScale/uvOffset 直接采样，无需知道网格结构）
      this.texFrames = list.map((f) => ({
        ou: f.x / tex.width,
        ov: f.y / tex.height,
        su: f.width / tex.width,
        sv: f.height / tex.height,
      }))
      this.frameCount = this.texFrames.length
    } else {
      this.texFrames = null
      this.frameCount = this.sequenceMul * this.sequenceMul
    }
    // 精灵 quad 的宽高比取自**贴图（单帧）本身**：WE 的粒子精灵是带纹理比例的四边形。
    // size 控制的是精灵的**长边**，短边按贴图比例压缩：
    // 全库 72 个非方形贴图的粒子层实测，按"size=长边"只有 4% 的精灵超出屏幕 1.5 倍，
    // 按"size=短边"则有 18%（光轴会算出 36545px、雨滴 6400px 这种荒谬尺寸）。
    // 这样光轴/雨丝才是细长条：高度由 size 决定、宽度被压到 1/4。
    let aw = 1
    let ah = 1
    if (this.texFrames && list) {
      aw = list[0].width || 1
      ah = list[0].height || 1
    } else if (tex && tex.width > 0 && tex.height > 0) {
      aw = tex.width
      ah = tex.height
    }
    const longSide = Math.max(aw, ah) || 1
    this.texAspectX = aw / longSide
    this.texAspectY = ah / longSide
    this._frameData = undefined // 帧表变化时重建 uniform 缓存
  }

  setVisible(v) {
    this.visible = v
  }

  // 宿主每帧提供鼠标位置（世界像素）；转到局部空间供控制点使用
  setPointer(worldX, worldY) {
    const dx = worldX - this.originX
    const dy = worldY - this.originY
    const c = Math.cos(-this.angleZ)
    const s = Math.sin(-this.angleZ)
    this.pointer = {
      x: (dx * c - dy * s) / (this.scaleX || 1),
      y: (dx * s + dy * c) / (this.scaleY || 1),
    }
  }

  // ---------- 生成 ----------

  spawn(em) {
    // 线性扫描找空位在 maxcount 大时是热点；用游标做环形查找
    const pool = this.pool
    const n = pool.length
    let p = null
    let cur = this._cursor || 0
    for (let i = 0; i < n; i++) {
      const q = pool[(cur + i) % n]
      if (!q.alive) {
        p = q
        this._cursor = (cur + i + 1) % n
        break
      }
    }
    if (!p) return

    p.alive = true
    p.age = 0
    p.seed = Math.random()
    p.rot = 0
    p.rotVel = 0
    p.vx = p.vy = p.vz = 0
    p.frame = 0

    // ---- 发射位置 ----
    const o = em.origin
    if (em.kind === 'box') {
      const d = em.distanceMax || [0, 0, 0]
      const dmin = em.distanceMin
      // boxrandom：在 ±distancemax 的盒内均匀取点（distancemin 存在时作为偏移基准）
      p.x = o[0] + rand(-d[0], d[0]) + (dmin ? dmin[0] : 0)
      p.y = o[1] + rand(-d[1], d[1]) + (dmin ? dmin[1] : 0)
      p.z = o[2] + rand(-d[2], d[2]) + (dmin ? dmin[2] : 0)
    } else {
      // sphererandom：球壳内随机方向 × [distancemin, distancemax] 半径，
      // 再按 directions 各轴缩放（"1 0.03 0" = 几乎水平的一条线）
      const dir = em.directions
      const rmin = em.distanceMin ? em.distanceMin[0] : 0
      const rmax = em.distanceMax ? em.distanceMax[0] : 0
      // 球面均匀采样
      const u = Math.random() * 2 - 1
      const th = Math.random() * TAU
      const sq = Math.sqrt(Math.max(0, 1 - u * u))
      let nx = sq * Math.cos(th)
      let ny = sq * Math.sin(th)
      let nz = u
      if (em.sign) {
        // sign 非 0 的轴强制取正（半球发射）
        if (em.sign[0]) nx = Math.abs(nx) * Math.sign(em.sign[0])
        if (em.sign[1]) ny = Math.abs(ny) * Math.sign(em.sign[1])
        if (em.sign[2]) nz = Math.abs(nz) * Math.sign(em.sign[2])
      }
      const r = rand(rmin, rmax)
      p.x = o[0] + nx * r * dir[0]
      p.y = o[1] + ny * r * dir[1]
      p.z = o[2] + nz * r * dir[2]
      // 发射器自身的 speedmin/max：沿发射方向的初速
      if (em.speedMax || em.speedMin) {
        const sp = rand(em.speedMin, em.speedMax)
        p.vx += nx * sp
        p.vy += ny * sp
        p.vz += nz * sp
      }
    }

    // ---- 初始化器 ----
    const I = this.init
    const speedMul = this._ov.speed || 1
    const sizeMul = this._ov.size || 1

    p.life = I.life ? Math.max(0.001, randExp(I.life.min, I.life.max, I.life.exp) * this.lifetimeMul) : 1
    p.baseSize = I.size ? randExp(I.size.min, I.size.max, I.size.exp) * sizeMul : sizeMul
    p.size = p.baseSize

    if (I.color) {
      const mn = I.color.min
      const mx = I.color.max
      // min/max 无序（实测大量 min>max），逐分量取区间
      const t = Math.random()
      p.baseR = (mn[0] + (mx[0] - mn[0]) * t) / 255
      p.baseG = (mn[1] + (mx[1] - mn[1]) * t) / 255
      p.baseB = (mn[2] + (mx[2] - mn[2]) * t) / 255
    } else {
      p.baseR = p.baseG = p.baseB = 1
    }
    // instanceoverride 的 color/colorn 覆盖初始化器（用户属性调色）
    if (this._ov.color) {
      p.baseR = this._ov.color[0]
      p.baseG = this._ov.color[1]
      p.baseB = this._ov.color[2]
    }
    p.r = p.baseR
    p.g = p.baseG
    p.b = p.baseB

    p.baseAlpha = (I.alpha ? rand(I.alpha.min, I.alpha.max) : 1) * this.opacityMul
    p.alpha = p.baseAlpha

    if (I.velocity) {
      p.vx += rand(I.velocity.min[0], I.velocity.max[0]) * speedMul
      p.vy += rand(I.velocity.min[1], I.velocity.max[1]) * speedMul
      p.vz += rand(I.velocity.min[2], I.velocity.max[2]) * speedMul
    }
    if (I.turbulentVelocity) {
      const tv = I.turbulentVelocity
      const sp = rand(tv.speedMin, tv.speedMax) * speedMul
      const t = this.simTime * (tv.timeScale || 0)
      const nv = noiseVec3(p.x * tv.scale + tv.offset, p.y * tv.scale + tv.offset, t + p.seed * (tv.phaseMax || 0), 3)
      p.vx += nv[0] * sp
      p.vy += nv[1] * sp
      p.vz += nv[2] * sp
    }
    if (I.rotation) {
      // 只用 z 分量（2D 精灵绕视线轴旋转）
      p.rot = rand(I.rotation.min[2], I.rotation.max[2])
    }
    if (I.angularVelocity) {
      p.rotVel = rand(I.angularVelocity.min[2], I.angularVelocity.max[2])
    }

    // 振荡相位/频率在生成时固化一次（每帧重取会导致抖动）
    const O = this.ops
    if (O.oscPos) {
      p.oscFreq = rand(O.oscPos.freqMin, O.oscPos.freqMax)
      p.oscAmp = rand(O.oscPos.scaleMin, O.oscPos.scaleMax)
      p.oscPhase = rand(O.oscPos.phaseMin, O.oscPos.phaseMax) + p.seed * TAU
    }
    if (O.oscAlpha) {
      p.oscAFreq = rand(O.oscAlpha.freqMin, O.oscAlpha.freqMax)
      p.oscAPhase = Math.random() * (O.oscAlpha.phaseMax || TAU)
    }
    if (O.oscSize) {
      p.oscSFreq = rand(O.oscSize.freqMin, O.oscSize.freqMax)
      p.oscSPhase = Math.random() * (O.oscSize.phaseMax || TAU)
    }
    if (O.turbulence.length) {
      const t0 = O.turbulence[0]
      p.turbSpeed = rand(t0.speedMin, t0.speedMax)
      p.turbPhase = rand(t0.phaseMin, t0.phaseMax)
    }

    // 序列帧：randomframe 随机起始帧，否则按生命进度推进
    if (this.frameCount > 1) {
      p.frame = this.animationMode === 'randomframe' ? Math.floor(Math.random() * this.frameCount) : 0
    }

    p.bx = p.x
    p.by = p.y
    p.bz = p.z
    if (p.trail) {
      // 轨迹初始化为当前位置，避免新粒子拖出一条从原点来的长尾
      for (let i = 0; i < p.trail.length; i += 3) {
        p.trail[i] = p.x
        p.trail[i + 1] = p.y
        p.trail[i + 2] = p.z
      }
    }
  }

  // ---------- 每粒子更新 ----------

  updateParticle(p, dt) {
    p.age += dt
    if (p.age >= p.life) {
      p.alive = false
      return
    }
    const lt = p.age / p.life // 生命进度 0..1
    const O = this.ops

    // --- 力 / 速度 ---
    if (O.movement) {
      p.vx += O.movement.gravity[0] * dt
      p.vy += O.movement.gravity[1] * dt
      p.vz += O.movement.gravity[2] * dt
      if (O.movement.drag > 0) {
        // 指数衰减，dt 无关（线性 1-drag*dt 在大 dt 下会变成负数使速度反向）
        const d = Math.exp(-O.movement.drag * dt)
        p.vx *= d
        p.vy *= d
        p.vz *= d
      }
    }
    if (O.angularMovement) {
      p.rotVel += (O.angularMovement.force[2] || 0) * dt
      if (O.angularMovement.drag > 0) p.rotVel *= Math.exp(-O.angularMovement.drag * dt)
    }

    // 湍流：噪声场加速度（雪花飘、烟雾扰动）
    for (const t of O.turbulence) {
      const ts = t.timeScale || 1
      const nv = noiseVec3(
        p.bx * t.scale,
        p.by * t.scale,
        this.simTime * t.scale * ts + p.turbPhase,
        3
      )
      p.vx += nv[0] * p.turbSpeed * t.mask[0] * dt
      p.vy += nv[1] * p.turbSpeed * t.mask[1] * dt
      p.vz += nv[2] * p.turbSpeed * t.mask[2] * dt
    }

    // 控制点吸引/排斥（scale<0 = 排斥；threshold 是作用半径）
    for (const a of O.attract) {
      const cp = this._cpPos(a.cp)
      if (!cp) continue
      const dx = cp[0] + a.origin[0] - p.x
      const dy = cp[1] + a.origin[1] - p.y
      const dist = Math.hypot(dx, dy)
      if (dist < 1e-3) continue
      if (a.threshold > 0 && dist > a.threshold) continue
      // 力随距离衰减（1 - d/threshold），threshold 内平滑过渡到 0
      const falloff = a.threshold > 0 ? 1 - dist / a.threshold : 1
      const f = (a.scale * falloff * dt) / Math.max(1, dist)
      p.vx += dx * f
      p.vy += dy * f
    }

    // 涡流：绕轴的切向速度
    for (const v of O.vortex) {
      const dx = p.x - v.offset[0]
      const dy = p.y - v.offset[1]
      const dist = Math.hypot(dx, dy)
      if (dist < 1e-3) continue
      const span = v.distanceOuter - v.distanceInner
      const k = span > 0 ? Math.min(1, Math.max(0, (dist - v.distanceInner) / span)) : 0
      const speed = v.speedInner + (v.speedOuter - v.speedInner) * k
      // 切向（绕 z 轴）
      p.vx += (-dy / dist) * speed * dt
      p.vy += (dx / dist) * speed * dt
    }

    // remapvalue：噪声重映射到速度/速率
    for (const rm of O.remap) {
      const s = rm.inputScale || 1
      const nv = noiseVec3(p.bx * 0.01 * s, p.by * 0.01 * s, this.simTime * 0.1, rm.fn === 'fbmnoise' ? 4 : 2)
      if (rm.output === 'velocity') {
        for (let i = 0; i < 3; i++) {
          const t = (nv[i] + 1) / 2
          const val = rm.rangeMin[i] + (rm.rangeMax[i] - rm.rangeMin[i]) * t
          if (i === 0) p.vx = val
          else if (i === 1) p.vy = val
          else p.vz = val
        }
      } else if (rm.output === 'speed') {
        const t = (nv[0] + 1) / 2
        const mul = rm.rangeMin[0] + (rm.rangeMax[0] - rm.rangeMin[0]) * t
        const len = Math.hypot(p.vx, p.vy) || 1
        p.vx = (p.vx / len) * mul
        p.vy = (p.vy / len) * mul
      }
    }

    // --- 积分 ---
    p.bx += p.vx * dt
    p.by += p.vy * dt
    p.bz += p.vz * dt
    p.rot += p.rotVel * dt

    // 振荡位移叠加在基准位置上（不回写基准，否则与运动互相累加发散）
    p.x = p.bx
    p.y = p.by
    p.z = p.bz
    if (O.oscPos) {
      const ph = this.simTime * p.oscFreq * TAU + p.oscPhase
      const m = O.oscPos.mask
      p.x += Math.sin(ph) * p.oscAmp * m[0]
      p.y += Math.cos(ph) * p.oscAmp * m[1]
      p.z += Math.sin(ph * 0.7) * p.oscAmp * m[2]
    }

    // --- 尺寸 ---
    let size = p.baseSize
    if (O.sizeChange) {
      const sc = O.sizeChange
      const span = sc.endTime - sc.startTime
      const k = span > 0 ? Math.min(1, Math.max(0, (lt - sc.startTime) / span)) : lt >= sc.startTime ? 1 : 0
      size *= sc.startValue + (sc.endValue - sc.startValue) * k
    }
    if (O.oscSize) {
      const ph = this.simTime * p.oscSFreq * TAU + p.oscSPhase
      const os = O.oscSize
      const mid = (os.scaleMin + os.scaleMax) / 2
      const amp = (os.scaleMax - os.scaleMin) / 2
      size *= mid + Math.sin(ph) * amp
    }
    p.size = size

    // --- 颜色 ---
    if (O.colorChange) {
      const cc = O.colorChange
      const span = cc.endTime - cc.startTime
      const k = span > 0 ? Math.min(1, Math.max(0, (lt - cc.startTime) / span)) : lt >= cc.startTime ? 1 : 0
      // startvalue 缺省时以粒子自身初始色为起点
      const s0 = cc.startValue || [p.baseR, p.baseG, p.baseB]
      p.r = s0[0] + (cc.endValue[0] - s0[0]) * k
      p.g = s0[1] + (cc.endValue[1] - s0[1]) * k
      p.b = s0[2] + (cc.endValue[2] - s0[2]) * k
    }

    // --- 透明度 ---
    let a = p.baseAlpha
    if (O.alphaFade) {
      // fadeintime / fadeouttime 是生命比例：[0,fadeIn] 淡入，[fadeOut,1] 淡出
      const fi = O.alphaFade.fadeIn
      const fo = O.alphaFade.fadeOut
      if (fi > 0 && lt < fi) a *= lt / fi
      if (fo < 1 && lt > fo) a *= Math.max(0, (1 - lt) / (1 - fo))
    } else if (!O.alphaChange) {
      // 无任何 alpha 算子时给一条温和的默认包络，避免粒子突然出现/消失
      if (lt < 0.1) a *= lt / 0.1
      else if (lt > 0.85) a *= (1 - lt) / 0.15
    }
    if (O.alphaChange) {
      const ac = O.alphaChange
      const span = ac.endTime - ac.startTime
      const k = span > 0 ? Math.min(1, Math.max(0, (lt - ac.startTime) / span)) : lt >= ac.startTime ? 1 : 0
      a *= ac.startValue + (ac.endValue - ac.startValue) * k
    }
    if (O.oscAlpha) {
      const ph = this.simTime * p.oscAFreq * TAU + p.oscAPhase
      const oa = O.oscAlpha
      const mid = (oa.scaleMin + oa.scaleMax) / 2
      const amp = (oa.scaleMax - oa.scaleMin) / 2
      a *= mid + Math.sin(ph) * amp
    }
    p.alpha = Math.max(0, a)

    // 序列帧推进（非 randomframe 时按生命进度走完一轮）
    if (this.frameCount > 1 && this.animationMode !== 'randomframe') {
      p.frame = Math.min(this.frameCount - 1, Math.floor(lt * this.frameCount))
    }

    // 轨迹采样：把历史位置向后挪一格
    if (p.trail) {
      const tr = p.trail
      for (let i = tr.length - 3; i >= 3; i -= 3) {
        tr[i] = tr[i - 3]
        tr[i + 1] = tr[i - 2]
        tr[i + 2] = tr[i - 1]
      }
      tr[0] = p.x
      tr[1] = p.y
      tr[2] = p.z
    }
  }

  // 控制点当前位置（局部空间）
  _cpPos(id) {
    const cp = this.controlPoints.find((c) => c.id === id)
    if (!cp) return null
    if (cp.lockToPointer) {
      if (!this.pointer) return null
      return [this.pointer.x + cp.offset[0], this.pointer.y + cp.offset[1], cp.offset[2]]
    }
    return cp.offset
  }

  // ---------- 帧推进 ----------

  advance(dt) {
    if (this.paused || !this.visible) return
    // starttime 预热：首帧一次性快进，让场景一打开就有稳定的粒子分布
    if (!this._warmed) {
      this._warmed = true
      if (this.startTime > 0) {
        const step = 1 / 30
        const steps = Math.min(900, Math.round(this.startTime / step))
        for (let i = 0; i < steps; i++) this._step(step)
      }
    }
    this._step(dt)
  }

  _step(dt) {
    this.simTime += dt
    for (const em of this.emitters) {
      // instantaneous：一次性爆发 N 个（烟花/冲击波）
      if (em.instantaneous > 0 && !em._burst) {
        em._burst = true
        for (let i = 0; i < Math.min(em.instantaneous, this.maxCount); i++) this.spawn(em)
      }
      // 无 rate 的常驻场：维持池满（粒子按 lifetime 自然回收，补充维持总量恒定）。
      // 这类发射器没有 rate 来自然限流，总量完全由 maxcount 决定，而 maxcount 是
      // 作者在编辑器里填的「预算上限」而非期望值 —— 同一个 dust_motes_0 预设在 7 个
      // 场景里都是 128，只有 2370927443 被改成 100000。WE 靠 GPU 预算实际压制它；
      // 照搬会画出近两万颗尘埃（占该场景粒子总量的 82%），既拖慢渲染又让画面糊成一片。
      // 故按同预设的常见量级封顶，更接近真实观感。
      if (em.fill) {
        const FILL_CAP = 2048
        const cap = Math.min(this.maxCount, FILL_CAP)
        let live = 0
        for (let i = 0; i < this.pool.length; i++) if (this.pool[i].alive) live++
        // 单帧补充量也设上限，避免首帧一次性生成造成明显卡顿
        let refill = Math.min(cap - live, 512)
        while (refill-- > 0) this.spawn(em)
        continue
      }
      // 发射率同时受 rate 与 count 两个倍率影响（见 _applyOverride 里 count 的说明）
      const rate =
        em.rate *
        (this._ov.rateMul !== undefined ? this._ov.rateMul : 1) *
        (this._ov.countMul !== undefined ? this._ov.countMul : 1)
      if (!(rate > 0)) continue
      // 累加器必须**按发射器各存一份**：低速发射器（如光轴 rate=0.3/s）每帧只累加
      // 0.005 颗，需要几百帧才凑够 1 颗。若与其它发射器共用一个累加器，
      // 高速发射器会在每帧把整数部分取走，低速发射器的零头被反复清掉、永远发不出粒子
      // （2885492021 的光轴存活数只降不升，看起来就是"几根固定不动的光柱"）。
      em._accum = (em._accum || 0) + rate * dt
      let n = Math.floor(em._accum)
      if (n > 0) {
        em._accum -= n
        // 单帧生成上限：rate 可达 15000/s，掉帧时累积量会瞬间打满池子
        if (n > this.maxCount) n = this.maxCount
        for (let i = 0; i < n; i++) this.spawn(em)
      }
    }
    const pool = this.pool
    for (let i = 0; i < pool.length; i++) {
      if (pool[i].alive) this.updateParticle(pool[i], dt)
    }
  }

  // ---------- 渲染 ----------

  // viewProj：场景投影矩阵；projH 用于 y 翻转（世界 y 向下 → 投影空间）
  render(viewProj, width, height, projW, projH) {
    if (!this._ready || !this.visible) return
    const gl = this.gl
    if (!this._prog) this._buildProgram(gl)

    const trail = this.trailCfg
    const segs = trail ? this.trailSegments : 1
    // 每实例 12 float：pos(3) size(1) rot(1) color(4) frame(1) aspect(1) pad(1)
    const STRIDE = 12
    const pool = this.pool
    let live = 0
    for (let i = 0; i < pool.length; i++) if (pool[i].alive) live++
    if (live === 0) return

    const instCount = live * segs
    const need = instCount * STRIDE
    if (!this._data || this._data.length < need) this._data = new Float32Array(Math.max(need, 1024))
    const data = this._data
    let k = 0

    const bright = this._ov.brightness || 1
    const sysScale = this.sysScale
    // 精灵形状 = 贴图宽高比 × 图层非等比 scale
    const stretchX = this.spriteStretchX * (this.texAspectX || 1)
    const stretchY = this.spriteStretchY * (this.texAspectY || 1)
    const cos = Math.cos(this.angleZ)
    const sin = Math.sin(this.angleZ)
    const ox = this.originX
    const oy = this.originY
    const sx = this.scaleX
    const sy = this.scaleY

    // 局部 → 世界（含图层 origin/scale/angles），再 y 翻转到投影空间
    const toWorld = (lx, ly) => {
      const px = lx * sx
      const py = ly * sy
      return [ox + px * cos - py * sin, projH - (oy + px * sin + py * cos)]
    }

    for (let i = 0; i < pool.length; i++) {
      const p = pool[i]
      if (!p.alive) continue
      for (let s = 0; s < segs; s++) {
        let lx = p.x
        let ly = p.y
        let segAlpha = 1
        let segSize = 1
        if (trail && p.trail) {
          lx = p.trail[s * 3]
          ly = p.trail[s * 3 + 1]
          // 尾部越远越淡越细
          const t = segs > 1 ? s / (segs - 1) : 0
          segAlpha = 1 - t
          segSize = 1 - t * 0.55
        }
        const w = toWorld(lx, ly)
        data[k++] = w[0]
        data[k++] = w[1]
        data[k++] = 0
        data[k++] = Math.abs(p.size) * sysScale * segSize
        data[k++] = p.rot
        data[k++] = p.r * bright
        data[k++] = p.g * bright
        data[k++] = p.b * bright
        data[k++] = p.alpha * segAlpha
        // 非等比拉伸（光柱/雨丝/雾带靠它成形），在精灵局部空间应用于旋转前
        data[k++] = stretchX
        data[k++] = stretchY
        data[k++] = p.frame
      }
    }

    const prog = this._prog
    gl.useProgram(prog.prog)
    gl.bindVertexArray(this._vao)
    gl.bindBuffer(gl.ARRAY_BUFFER, this._vbuf)
    gl.bufferData(gl.ARRAY_BUFFER, data.subarray(0, k), gl.DYNAMIC_DRAW)

    gl.activeTexture(gl.TEXTURE0)
    gl.bindTexture(gl.TEXTURE_2D, this.texture.glTex)
    gl.uniform1i(prog.uniTex, 0)
    gl.uniformMatrix4fv(prog.uniMvp, false, viewProj)
    const fd = this._frameUniformData()
    if (fd) {
      gl.uniform1i(prog.uniFrameCount, fd.count)
      gl.uniform4fv(prog.uniFrames, fd.arr)
    } else {
      gl.uniform1i(prog.uniFrameCount, 0)
    }

    gl.enable(gl.BLEND)
    // WE 混合模式：additive 用于发光类（火花/萤火虫/光晕），translucent 是普通 alpha。
    // 贴图是非预乘的，故 alpha 混合用 (SRC_ALPHA, 1-SRC_ALPHA)；
    // additive 也要乘 SRC_ALPHA，否则透明区域会把整块贴图矩形加亮成方块。
    if (this.blend === 'additive') gl.blendFunc(gl.SRC_ALPHA, gl.ONE)
    else gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA)
    gl.depthMask(false)

    gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, instCount)

    gl.depthMask(true)
    gl.disable(gl.BLEND)
    gl.bindVertexArray(null)
  }

  _buildProgram(gl) {
    const vs = `#version 300 es
// 单位 quad（TRIANGLE_STRIP 4 顶点），按实例的 size/rot 展开为朝屏幕的精灵
layout(location=0) in vec2 a_corner;      // -0.5..0.5
layout(location=1) in vec3 a_pos;         // 实例中心（投影空间世界坐标）
layout(location=2) in vec2 a_sizeRot;     // x=size(像素) y=rot(弧度)
layout(location=3) in vec4 a_color;       // rgb + alpha
layout(location=4) in vec3 a_stretchFrame; // xy=非等比拉伸 z=帧序号
uniform mat4 u_mvp;
// 序列帧 uv 变换表（TEXS 帧矩形归一化后的 offset/scale），最多 64 帧
uniform int u_frameCount;
uniform vec4 u_frames[64];               // xy=offset zw=scale
out vec2 v_uv;
out vec4 v_color;
void main(){
  float size = a_sizeRot.x;
  float rot = a_sizeRot.y;
  float c = cos(rot), s = sin(rot);
  // 先按 size 与非等比拉伸展开，再旋转（顺序反了会把拉伸方向也转走）
  vec2 corner = a_corner * size * a_stretchFrame.xy;
  vec2 rotated = vec2(corner.x * c - corner.y * s, corner.x * s + corner.y * c);
  gl_Position = u_mvp * vec4(a_pos.xy + rotated, a_pos.z, 1.0);
  // quad 角 → 贴图 uv。世界 y 已翻转到投影空间（y 向下），故 quad 的 +y 角
  // 对应屏幕上方，应采样纹理顶行 v=1（与 renderer.js 的 layerQuadVerts 同约定）。
  vec2 uv = a_corner + 0.5;
  if (u_frameCount > 0) {
    // 帧矩形以左上为原点（TEXS 是 top-down 像素坐标），故先把 v 翻成 top-down
    int fi = int(a_stretchFrame.z);
    fi = clamp(fi, 0, u_frameCount - 1);
    vec4 fr = u_frames[fi];
    vec2 cell = vec2(uv.x, 1.0 - uv.y) * fr.zw + fr.xy;
    uv = vec2(cell.x, 1.0 - cell.y);
  }
  v_uv = uv;
  v_color = a_color;
}`
    const fs = `#version 300 es
precision mediump float;
uniform sampler2D u_tex;
in vec2 v_uv;
in vec4 v_color;
out vec4 fragColor;
void main(){
  vec4 t = texture(u_tex, v_uv);
  fragColor = vec4(t.rgb * v_color.rgb, t.a * v_color.a);
}`
    const compile = (type, src) => {
      const s = gl.createShader(type)
      gl.shaderSource(s, src)
      gl.compileShader(s)
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error('粒子 shader: ' + gl.getShaderInfoLog(s))
      return s
    }
    const prog = gl.createProgram()
    gl.attachShader(prog, compile(gl.VERTEX_SHADER, vs))
    gl.attachShader(prog, compile(gl.FRAGMENT_SHADER, fs))
    gl.linkProgram(prog)
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error('粒子 shader 链接失败: ' + gl.getProgramInfoLog(prog))

    // 静态 quad 角点
    this._quadBuf = gl.createBuffer()
    gl.bindBuffer(gl.ARRAY_BUFFER, this._quadBuf)
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-0.5, -0.5, 0.5, -0.5, -0.5, 0.5, 0.5, 0.5]), gl.STATIC_DRAW)

    this._vbuf = gl.createBuffer()
    this._vao = gl.createVertexArray()
    gl.bindVertexArray(this._vao)
    // location 0：quad 角（每顶点）
    gl.bindBuffer(gl.ARRAY_BUFFER, this._quadBuf)
    gl.enableVertexAttribArray(0)
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 8, 0)
    gl.vertexAttribDivisor(0, 0)
    // location 1..4：实例数据（stride 48 = 12 float）
    gl.bindBuffer(gl.ARRAY_BUFFER, this._vbuf)
    const S = 48
    gl.enableVertexAttribArray(1)
    gl.vertexAttribPointer(1, 3, gl.FLOAT, false, S, 0)
    gl.vertexAttribDivisor(1, 1)
    gl.enableVertexAttribArray(2)
    gl.vertexAttribPointer(2, 2, gl.FLOAT, false, S, 12)
    gl.vertexAttribDivisor(2, 1)
    gl.enableVertexAttribArray(3)
    gl.vertexAttribPointer(3, 4, gl.FLOAT, false, S, 20)
    gl.vertexAttribDivisor(3, 1)
    gl.enableVertexAttribArray(4)
    gl.vertexAttribPointer(4, 3, gl.FLOAT, false, S, 36)
    gl.vertexAttribDivisor(4, 1)
    gl.bindVertexArray(null)

    this._prog = {
      prog,
      uniTex: gl.getUniformLocation(prog, 'u_tex'),
      uniMvp: gl.getUniformLocation(prog, 'u_mvp'),
      uniFrameCount: gl.getUniformLocation(prog, 'u_frameCount'),
      uniFrames: gl.getUniformLocation(prog, 'u_frames'),
    }
  }

  // 序列帧 uv 表（vec4[]：xy=offset zw=scale）。
  // 有 TEXS 用真实帧矩形；否则按 sequencemultiplier 退回 N×N 方格。
  _frameUniformData() {
    if (this._frameData !== undefined) return this._frameData
    let list = this.texFrames
    if (!list && this.sequenceMul > 1) {
      const n = this.sequenceMul
      list = []
      for (let r = 0; r < n; r++)
        for (let c = 0; c < n; c++) list.push({ ou: c / n, ov: r / n, su: 1 / n, sv: 1 / n })
    }
    if (!list || list.length === 0) {
      this._frameData = null
      return null
    }
    const cap = Math.min(list.length, 64)
    const arr = new Float32Array(cap * 4)
    for (let i = 0; i < cap; i++) {
      arr[i * 4] = list[i].ou
      arr[i * 4 + 1] = list[i].ov
      arr[i * 4 + 2] = list[i].su
      arr[i * 4 + 3] = list[i].sv
    }
    this._frameData = { count: cap, arr }
    return this._frameData
  }

  // 诊断用：当前存活粒子数
  liveCount() {
    let n = 0
    for (const p of this.pool) if (p.alive) n++
    return n
  }

  dispose() {
    this.paused = true
    const gl = this.gl
    if (!gl) return
    try {
      if (this._vbuf) gl.deleteBuffer(this._vbuf)
      if (this._quadBuf) gl.deleteBuffer(this._quadBuf)
      if (this._vao) gl.deleteVertexArray(this._vao)
      if (this._prog && this._prog.prog) gl.deleteProgram(this._prog.prog)
    } catch (e) {
      /* 上下文可能已丢失 */
    }
    this._prog = null
    this._vbuf = null
    this._quadBuf = null
    this._vao = null
    this._data = null
  }
}
