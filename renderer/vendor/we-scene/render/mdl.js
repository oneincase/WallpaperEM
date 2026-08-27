// [we-scene patch] MDL（Puppet Warp 骨骼网格）解析与渲染
// 格式逆向见 /tmp/mdl_analysis.md。本实现聚焦静态网格渲染：
// MDLV 网格(80B 顶点 + u16 索引) + 材质贴图 → WebGL 绘制。
// 骨骼动画（MDLA）后续扩展。

const DV = (buf, off) => {
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  return { dv, off }
}

export function parseMDL(buf) {
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  // 魔数
  const magic = readStr(dv, 0, 8)
  if (!magic.startsWith('MDLV')) throw new Error('不是 MDL: ' + magic)
  // 材质路径：0x15 起 null 结尾字符串
  const materialPath = readCStr(dv, 0x15)
  // 网格头
  const meshHeader = 0x47
  const vertexBytes = dv.getUint32(meshHeader + 4, true)
  const vertexCount = vertexBytes / 80
  const vertStart = meshHeader + 8
  // 顶点
  const positions = new Float32Array(vertexCount * 3)
  const uvs = new Float32Array(vertexCount * 2)
  const boneIdx = new Int32Array(vertexCount * 4)
  const weights = new Float32Array(vertexCount * 4)
  for (let i = 0; i < vertexCount; i++) {
    const base = vertStart + i * 80
    positions[i * 3] = dv.getFloat32(base, true)
    positions[i * 3 + 1] = dv.getFloat32(base + 4, true)
    positions[i * 3 + 2] = dv.getFloat32(base + 8, true)
    uvs[i * 2] = dv.getFloat32(base + 72, true)
    uvs[i * 2 + 1] = dv.getFloat32(base + 76, true)
    for (let b = 0; b < 4; b++) {
      boneIdx[i * 4 + b] = dv.getUint32(base + 40 + b * 4, true)
      weights[i * 4 + b] = dv.getFloat32(base + 56 + b * 4, true)
    }
  }
  // 索引
  const idxByteLen = dv.getUint32(vertStart + vertexBytes, true)
  const idxStart = vertStart + vertexBytes + 4
  const indexCount = idxByteLen / 2
  const indices = new Uint16Array(indexCount)
  for (let i = 0; i < indexCount; i++) {
    indices[i] = dv.getUint16(idxStart + i * 2, true)
  }

  // [we-scene patch] 骨骼蒙皮：解析骨架(MDLS) + 绑定矩阵(MDLE)，顶点变换回绑定姿势
  // 使各部位（受不同骨骼影响）拼合成完整模型。静态渲染用逆绑定矩阵。
  const skinned = skinVertices(dv, vertexCount, positions, boneIdx, weights)

  return {
    magic,
    materialPath,
    vertexCount,
    positions: skinned,
    uvs,
    boneIdx,
    weights,
    indexCount,
    indices,
    bounds: (() => {
      let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity
      for (let i = 0; i < vertexCount; i++) {
        const x = skinned[i * 3], y = skinned[i * 3 + 1]
        if (x < minX) minX = x
        if (x > maxX) maxX = x
        if (y < minY) minY = y
        if (y > maxY) maxY = y
      }
      return { minX, maxX, minY, maxY }
    })(),
  }
}

// [we-scene patch] 骨骼蒙皮：pos' = Σ w_i · invBind[bone_i] · pos
// MDLS0004 骨架（逐骨扫描 null 结尾 JSON，可变长条目）+ MDLE0002 绑定矩阵
function skinVertices(dv, vertexCount, positions, boneIdx, weights) {
  try {
    const n = dv.byteLength
    // 定位 MDLS 骨架（ASCII 魔数 'MDLS0004'）
    const sIdx = findAscii(dv, n, 'MDLS0004')
    if (sIdx < 0) return positions
    // MDLS: 魔数(8) + 00(1) + nextOff(4) + boneCount(4)
    let j = sIdx + 8 + 1 + 4 + 4
    const boneCount = dv.getUint32(sIdx + 8 + 1 + 4, true)
    if (boneCount <= 0 || boneCount > 512) return positions
    // 逐骨扫描：读 13B 头 + 64B 矩阵 + 跳 JSON(null 结尾)
    const parents = new Int32Array(boneCount)
    const localMat = new Float32Array(boneCount * 16)
    for (let b = 0; b < boneCount; b++) {
      if (j + 77 > n) return positions
      parents[b] = dv.getInt32(j + 5, true)
      for (let k = 0; k < 16; k++) localMat[b * 16 + k] = dv.getFloat32(j + 13 + k * 4, true)
      // 跳 JSON：从 j+77 起找 null
      let e = j + 77
      while (e < n && dv.getUint8(e) !== 0) e++
      j = e + 1
    }
    // 世界矩阵：父链累积（行主序 v' = v·M，故 world = parent_world · local）
    const worldMat = new Float32Array(boneCount * 16)
    {
      const tmp = new Float32Array(16)
      for (let b = 0; b < boneCount; b++) {
        const p = parents[b]
        if (p < 0 || p >= boneCount) {
          for (let k = 0; k < 16; k++) worldMat[b * 16 + k] = localMat[b * 16 + k]
        } else {
          mulMat4(worldMat, p * 16, localMat, b * 16, tmp)
          for (let k = 0; k < 16; k++) worldMat[b * 16 + k] = tmp[k]
        }
      }
    }
    // MDLE 绑定矩阵
    const mIdx = findAscii(dv, n, 'MDLE0002')
    if (mIdx < 0) return positions
    // MDLE: 魔数(8) + 00(1) + endPos(4) + byteSize(4) + 每骨 64B
    const bj = mIdx + 8 + 1 + 4 + 4
    // 计算每骨逆绑定矩阵
    const invBind = []
    for (let b = 0; b < boneCount; b++) {
      const m = []
      for (let k = 0; k < 16; k++) m.push(dv.getFloat32(bj + b * 64 + k * 4, true))
      invBind.push(invMat4(m))
    }
    // 蒙皮: pos' = Σ w_i · (World[bone_i] · InvBind[bone_i]) · pos
    const out = new Float32Array(vertexCount * 3)
    const skinMat = new Float32Array(16)
    for (let i = 0; i < vertexCount; i++) {
      let sx = 0, sy = 0, sz = 0
      const x = positions[i * 3], y = positions[i * 3 + 1], z = positions[i * 3 + 2]
      for (let q = 0; q < 4; q++) {
        const wt = weights[i * 4 + q]
        if (wt <= 0.001) continue
        const bi = boneIdx[i * 4 + q]
        if (bi < 0 || bi >= boneCount) continue
        // skin = World[bi] · InvBind[bi]
        mulMat4(worldMat, bi * 16, invBind[bi], 0, skinMat)
        // 行主序 v' = v·M
        const tx = x * skinMat[0] + y * skinMat[1] + z * skinMat[2] + skinMat[3]
        const ty = x * skinMat[4] + y * skinMat[5] + z * skinMat[6] + skinMat[7]
        const tz = x * skinMat[8] + y * skinMat[9] + z * skinMat[10] + skinMat[11]
        sx += tx * wt; sy += ty * wt; sz += tz * wt
      }
      out[i * 3] = sx; out[i * 3 + 1] = sy; out[i * 3 + 2] = sz
    }
    return out
  } catch (e) {
    // 蒙皮失败则用原始位置
    return positions
  }
}

// 4x4 行主序矩阵乘法：dst = A(srcAOff) · B(srcBOff)
function mulMat4(A, srcAOff, B, srcBOff, dst) {
  for (let i = 0; i < 4; i++) {
    for (let c = 0; c < 4; c++) {
      let s = 0
      for (let k = 0; k < 4; k++) s += A[srcAOff + i * 4 + k] * B[srcBOff + k * 4 + c]
      dst[i * 4 + c] = s
    }
  }
}

// 在 DataView 中查找 ASCII 字符串，返回偏移或 -1
function findAscii(dv, n, str) {
  const first = str.charCodeAt(0)
  const len = str.length
  for (let p = 0; p <= n - len; p++) {
    if (dv.getUint8(p) === first) {
      let ok = true
      for (let k = 1; k < len; k++) {
        if (dv.getUint8(p + k) !== str.charCodeAt(k)) { ok = false; break }
      }
      if (ok) return p
    }
  }
  return -1
}

// 4x4 行主序矩阵求逆（纯 JS）
function invMat4(M) {
  const a = []
  for (let i = 0; i < 4; i++) a.push([M[i * 4], M[i * 4 + 1], M[i * 4 + 2], M[i * 4 + 3]])
  const aug = a.map((row, i) => row.concat([1, 0, 0, 0].map((v, j) => (i === j ? 1 : 0))))
  for (let col = 0; col < 4; col++) {
    let piv = col
    for (let r = col; r < 4; r++) if (Math.abs(aug[r][col]) > Math.abs(aug[piv][col])) piv = r
    ;[aug[col], aug[piv]] = [aug[piv], aug[col]]
    const pv = aug[col][col]
    if (Math.abs(pv) < 1e-12) return M
    for (let j = 0; j < 8; j++) aug[col][j] /= pv
    for (let r = 0; r < 4; r++) {
      if (r !== col) {
        const f = aug[r][col]
        for (let j = 0; j < 8; j++) aug[r][j] -= f * aug[col][j]
      }
    }
  }
  const res = []
  for (let r = 0; r < 4; r++) for (let j = 0; j < 4; j++) res.push(aug[r][4 + j])
  return res
}

function readStr(dv, off, len) {
  let s = ''
  for (let i = off; i < off + len; i++) s += String.fromCharCode(dv.getUint8(i))
  return s
}
function readCStr(dv, off) {
  let s = ''
  while (off < dv.byteLength) {
    const c = dv.getUint8(off++)
    if (c === 0) break
    s += String.fromCharCode(c)
  }
  return s
}

// 创建一个 WebGL 网格渲染器
export function createMDLRenderer(gl) {
  const vs = `#version 300 es
in vec2 a_pos;
in vec2 a_uv;
uniform mat4 u_mvp;
uniform float u_scaleX;
uniform float u_scaleY;
uniform vec2 u_offset;
out vec2 v_uv;
void main(){
  vec2 p = a_pos * vec2(u_scaleX, u_scaleY) + u_offset;
  gl_Position = u_mvp * vec4(p, 0.0, 1.0);
  v_uv = a_uv;
}`
  const fs = `#version 300 es
precision mediump float;
uniform sampler2D u_tex;
in vec2 v_uv;
out vec4 fragColor;
void main(){
  vec4 t = texture(u_tex, v_uv);
  if (t.a < 0.02) discard;
  fragColor = t;
}`
  const prog = gl.createProgram()
  const compile = (type, src) => {
    const s = gl.createShader(type)
    gl.shaderSource(s, src)
    gl.compileShader(s)
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error('MDL shader: ' + gl.getShaderInfoLog(s))
    return s
  }
  gl.attachShader(prog, compile(gl.VERTEX_SHADER, vs))
  gl.attachShader(prog, compile(gl.FRAGMENT_SHADER, fs))
  gl.bindAttribLocation(prog, 0, 'a_pos')
  gl.bindAttribLocation(prog, 1, 'a_uv')
  gl.linkProgram(prog)
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error('MDL shader link fail')

  // VAO
  const vao = gl.createVertexArray()
  gl.bindVertexArray(vao)
  const vbuf = gl.createBuffer()
  gl.bindBuffer(gl.ARRAY_BUFFER, vbuf)
  gl.enableVertexAttribArray(0)
  gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 16, 0)
  gl.enableVertexAttribArray(1)
  gl.vertexAttribPointer(1, 2, gl.FLOAT, false, 16, 8)
  const ibuf = gl.createBuffer()
  gl.bindVertexArray(null)

  return {
    gl,
    prog,
    vao,
    vbuf,
    ibuf,
    uni: {
      mvp: gl.getUniformLocation(prog, 'u_mvp'),
      tex: gl.getUniformLocation(prog, 'u_tex'),
      scaleX: gl.getUniformLocation(prog, 'u_scaleX'),
      scaleY: gl.getUniformLocation(prog, 'u_scaleY'),
      offset: gl.getUniformLocation(prog, 'u_offset'),
    },
    // 上传网格
    upload(mdl) {
      const vertData = new Float32Array(mdl.vertexCount * 4)
      for (let i = 0; i < mdl.vertexCount; i++) {
        vertData[i * 4] = mdl.positions[i * 3]
        vertData[i * 4 + 1] = mdl.positions[i * 3 + 1]
        vertData[i * 4 + 2] = mdl.uvs[i * 2]
        vertData[i * 4 + 3] = mdl.uvs[i * 2 + 1]
      }
      gl.bindBuffer(gl.ARRAY_BUFFER, vbuf)
      gl.bufferData(gl.ARRAY_BUFFER, vertData, gl.STATIC_DRAW)
      gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, ibuf)
      gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, mdl.indices, gl.STATIC_DRAW)
    },
    draw(viewProj, mdl, opts, texture) {
      gl.useProgram(prog)
      gl.bindVertexArray(vao)
      gl.activeTexture(gl.TEXTURE0)
      gl.bindTexture(gl.TEXTURE_2D, texture.glTex || texture)
      gl.uniform1i(this.uni.tex, 0)
      gl.uniformMatrix4fv(this.uni.mvp, false, viewProj)
      gl.uniform1f(this.uni.scaleX, opts.scaleX || 1)
      gl.uniform1f(this.uni.scaleY, opts.scaleY || 1)
      gl.uniform2f(this.uni.offset, opts.offsetX || 0, opts.offsetY || 0)
      gl.enable(gl.BLEND)
      gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA)
      gl.drawElements(gl.TRIANGLES, mdl.indexCount, gl.UNSIGNED_SHORT, 0)
      gl.disable(gl.BLEND)
      gl.bindVertexArray(null)
    },
  }
}
