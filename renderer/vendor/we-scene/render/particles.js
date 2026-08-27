// [we-scene patch] 粒子系统：CPU 模拟 + WebGL 点精灵渲染。
// 坐标：世界坐标 = 像素，y 向下（与场景一致）。
// 用法：main.ts 从 scene.pkg 解析粒子模型 json + 贴图，构造 ParticleSystem，注入 renderer。

const TAU = Math.PI * 2

function rand(a, b) {
  if (a === undefined || a === null) return Math.random()
  if (b === undefined || b === null) return a
  if (Array.isArray(a)) return a.map((av, i) => rand(av, b[i]))
  return a + Math.random() * (b - a)
}

function parseVec(s) {
  if (Array.isArray(s)) return s
  if (s === undefined || s === null) return [0, 0, 0]
  if (typeof s === 'number') return [s, s, s]
  const p = String(s).trim().split(/\s+/).map(Number)
  return [p[0] || 0, p[1] || 0, p[2] || 0]
}

function hash2(x, y) {
  const n = Math.sin(x * 127.1 + y * 311.7) * 43758.5453
  return n - Math.floor(n)
}

// 简易值噪声（remapvalue transformfunction: simplexnoise/fbmnoise）
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
function fbm(x, y, oct = 3) {
  let v = 0
  let amp = 0.5
  let f = 1
  for (let i = 0; i < oct; i++) {
    v += amp * vnoise(x * f, y * f)
    amp *= 0.5
    f *= 2
  }
  return v
}

class Particle {
  constructor() {
    this.alive = false
    this.x = this.y = this.z = 0
    this.vx = this.vy = this.vz = 0
    this.age = 0
    this.life = 1
    this.size = 1
    this.rot = 0
    this.rotVel = 0
    this.color = [1, 1, 1]
    this.alpha = 1
    this.seed = Math.random()
    this.baseX = 0
    this.baseY = 0
    this.baseZ = 0
  }
}

export class ParticleSystem {
  constructor(gl, model, override) {
    this.gl = gl
    this.model = model || {}
    this.override = override || {}
    this.maxCount = Math.max(1, this.model.maxcount || 100)
    this.particles = []
    this.spawnAccum = 0
    this.simTime = 0
    this.paused = false
    this.texture = null // {glTex,width,height}
    this.material = null
    this.blend = 'translucent'
    this.opacityMul = 1
    this.visible = true
    this.positionScale = 1
    this.pool = []
    this._ready = false
    // instanceoverride 展开
    this._ov = {}
    this._applyOverride()
  }

  _applyOverride() {
    const ov = this.override || {}
    for (const k of Object.keys(ov)) {
      if (k === 'id') continue
      const v = ov[k]
      const val = v && typeof v === 'object' && 'value' in v ? v.value : v
      if (k === 'count') {
        const base = this.model.maxcount || 100
        this.maxCount = Math.max(1, Math.round(base * Number(val) || base))
      } else if (k === 'alpha') {
        this.opacityMul = Number(val) || 0
      } else if (k === 'color' || k === 'colorn') {
        this._ov.color = parseVec(val)
      } else if (k === 'size') {
        this._ov.size = Number(val) || 1
      } else if (k === 'rate') {
        this._ov.rate = Number(val) || 0
      } else if (k === 'speed') {
        this._ov.speed = Number(val) || 1
      } else if (k === 'brightness') {
        this._ov.brightness = Number(val) || 1
      } else if (k === 'controlpoint1' || k === 'controlpoint2') {
        this._ov[k] = parseVec(val)
      } else {
        this._ov[k] = val
      }
    }
    // 重建粒子池
    this.pool = []
    for (let i = 0; i < this.maxCount; i++) this.pool.push(new Particle())
  }

  // 注入模型（由 main.ts 在加载 json 后调用）
  setModel(model) {
    this.model = model
  }

  // 注入材质（决定混合模式）
  setMaterial(mat) {
    this.material = mat
    const pass = mat && mat.passes && mat.passes[0]
    this.blend = (pass && pass.blending) || 'translucent'
  }

  // 注入贴图 {glTex,width,height}
  setTexture(tex) {
    this.texture = tex
    this._ready = !!tex
  }

  setVisible(v) {
    this.visible = v
  }

  // ---- 粒子生成（emitter + initializer）----
  spawn(emitter) {
    const p = this.pool.find((q) => !q.alive)
    if (!p) return
    const init = this.model.initializer || []
    p.alive = true
    p.age = 0
    p.life = 1
    p.size = 1
    p.rot = 0
    p.rotVel = 0
    p.color = [1, 1, 1]
    p.alpha = 1
    p.vx = p.vy = p.vz = 0
    p.seed = Math.random()

    // 发射位置
    const origin = parseVec(emitter && emitter.origin)
    const eName = emitter && emitter.name
    if (eName === 'boxrandom') {
      const d = parseVec(emitter.distancemax || '0 0 0')
      p.x = origin[0] + rand(-d[0], d[0])
      p.y = origin[1] + rand(-d[1], d[1])
      p.z = origin[2] + rand(-d[2], d[2])
    } else {
      // sphererandom / 默认
      const dir = parseVec(emitter.directions || '1 1 1')
      const dmin = emitter.distancemin || 0
      const dmax = emitter.distancemax || 0
      const a = Math.random() * TAU
      const b = Math.random() * 2 - 1
      const r = rand(dmin, dmax)
      p.x = origin[0] + Math.cos(a) * b * r * (dir[0] || 1)
      p.y = origin[1] + Math.sin(a) * b * r * (dir[1] || 1)
      p.z = origin[2] + b * r * (dir[2] || 1)
    }
    p.baseX = p.x
    p.baseY = p.y
    p.baseZ = p.z

    // initializer
    const sizeMul = this._ov.size || 1
    const speedMul = this._ov.speed || 1
    for (const inz of init) {
      const name = inz.name
      if (name === 'lifetimerandom') {
        p.life = rand(inz.min, inz.max)
      } else if (name === 'sizerandom') {
        p.size = sizeMul * rand(inz.min, inz.max)
      } else if (name === 'velocityrandom') {
        const mn = parseVec(inz.min)
        const mx = parseVec(inz.max)
        p.vx = rand(mn[0], mx[0]) * speedMul
        p.vy = rand(mn[1], mx[1]) * speedMul
        p.vz = rand(mn[2], mx[2]) * speedMul
      } else if (name === 'colorrandom') {
        const mn = parseVec(inz.min)
        const mx = parseVec(inz.max)
        let c = [rand(mn[0], mx[0]) / 255, rand(mn[1], mx[1]) / 255, rand(mn[2], mx[2]) / 255]
        if (this._ov.color) {
          const cv = this._ov.color
          c = cv.map((v) => Math.abs(v) / 255)
        }
        p.color = c
      } else if (name === 'rotationrandom') {
        p.rot = rand(inz.min, inz.max)
      } else if (name === 'angularvelocityrandom') {
        p.rotVel = rand(inz.min, inz.max)
      }
    }
    if (this.opacityMul !== 1) p.alpha = this.opacityMul
  }

  // ---- 更新粒子（operator）----
  updateParticle(p, dt) {
    p.age += dt
    if (p.age >= p.life) {
      p.alive = false
      return
    }
    const ops = this.model.operator || []
    let gx = 0
    let gy = 0
    let drag = 0
    const turb = []
    const oscPos = []
    for (const op of ops) {
      const name = op.name
      if (name === 'movement') {
        const g = parseVec(op.gravity)
        gx = g[0]
        gy = g[1]
        drag = op.drag || 0
      } else if (name === 'turbulence') {
        turb.push(op)
      } else if (name === 'oscillateposition') {
        oscPos.push(op)
      }
    }
    // 重力
    p.vx += gx * dt
    p.vy += gy * dt
    // 拖拽
    if (drag > 0) {
      const d = Math.max(0, 1 - drag * dt)
      p.vx *= d
      p.vy *= d
    }
    // 湍流
    for (const t of turb) {
      const s = t.scale || 0.01
      const smin = t.speedmin || 0
      const smax = t.speedmax || 0
      const sp = rand(smin, smax)
      const n1 = fbm(p.x * s + this.simTime * sp, p.y * s)
      const n2 = fbm(p.x * s + this.simTime * sp + 100, p.y * s)
      p.vx += (n1 - 0.5) * 200 * dt
      p.vy += (n2 - 0.5) * 200 * dt
    }
    p.x += p.vx * dt
    p.y += p.vy * dt
    p.z += p.vz * dt
    p.rot += p.rotVel * dt
    // 振荡位移
    for (const o of oscPos) {
      const f = rand(o.frequencymin || 0.1, o.frequencymax || 0.5)
      const amp = rand(o.scalemin || 0, o.scalemax || 1)
      const ph = rand(o.phasemin || 0, o.phasemax || TAU)
      p.x = p.baseX + Math.sin(this.simTime * f + ph + p.seed * TAU) * amp
      p.y = p.baseY + Math.cos(this.simTime * f + ph + p.seed * TAU) * amp
    }
  }

  // 帧推进
  advance(dt) {
    if (this.paused || !this.visible) return
    this.simTime += dt
    const emitters = this.model.emitter || []
    for (const em of emitters) {
      const rate = this._ov.rate !== undefined ? this._ov.rate : em.rate || 0
      if (rate <= 0) continue
      this.spawnAccum += rate * dt
      const n = Math.floor(this.spawnAccum)
      if (n > 0) {
        this.spawnAccum -= n
        for (let i = 0; i < Math.min(n, 200); i++) this.spawn(em)
      }
    }
    for (const p of this.pool) {
      if (p.alive) this.updateParticle(p, dt)
    }
  }

  // alpha 曲线（淡入淡出）
  _alphaCurve(lr) {
    const fi = 0.12
    const fo = 0.82
    if (lr < fi) return lr / fi
    if (lr > fo) return Math.max(0, (1 - lr) / (1 - fo))
    return 1
  }

  // 渲染（在场景 viewProj 下，作为点精灵）
  render(viewProj, width, height, projW, projH) {
    if (!this._ready || !this.visible) return
    const gl = this.gl
    const alive = this.pool.filter((p) => p.alive)
    if (alive.length === 0) return
    if (!this._prog) this._buildProgram(gl)

    const prog = this._prog
    const n = alive.length
    const data = new Float32Array(n * 9)
    let idx = 0
    const bright = this._ov.brightness || 1
    for (const p of alive) {
      const lr = 1 - p.age / p.life
      const a = p.alpha * this._alphaCurve(lr)
      data[idx++] = p.x
      data[idx++] = projH - p.y // y 翻转到投影坐标系（y 向下世界 → NDC）
      data[idx++] = p.size
      data[idx++] = p.rot
      data[idx++] = p.color[0] * bright
      data[idx++] = p.color[1] * bright
      data[idx++] = p.color[2] * bright
      data[idx++] = a
      data[idx++] = lr
    }

    gl.useProgram(prog.prog)
    gl.bindBuffer(gl.ARRAY_BUFFER, this._vbuf)
    gl.bufferData(gl.ARRAY_BUFFER, data, gl.DYNAMIC_DRAW)
    gl.enableVertexAttribArray(0)
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 36, 0)
    gl.enableVertexAttribArray(1)
    gl.vertexAttribPointer(1, 2, gl.FLOAT, false, 36, 8)
    gl.enableVertexAttribArray(2)
    gl.vertexAttribPointer(2, 4, gl.FLOAT, false, 36, 16)
    gl.enableVertexAttribArray(3)
    gl.vertexAttribPointer(3, 1, gl.FLOAT, false, 36, 32)

    gl.activeTexture(gl.TEXTURE0)
    gl.bindTexture(gl.TEXTURE_2D, this.texture.glTex)
    gl.uniform1i(prog.uniTex, 0)
    gl.uniformMatrix4fv(prog.uniMvp, false, viewProj)
    gl.uniform2f(prog.uniScreen, width, height)

    // 点精灵屏幕尺寸：粒子世界尺寸（像素）在正交投影下 = NDC → 屏幕像素
    // gl_PointSize 用 NDC 高度换算
    const blend = this.blend
    gl.enable(gl.BLEND)
    if (blend === 'additive') {
      gl.blendFunc(gl.ONE, gl.ONE)
    } else if (blend === 'alphatocoverage') {
      gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA)
    } else {
      gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA)
    }
    gl.drawArrays(gl.POINTS, 0, n)
    gl.disable(gl.BLEND)
  }

  _buildProgram(gl) {
    const vs = `#version 300 es
layout(location=0) in vec2 a_pos;
layout(location=1) in vec2 a_sizeRot;
layout(location=2) in vec4 a_color;
layout(location=3) in float a_life;
uniform mat4 u_mvp;
uniform vec2 u_screen;
out vec2 v_uv;
out vec4 v_color;
void main(){
  vec4 clip = u_mvp * vec4(a_pos, 0.0, 1.0);
  gl_Position = clip;
  // NDC 空间尺寸 → 屏幕像素
  float size = a_sizeRot.x;
  gl_PointSize = max(1.0, size);
  v_uv = vec2(0.5);
  v_color = a_color;
}`
    const fs = `#version 300 es
precision mediump float;
uniform sampler2D u_tex;
in vec2 v_uv;
in vec4 v_color;
out vec4 fragColor;
void main(){
  vec2 uv = gl_PointCoord;
  vec4 t = texture(u_tex, uv);
  fragColor = vec4(t.rgb * v_color.rgb, t.a * v_color.a);
}`
    const prog = gl.createProgram()
    const compile = (type, src) => {
      const s = gl.createShader(type)
      gl.shaderSource(s, src)
      gl.compileShader(s)
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error('粒子 shader: ' + gl.getShaderInfoLog(s))
      return s
    }
    gl.attachShader(prog, compile(gl.VERTEX_SHADER, vs))
    gl.attachShader(prog, compile(gl.FRAGMENT_SHADER, fs))
    gl.linkProgram(prog)
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error('粒子 shader 链接失败')
    this._prog = {
      prog,
      uniTex: gl.getUniformLocation(prog, 'u_tex'),
      uniMvp: gl.getUniformLocation(prog, 'u_mvp'),
      uniScreen: gl.getUniformLocation(prog, 'u_screen'),
    }
    this._vbuf = gl.createBuffer()
  }

  dispose() {
    this.paused = true
  }
}
