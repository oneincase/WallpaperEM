// [we-scene patch] MDL（Puppet Warp 骨骼网格）解析与渲染
//
// 格式（逆向自本机壁纸库 23 个 MDLV0023 模型，全部校验通过）：
//
//   MDLV0023 头
//     0x00  char[8]   "MDLV0023"
//     0x15  cstr      材质路径（如 "materials/01腿-后.json"）
//     +32   u32       顶点区字节数（= vertexCount × 80）★ 相对材质路径 null 之后
//             顶点 80B：pos vec3 @0 / boneIdx u32×4 @40 / weights f32×4 @56 / uv vec2 @72
//     后接  u32       索引区字节数，随后 u16 索引
//
//   MDLS0004 骨架（魔数 + u8 + u32 nextOff + u32 boneCount，逐骨条目）
//     每骨：u8 flag / u32 id / i32 parent / u32 matSize(=64) / f32[16] 绑定矩阵 / cstr JSON
//     绑定矩阵为**列主序**（平移在 [12],[13],[14]），与 WebGL 一致。
//
//   MDLA0006 动画（魔数 + u8 + u32 endPos + u32 animCount，逐动画条目）
//     每动画：u32 id / u32 unk / cstr 名称 / cstr 模式("loop") / f32 fps
//             u32 frameCount / u32 unk / u32 trackCount
//             每轨：u32 boneId / u32 trackBytes / 关键帧×(trackBytes/36)
//               关键帧 36B：f32[3] 平移 / f32[4] 四元数 xyzw / f32[2] 缩放 xy
//             动画条目末尾固定 35B 填充（据此 endPos 与实际游标精确吻合）
//     关键帧存**绝对局部变换**：frame 0 的平移与绑定矩阵平移完全相等
//     ⇒ 蒙皮矩阵 = World(anim) · World(bind)⁻¹，frame 0 为单位变换。
//
// 网格坐标 = 图层局部像素、**Y 轴朝上**；UV 与之精确对应：
//     u = (x + W/2) / W ,  v = (H/2 - y) / H   （W/H = 图层 size，实测误差 0）
// 因此渲染时把 y 取反即回到 y-down 的场景世界空间（与 renderer.js 的层空间一致）。
//
// 历史 bug（导致「人物不动 + 贴图错乱」）：
//   1) 顶点区偏移硬编码 0x47，实际为「材质路径长度 + 32」，材质路径长短不同即错位
//      → 顶点/UV 全部读到错误字节 = 贴图错乱；
//   2) 骨骼矩阵按行主序解读（实为列主序），且要求 MDLE 块存在，
//      多数模型没有 MDLE 便直接返回未蒙皮顶点；
//   3) MDLA 动画块完全未解析 = 人物不动。

const IDENTITY = Object.freeze([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1])

// ---------- 列主序 4x4 工具（与 WebGL uniformMatrix4fv 的内存布局一致） ----------

function mat4Mul(a, b, out) {
  const o = out || new Float32Array(16)
  for (let c = 0; c < 4; c++) {
    for (let r = 0; r < 4; r++) {
      o[c * 4 + r] =
        a[r] * b[c * 4] +
        a[4 + r] * b[c * 4 + 1] +
        a[8 + r] * b[c * 4 + 2] +
        a[12 + r] * b[c * 4 + 3]
    }
  }
  return o
}

// 高斯消元求逆（骨骼绑定矩阵含平移/旋转/缩放，不保证正交，故用通用求逆）
function mat4Invert(m) {
  const a = []
  for (let r = 0; r < 4; r++) {
    a.push([m[r], m[4 + r], m[8 + r], m[12 + r], 0, 0, 0, 0])
    a[r][4 + r] = 1
  }
  for (let col = 0; col < 4; col++) {
    let piv = col
    for (let r = col + 1; r < 4; r++) {
      if (Math.abs(a[r][col]) > Math.abs(a[piv][col])) piv = r
    }
    const t = a[col]
    a[col] = a[piv]
    a[piv] = t
    const pv = a[col][col]
    if (Math.abs(pv) < 1e-12) return Float32Array.from(IDENTITY)
    for (let j = 0; j < 8; j++) a[col][j] /= pv
    for (let r = 0; r < 4; r++) {
      if (r === col) continue
      const f = a[r][col]
      if (f === 0) continue
      for (let j = 0; j < 8; j++) a[r][j] -= f * a[col][j]
    }
  }
  const out = new Float32Array(16)
  for (let c = 0; c < 4; c++) for (let r = 0; r < 4; r++) out[c * 4 + r] = a[r][4 + c]
  return out
}

// 平移 + 四元数旋转 + xy 缩放 → 列主序矩阵
function composeTRS(tx, ty, tz, qx, qy, qz, qw, sx, sy, out) {
  const o = out || new Float32Array(16)
  // WE 存的四元数未严格归一（实测 |q| 最大约 1.005），不归一会引入可见缩放
  const n = Math.hypot(qx, qy, qz, qw) || 1
  const x = qx / n
  const y = qy / n
  const z = qz / n
  const w = qw / n
  const xx = x * x
  const yy = y * y
  const zz = z * z
  o[0] = (1 - 2 * (yy + zz)) * sx
  o[1] = 2 * (x * y + z * w) * sx
  o[2] = 2 * (x * z - y * w) * sx
  o[3] = 0
  o[4] = 2 * (x * y - z * w) * sy
  o[5] = (1 - 2 * (xx + zz)) * sy
  o[6] = 2 * (y * z + x * w) * sy
  o[7] = 0
  o[8] = 2 * (x * z + y * w)
  o[9] = 2 * (y * z - x * w)
  o[10] = 1 - 2 * (xx + yy)
  o[11] = 0
  o[12] = tx
  o[13] = ty
  o[14] = tz
  o[15] = 1
  return o
}

// ---------- 解析 ----------

function readCStr(dv, off) {
  const bytes = []
  while (off < dv.byteLength) {
    const c = dv.getUint8(off++)
    if (c === 0) break
    bytes.push(c)
  }
  let s = ''
  try {
    s = new TextDecoder('utf-8').decode(new Uint8Array(bytes))
  } catch (e) {
    s = String.fromCharCode.apply(null, bytes)
  }
  return { value: s, next: off }
}

function findAscii(buf, str, from = 0) {
  const pat = []
  for (let i = 0; i < str.length; i++) pat.push(str.charCodeAt(i))
  outer: for (let p = from; p <= buf.length - pat.length; p++) {
    for (let k = 0; k < pat.length; k++) {
      if (buf[p + k] !== pat[k]) continue outer
    }
    return p
  }
  return -1
}

export function parseMDL(buf) {
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  const magic = String.fromCharCode.apply(null, Array.from(buf.subarray(0, 8)))
  if (!magic.startsWith('MDLV')) throw new Error('不是 MDL: ' + magic)

  // 材质路径（0x15 起 null 结尾）；顶点区长度紧随其后 +32 字节处
  const mat = readCStr(dv, 0x15)
  const vbOff = mat.next + 32
  const vertexBytes = dv.getUint32(vbOff, true)
  if (vertexBytes <= 0 || vertexBytes % 80 !== 0) {
    throw new Error('顶点区长度异常: ' + vertexBytes)
  }
  const vertStart = vbOff + 4
  const vertexCount = vertexBytes / 80

  const positions = new Float32Array(vertexCount * 3)
  const uvs = new Float32Array(vertexCount * 2)
  const boneIdx = new Float32Array(vertexCount * 4) // 顶点属性用 float 传（WebGL2 attribute）
  const weights = new Float32Array(vertexCount * 4)
  for (let i = 0; i < vertexCount; i++) {
    const b = vertStart + i * 80
    positions[i * 3] = dv.getFloat32(b, true)
    positions[i * 3 + 1] = dv.getFloat32(b + 4, true)
    positions[i * 3 + 2] = dv.getFloat32(b + 8, true)
    uvs[i * 2] = dv.getFloat32(b + 72, true)
    uvs[i * 2 + 1] = dv.getFloat32(b + 76, true)
    for (let k = 0; k < 4; k++) {
      boneIdx[i * 4 + k] = dv.getUint32(b + 40 + k * 4, true)
      weights[i * 4 + k] = dv.getFloat32(b + 56 + k * 4, true)
    }
  }

  const idxByteLen = dv.getUint32(vertStart + vertexBytes, true)
  const idxStart = vertStart + vertexBytes + 4
  const indexCount = idxByteLen / 2
  const indices = new Uint16Array(indexCount)
  for (let i = 0; i < indexCount; i++) indices[i] = dv.getUint16(idxStart + i * 2, true)

  const bones = parseSkeleton(buf, dv)
  const animations = parseAnimations(buf, dv, bones.length)
  // MDLE0002（可选）：贴图空间的静止姿势。
  //   顶点按 MDLS 姿势烘焙，而 UV 对应的是 MDLE 姿势 —— 实测 lainpw 用
  //   World(MDLE) · World(MDLS)⁻¹ 变换顶点，结果与 UV 反推的贴图坐标误差为 0。
  //   动画关键帧 frame 0 恒等于 MDLS 局部矩阵，故动画仍以 MDLS 为基准；
  //   MDLE 只是把烘焙姿势"摆正"到贴图姿势的一次性校正。
  //   绝大多数模型无 MDLE 块，此时校正为单位变换。
  const restLocal = parseBindPose(buf, dv, bones)

  // 蒙皮矩阵 = World(anim) · World(绑定姿势)⁻¹
  //
  // 「绑定姿势」取**主 loop 动画的 frame 0**，而不是 MDLS 骨架块里的绑定矩阵：
  // 顶点是按动画的静止姿势烘焙的。两者一致时（lainpw / 02腿-前，差 ≤0.0001）用哪个
  // 都一样；不一致时必须用 frame0，否则蒙皮在静止状态就把部件推开：
  //   Lucy（50/61 骨不一致）静止偏移 20.1% → 0.6%
  //   头  69.9% → 6.0%
  //   龙  75.9% → 3.0%
  // 判据是「静止偏移」与「动画摆动幅度」分离度：前者远大于后者即静态错位。
  //
  // 已知例外：WLOP DOME GIRL（3113287126）的 frame0 平移几乎全为 0
  // （关键帧平移跨度最大仅 14px，而顶点位移中位数 176px），它的顶点按 MDLS 烘焙，
  // 用 frame0 会残留约 100% 尺度的偏移。试过按残差自动二选一，但代理判据对 Lucy
  // 判错、直接残差又区分不出来，故先统一用 frame0（本机 9/10 个模型正确），
  // 该模型的偏差作为已知待办。
  const invBindWorld = []
  const restCorrection = []
  if (bones.length > 0) {
    const baseAnim = animations.find((a) => a.mode !== 'single') || animations[0] || null
    const baseLocal = bones.map((b, i) => {
      const tr = baseAnim && baseAnim.tracks[i]
      if (tr && tr.frameCount > 0) {
        const k = tr.keyframes
        return composeTRS(k[0], k[1], k[2], k[3], k[4], k[5], k[6], k[7], k[8])
      }
      return Float32Array.from(b.matrix)
    })
    const bindWorld = []
    for (let i = 0; i < bones.length; i++) {
      const p = bones[i].parent
      bindWorld[i] =
        p >= 0 && p < i ? mat4Mul(bindWorld[p], baseLocal[i]) : Float32Array.from(baseLocal[i])
    }
    for (let i = 0; i < bones.length; i++) invBindWorld[i] = mat4Invert(bindWorld[i])

    if (restLocal) {
      const restWorld = []
      for (let i = 0; i < bones.length; i++) {
        const p = bones[i].parent
        restWorld[i] =
          p >= 0 && p < i ? mat4Mul(restWorld[p], restLocal[i]) : Float32Array.from(restLocal[i])
      }
      // correction = World(MDLE) · World(绑定)⁻¹（仅供 bench 校验页参考，不进蒙皮）
      for (let i = 0; i < bones.length; i++) {
        restCorrection[i] = mat4Mul(restWorld[i], invBindWorld[i])
      }
    }
  }

  let minX = Infinity
  let maxX = -Infinity
  let minY = Infinity
  let maxY = -Infinity
  for (let i = 0; i < vertexCount; i++) {
    const x = positions[i * 3]
    const y = positions[i * 3 + 1]
    if (x < minX) minX = x
    if (x > maxX) maxX = x
    if (y < minY) minY = y
    if (y > maxY) maxY = y
  }

  return {
    magic,
    materialPath: mat.value,
    vertexCount,
    positions,
    uvs,
    boneIdx,
    weights,
    indexCount,
    indices,
    bones,
    animations,
    invBindWorld,
    restCorrection: restCorrection.length > 0 ? restCorrection : null,
    bounds: { minX, maxX, minY, maxY },
  }
}

// MDLS0004：魔数(8) + u8 + u32 nextOff + u32 boneCount，逐骨可变长条目
function parseSkeleton(buf, dv) {
  const s = findAscii(buf, 'MDLS')
  if (s < 0) return []
  const boneCount = dv.getUint32(s + 13, true)
  if (boneCount <= 0 || boneCount > 1024) return []
  const bones = []
  let j = s + 17
  for (let b = 0; b < boneCount; b++) {
    if (j + 77 > dv.byteLength) return bones
    const id = dv.getUint32(j + 1, true)
    const parent = dv.getInt32(j + 5, true)
    const matrix = new Float32Array(16)
    for (let k = 0; k < 16; k++) matrix[k] = dv.getFloat32(j + 13 + k * 4, true)
    const meta = readCStr(dv, j + 13 + 64)
    bones.push({ id, parent: parent >= 0 && parent < boneCount ? parent : -1, matrix })
    j = meta.next
  }
  return bones
}

// MDLA0006：魔数(8) + u8 + u32 endPos + u32 animCount，逐动画（末尾 35B 填充）
function parseAnimations(buf, dv, boneCount) {
  const a = findAscii(buf, 'MDLA')
  if (a < 0) return []
  let o = a + 9
  const endPos = dv.getUint32(o, true)
  o += 4
  const animCount = dv.getUint32(o, true)
  o += 4
  if (animCount <= 0 || animCount > 256) return []
  const anims = []
  for (let ai = 0; ai < animCount; ai++) {
    if (o + 16 > dv.byteLength) break
    const id = dv.getUint32(o, true)
    o += 8 // id + unknown
    const nameR = readCStr(dv, o)
    o = nameR.next
    const modeR = readCStr(dv, o)
    o = modeR.next
    if (o + 16 > dv.byteLength) break
    const fps = dv.getFloat32(o, true)
    o += 4
    const frameCount = dv.getUint32(o, true)
    o += 8 // frameCount + unknown
    const trackCount = dv.getUint32(o, true)
    o += 4
    if (trackCount > 1024) break
    const tracks = []
    for (let t = 0; t < trackCount; t++) {
      if (o + 8 > dv.byteLength) break
      o += 4 // boneId 字段实测恒为 0，轨顺序即骨骼顺序
      const trackBytes = dv.getUint32(o, true)
      o += 4
      const n = Math.floor(trackBytes / 36)
      if (o + trackBytes > dv.byteLength) break
      // 关键帧扁平化为 [tx,ty,tz, qx,qy,qz,qw, sx,sy] × n
      const kf = new Float32Array(n * 9)
      for (let f = 0; f < n; f++) {
        const p = o + f * 36
        for (let k = 0; k < 9; k++) kf[f * 9 + k] = dv.getFloat32(p + k * 4, true)
      }
      o += trackBytes
      tracks.push({ frameCount: n, keyframes: kf })
    }
    o += 35 // 动画条目末尾填充
    if (tracks.length > 0 && frameCount > 0 && fps > 0) {
      anims.push({
        id,
        name: nameR.value,
        mode: modeR.value,
        fps,
        frameCount,
        duration: frameCount / fps,
        tracks,
      })
    }
    if (o >= endPos) break
  }
  void boneCount
  return anims
}

// MDLE0002：魔数(8) + u8 + u32 endPos + u32 byteSize + 每骨 64B 绑定姿势局部矩阵
// 顶点数据按这套姿势的世界变换烘焙，故它才是蒙皮的绑定基准；缺失时用 MDLS。
function parseBindPose(buf, dv, bones) {
  if (bones.length === 0) return null
  const e = findAscii(buf, 'MDLE')
  if (e < 0) return null
  const bj = e + 8 + 1 + 4 + 4
  if (bj + bones.length * 64 > dv.byteLength) return null
  const out = []
  for (let b = 0; b < bones.length; b++) {
    const m = new Float32Array(16)
    for (let k = 0; k < 16; k++) m[k] = dv.getFloat32(bj + b * 64 + k * 4, true)
    out.push(m)
  }
  return out
}

// ---------- 姿势求解 ----------

// 在 time（秒）处求 anim 某轨的 TRS 分量，写入 out9 = [tx,ty,tz, qx,qy,qz,qw, sx,sy]。
// 拆出 TRS（不直接出矩阵）是为了让多条动画层能按 WE 语义在 TRS 空间叠加/加权。
function sampleTrackTRS(track, anim, time, out9) {
  const n = track.frameCount
  if (n === 0) {
    out9[0] = 0; out9[1] = 0; out9[2] = 0
    out9[3] = 0; out9[4] = 0; out9[5] = 0; out9[6] = 1
    out9[7] = 1; out9[8] = 1
    return out9
  }
  const frame = time * anim.fps
  let f0
  let f1
  let t
  if (anim.mode === 'loop') {
    // 末帧与首帧重合（frameCount+1 个关键帧），故按 frameCount 环绕可无缝首尾相接
    const span = Math.max(1, n - 1)
    const w = ((frame % span) + span) % span
    f0 = Math.floor(w)
    f1 = (f0 + 1) % n
    t = w - f0
  } else {
    const c = Math.min(Math.max(frame, 0), n - 1)
    f0 = Math.floor(c)
    f1 = Math.min(f0 + 1, n - 1)
    t = c - f0
  }
  const k = track.keyframes
  const a = f0 * 9
  const b = f1 * 9
  const it = 1 - t
  // 四元数取最短弧（相邻帧点积为负时翻转，避免绕远路产生瞬时抖动）
  let dot = 0
  for (let i = 3; i < 7; i++) dot += k[a + i] * k[b + i]
  const s = dot < 0 ? -1 : 1
  out9[0] = k[a] * it + k[b] * t
  out9[1] = k[a + 1] * it + k[b + 1] * t
  out9[2] = k[a + 2] * it + k[b + 2] * t
  out9[3] = k[a + 3] * it + k[b + 3] * t * s
  out9[4] = k[a + 4] * it + k[b + 4] * t * s
  out9[5] = k[a + 5] * it + k[b + 5] * t * s
  out9[6] = k[a + 6] * it + k[b + 6] * t * s
  out9[7] = k[a + 7] * it + k[b + 7] * t
  out9[8] = k[a + 8] * it + k[b + 8] * t
  return out9
}

// 在 time 处求 anim 的骨骼局部变换矩阵（保留原签名，供单层路径与外部使用）
function sampleTrack(track, anim, time, out) {
  const v = sampleTrack._t || (sampleTrack._t = new Float32Array(9))
  sampleTrackTRS(track, anim, time, v)
  return composeTRS(v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8], out)
}

// 骨骼绑定姿势的 TRS 基准：平移取绑定矩阵第 4 列，旋转取单位、缩放取 1。
// 依据 MDLA 实测——每条动画的 frame 0 平移与绑定矩阵完全相等（误差 ≤3e-5），
// 且各层只在自己驱动的骨骼上偏离该基准。因此「相对基准的偏移」就是这一层的贡献。
function bindTRS(boneMatrix, out9) {
  out9[0] = boneMatrix[12]
  out9[1] = boneMatrix[13]
  out9[2] = boneMatrix[14]
  out9[3] = 0; out9[4] = 0; out9[5] = 0; out9[6] = 1
  out9[7] = 1; out9[8] = 1
  return out9
}


// 计算 time 处的蒙皮矩阵数组（World(anim) · invBindWorld），写入 mdl._skinCache
export function computeSkinMatrices(mdl, time, animLayers) {
  const bones = mdl.bones
  const count = bones.length
  if (count === 0) return null
  if (!mdl._skin || mdl._skin.length !== count * 16) {
    mdl._skin = new Float32Array(count * 16)
    mdl._local = []
    mdl._world = []
    mdl._tmp = new Float32Array(16)
    mdl._tmp2 = new Float32Array(16)
    for (let i = 0; i < count; i++) {
      mdl._local.push(new Float32Array(16))
      mdl._world.push(new Float32Array(16))
    }
  }
  // ---- 逐动画层求局部姿势 ----
  // scene.json 的 animationlayers 是一个**列表**，每条含 animation(id) / additive /
  // blend / rate / visible，需要全部叠加。此前只取第一条可见层，于是像 lainpw 那样
  // 用 kopf/arm/body 三条动画分别驱动头(骨8)、手臂(骨4,5,6)、身体(骨1) 的模型，
  // 只播 kopf 会让其余骨骼被这条动画的轨道按绑定姿势覆写 —— 表现就是五官、头发、
  // 躯体各自错位僵死。
  //
  // 叠加规则（TRS 空间）：
  //   base 取绑定姿势（frame0 实测等于绑定矩阵），
  //   每层贡献 = (该层采样 - base) × blend，additive 层累加，
  //   非 additive 层按 blend 覆盖式混合（mix(base, sample, blend)）。
  //   rate 逐层生效（各层动画速度不同，如 lainpw 的 0.4 / 0.5 / 0.5）。
  const layers = []
  if (animLayers && animLayers.length > 0) {
    for (const l of animLayers) {
      if (l.visible === false) continue
      const a = mdl.animations.find((x) => x.id === l.animation)
      if (a) layers.push({ anim: a, additive: !!l.additive, blend: typeof l.blend === 'number' ? l.blend : 1, rate: typeof l.rate === 'number' ? l.rate : 1 })
    }
  }
  // 没有可用的层信息时退回第一条动画（保持旧行为）
  if (layers.length === 0) {
    const a = mdl.animations[0]
    if (!a) return null
    layers.push({ anim: a, additive: false, blend: 1, rate: 1 })
  }

  const base = mdl._trsBase || (mdl._trsBase = new Float32Array(9))
  const acc = mdl._trsAcc || (mdl._trsAcc = new Float32Array(9))
  const smp = mdl._trsSmp || (mdl._trsSmp = new Float32Array(9))

  for (let i = 0; i < count; i++) {
    bindTRS(bones[i].matrix, base)
    acc.set(base)
    let touched = false
    let addW = 0
    for (const L of layers) {
      const track = L.anim.tracks[i]
      if (!track) continue
      sampleTrackTRS(track, L.anim, time * L.rate, smp)
      const w = L.blend
      if (L.additive) {
        // 相对绑定姿势的增量按权重累加（平移/缩放线性、旋转按四元数分量线性后归一）
        for (let k = 0; k < 9; k++) acc[k] += (smp[k] - base[k]) * w
        addW += w
      } else {
        for (let k = 0; k < 9; k++) acc[k] = acc[k] * (1 - w) + smp[k] * w
      }
      touched = true
    }
    // additive 权重总和 >1 时按总权重归一：Lucy 那样 5 条 additive 层（blend 均为 1）
    // 若直接累加，同一根骨的增量会被叠 5 次，人物幅度被放大到形体明显走形。
    // WE 的 additive 混合是加权平均而非无界累加，故超过 1 时整体收缩回 1。
    if (addW > 1) {
      for (let k = 0; k < 9; k++) acc[k] = base[k] + (acc[k] - base[k]) / addW
    }
    if (touched) {
      composeTRS(acc[0], acc[1], acc[2], acc[3], acc[4], acc[5], acc[6], acc[7], acc[8], mdl._local[i])
    } else {
      // 所有层都没有这根骨的轨道：保持绑定姿势
      mdl._local[i].set(bones[i].matrix)
    }
  }

  for (let i = 0; i < count; i++) {
    const p = bones[i].parent
    if (p >= 0 && p < i) mat4Mul(mdl._world[p], mdl._local[i], mdl._world[i])
    else mdl._world[i].set(mdl._local[i])
  }
  for (let i = 0; i < count; i++) {
    // 蒙皮矩阵 = World(anim) · World(绑定姿势)⁻¹，静止时为单位变换。
    //
    // 不要再乘 MDLE 校正（restCorrection）。MDLE 存的是**贴图空间的静止姿势**：
    // 作者把眼睛/头发/手臂等部件在贴图里分散摆放，MDLE 描述的正是「网格姿态 →
    // 贴图里那些分散位置」的搬运。渲染要的恰好相反 —— 顶点保持网格姿态（人物
    // 完整），UV 直接去贴图对应区域取样即可，不需要任何搬运。
    //
    // 实测 3787341007（lainpw）：乘上 restCorrection 会把眼睛（骨14/15）推到左边
    // 426px/360px、头发（骨12）推到右边 345px、头部（骨9/11）上移，躯干（骨0）不动
    // —— 正是「双眼在人物左边很远处独立存在、头发在右上方、头与脖子断开」的现象。
    // 不乘时顶点偏离原网格为 0px。取逆同样是 426px（方向不同、幅度一样），也不对。
    // 佐证：本机唯一一直正常的 puppet 模型（3791428510 各层）都没有 MDLE 块。
    mat4Mul(mdl._world[i], mdl.invBindWorld[i], mdl._tmp)
    mdl._skin.set(mdl._tmp, i * 16)
  }
  return mdl._skin
}

// ---------- 渲染 ----------

// 蒙皮上限。此前设为 24 并注明「实测最多 16 骨」，实际本机库里 WLOP DOME GIRL 有 64 骨、
// Lucy 有 61 骨：超限骨骼在顶点着色器里被 `bi >= u_boneCount` 跳过，权重丢失，
// 顶点落到错误位置 —— 表现为人物五官/头发/躯体撕裂错位。
//
// WebGL2 只保证 MAX_VERTEX_UNIFORM_VECTORS ≥ 256 个 vec4（mat4 占 4 个 → 64 骨），
// 但实测本机 ANGLE/Metal 上报 1024。故不写死上限：createMDLRenderer 按 GPU 实际
// 上报值算出可用骨数（留 32 个 vec4 给 u_mvp 等），链接失败时逐级减半回退。
const MAX_BONES_HARD_CAP = 128
const MAX_BONES_FLOOR = 24

function boneBudget(gl) {
  const vecs = gl.getParameter(gl.MAX_VERTEX_UNIFORM_VECTORS) || 256
  const usable = Math.floor((vecs - 32) / 4)
  return Math.max(MAX_BONES_FLOOR, Math.min(MAX_BONES_HARD_CAP, usable))
}

// 顶点着色器在 GPU 上做蒙皮。网格坐标为「图层局部像素、y 轴朝上」，
// u_mvp 由宿主构造（含 y 方向、图层旋转缩放、场景投影），故这里直传 xy 即可。
// UV 恒等直传：朝向差异一律由 u_mvp 的几何翻转表达，翻 UV 会把贴图镜像到未翻转的网格上。
const mdlVertSrc = (maxBones) => `#version 300 es
in vec3 a_pos;
in vec2 a_uv;
in vec4 a_bone;
in vec4 a_weight;
uniform mat4 u_mvp;
uniform mat4 u_skin[${maxBones}];
uniform int u_boneCount;
out vec2 v_uv;
void main() {
  vec4 p = vec4(a_pos, 1.0);
  vec4 skinned = vec4(0.0);
  float total = 0.0;
  if (u_boneCount > 0) {
    for (int i = 0; i < 4; i++) {
      float w = a_weight[i];
      if (w <= 0.0) continue;
      int bi = int(a_bone[i]);
      if (bi < 0 || bi >= u_boneCount) continue;
      skinned += (u_skin[bi] * p) * w;
      total += w;
    }
  }
  vec4 local = total > 0.0 ? skinned / total : p;
  gl_Position = u_mvp * vec4(local.xy, 0.0, 1.0);
  v_uv = a_uv;
}`

const MDL_FRAG = `#version 300 es
precision mediump float;
in vec2 v_uv;
uniform sampler2D u_tex;
uniform vec4 u_color;
out vec4 fragColor;
void main() {
  vec4 t = texture(u_tex, v_uv);
  fragColor = t * u_color;
}`

export function createMDLRenderer(gl) {
  const compile = (type, src) => {
    const s = gl.createShader(type)
    gl.shaderSource(s, src)
    gl.compileShader(s)
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
      throw new Error('MDL 着色器编译失败: ' + gl.getShaderInfoLog(s))
    }
    return s
  }
  // 按 GPU 上报的 uniform 预算编译；链接失败（驱动实际容量更小）时逐级减半重试
  let maxBones = boneBudget(gl)
  let prog = null
  for (;;) {
    const p = gl.createProgram()
    let linked = false
    try {
      gl.attachShader(p, compile(gl.VERTEX_SHADER, mdlVertSrc(maxBones)))
      gl.attachShader(p, compile(gl.FRAGMENT_SHADER, MDL_FRAG))
      gl.bindAttribLocation(p, 0, 'a_pos')
      gl.bindAttribLocation(p, 1, 'a_uv')
      gl.bindAttribLocation(p, 2, 'a_bone')
      gl.bindAttribLocation(p, 3, 'a_weight')
      gl.linkProgram(p)
      linked = !!gl.getProgramParameter(p, gl.LINK_STATUS)
    } catch (e) {
      linked = false
    }
    if (linked) { prog = p; break }
    gl.deleteProgram(p)
    if (maxBones <= MAX_BONES_FLOOR) {
      throw new Error('MDL 着色器链接失败（骨数已降到 ' + MAX_BONES_FLOOR + '）')
    }
    maxBones = Math.max(MAX_BONES_FLOOR, maxBones >> 1)
  }
  const MAX_BONES = maxBones

  const uni = {
    mvp: gl.getUniformLocation(prog, 'u_mvp'),
    tex: gl.getUniformLocation(prog, 'u_tex'),
    skin: gl.getUniformLocation(prog, 'u_skin'),
    boneCount: gl.getUniformLocation(prog, 'u_boneCount'),
    color: gl.getUniformLocation(prog, 'u_color'),
  }
  const identitySkin = new Float32Array(MAX_BONES * 16)
  for (let i = 0; i < MAX_BONES; i++) identitySkin.set(IDENTITY, i * 16)

  // 每个网格一套 VAO/VBO（顶点数据静态，蒙皮在 GPU 完成）
  const meshes = new WeakMap()
  function ensureMesh(mdl) {
    let m = meshes.get(mdl)
    if (m) return m
    const vao = gl.createVertexArray()
    gl.bindVertexArray(vao)
    const n = mdl.vertexCount
    // 交错：pos(3) uv(2) bone(4) weight(4) = 13 float / 52B
    const data = new Float32Array(n * 13)
    for (let i = 0; i < n; i++) {
      const o = i * 13
      data[o] = mdl.positions[i * 3]
      data[o + 1] = mdl.positions[i * 3 + 1]
      data[o + 2] = mdl.positions[i * 3 + 2]
      data[o + 3] = mdl.uvs[i * 2]
      data[o + 4] = mdl.uvs[i * 2 + 1]
      for (let k = 0; k < 4; k++) {
        data[o + 5 + k] = mdl.boneIdx[i * 4 + k]
        data[o + 9 + k] = mdl.weights[i * 4 + k]
      }
    }
    const vbuf = gl.createBuffer()
    gl.bindBuffer(gl.ARRAY_BUFFER, vbuf)
    gl.bufferData(gl.ARRAY_BUFFER, data, gl.STATIC_DRAW)
    const S = 52
    gl.enableVertexAttribArray(0)
    gl.vertexAttribPointer(0, 3, gl.FLOAT, false, S, 0)
    gl.enableVertexAttribArray(1)
    gl.vertexAttribPointer(1, 2, gl.FLOAT, false, S, 12)
    gl.enableVertexAttribArray(2)
    gl.vertexAttribPointer(2, 4, gl.FLOAT, false, S, 20)
    gl.enableVertexAttribArray(3)
    gl.vertexAttribPointer(3, 4, gl.FLOAT, false, S, 36)
    const ibuf = gl.createBuffer()
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, ibuf)
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, mdl.indices, gl.STATIC_DRAW)
    gl.bindVertexArray(null)
    m = { vao, vbuf, ibuf }
    meshes.set(mdl, m)
    return m
  }

  return {
    gl,
    prog,
    maxBones: MAX_BONES,
    upload(mdl) {
      ensureMesh(mdl)
    },
    // opts: { color:[r,g,b,a], time, animLayers, blending }
    draw(mvp, mdl, opts, texture) {
      const m = ensureMesh(mdl)
      gl.useProgram(prog)
      gl.bindVertexArray(m.vao)
      gl.activeTexture(gl.TEXTURE0)
      gl.bindTexture(gl.TEXTURE_2D, texture && texture.glTex ? texture.glTex : texture)
      gl.uniform1i(uni.tex, 0)
      gl.uniformMatrix4fv(uni.mvp, false, mvp)
      const col = opts.color || [1, 1, 1, 1]
      gl.uniform4f(uni.color, col[0], col[1], col[2], col[3])
      const skin = computeSkinMatrices(mdl, opts.time || 0, opts.animLayers)
      const count = Math.min(mdl.bones.length, MAX_BONES)
      if (skin && count > 0) {
        gl.uniformMatrix4fv(uni.skin, false, skin.subarray(0, count * 16))
        gl.uniform1i(uni.boneCount, count)
      } else {
        gl.uniformMatrix4fv(uni.skin, false, identitySkin)
        gl.uniform1i(uni.boneCount, 0)
      }
      if (opts.blending === 'additive') {
        gl.enable(gl.BLEND)
        gl.blendFuncSeparate(gl.SRC_ALPHA, gl.ONE, gl.ONE, gl.ONE)
      } else if (opts.blending === 'normal') {
        gl.disable(gl.BLEND)
      } else {
        gl.enable(gl.BLEND)
        gl.blendFuncSeparate(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA, gl.ONE, gl.ONE_MINUS_SRC_ALPHA)
      }
      gl.drawElements(gl.TRIANGLES, mdl.indexCount, gl.UNSIGNED_SHORT, 0)
      gl.bindVertexArray(null)
    },
  }
}
