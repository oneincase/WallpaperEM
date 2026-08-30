// 场景对象模型：scene.json + project.json → 归一化图层列表
//
// [we-scene patch] 用户属性解引用（resolveUserValue）
//
// scene.json 里受壁纸自定义属性控制的字段形如 `{"user": "bgcolor", "value": "0.1 0.2 0.3"}`：
// `user` 是 project.json `general.properties` 里的属性名，`value` 是场景保存时的快照。
//
// 快照与 project.json 默认值并非总是相等 —— 本机 78 个场景的 2598 处引用里有 372 处不等
// （作者改过属性默认值却没重存场景，或字段名与属性名撞车，如某场景的 visible 绑到一个
// color 属性上）。因此不能无条件改读属性表：那会让用户什么都没改，画面就先变了样。
//
// 规则：默认沿用场景内快照；仅当宿主在 project.json 上标记了 `userOverridden`
// （即用户显式改过该属性）时才采用属性表的值。无标记的 project.json 行为与改动前完全一致。
function propEntry(properties, name) {
  if (!properties || typeof name !== 'string') return null
  const p = properties[name]
  return p && typeof p === 'object' ? p : null
}

/** 取字段生效值：用户改过则用属性表的值，否则用场景快照 */
function resolveUserValue(v, properties) {
  if (v === null || typeof v !== 'object' || typeof v.user !== 'string') return v
  const p = propEntry(properties, v.user)
  if (p && p.userOverridden && p.value !== undefined && p.value !== null) return p.value
  return v.value
}

// 递归把对象树里的 {user, value} 包装解成生效值。就地改写 `value` 字段而非拆掉包装，
// 使所有下游读取方（parseVec3 / parseColor / 渲染器的 setConstant 等，都走 `.value`）
// 无需改动。嵌套包装（真实数据里存在 value 本身又是 {user,value} 的情形）一并展开。
function resolveUserProps(node, properties, depth) {
  if (depth > 32 || node === null || typeof node !== 'object') return node
  if (Array.isArray(node)) {
    for (let i = 0; i < node.length; i++) node[i] = resolveUserProps(node[i], properties, depth + 1)
    return node
  }
  if (typeof node.user === 'string') {
    let v = resolveUserValue(node, properties)
    // 解出的值可能仍是包装对象：继续展开，直到拿到标量/普通对象
    for (let guard = 0; guard < 8 && v !== null && typeof v === 'object' && typeof v.user === 'string'; guard++) {
      v = resolveUserValue(v, properties)
    }
    node.value = resolveUserProps(v, properties, depth + 1)
    return node
  }
  for (const k of Object.keys(node)) node[k] = resolveUserProps(node[k], properties, depth + 1)
  return node
}

// [we-scene patch] parseVec3/parseVec2 支持 {user, value} 用户属性包装对象（与 parseColor 一致）
export function parseVec3(s) {
  if (typeof s === 'object' && s !== null) s = s.value
  const p = String(s ?? '').trim().split(/\s+/).map(Number)
  return [p[0] || 0, p[1] || 0, p[2] || 0]
}

export function parseVec2(s) {
  if (typeof s === 'object' && s !== null) s = s.value
  const p = String(s ?? '').trim().split(/\s+/).map(Number)
  return [p[0] || 0, p[1] || 0]
}

export function parseColor(c) {
  if (c === undefined || c === null) return [1, 1, 1]
  if (typeof c === 'string') return parseVec3(c)
  if (typeof c === 'object' && c !== null) return parseVec3(c.value)
  return [1, 1, 1]
}

export function parseBool(v, dflt = false) {
  if (v === undefined || v === null) return dflt
  if (typeof v === 'boolean') return v
  if (typeof v === 'object' && v !== null) return !!v.value
  return dflt
}

// [we-scene patch] 数值字段：同样要拆 {user, value} 包装。
// 原版对 alpha / brightness / volume / pointsize / animationlayers.rate 一律用
// `typeof o.x === 'number'` 判定，包装对象直接落到默认值 —— 本机 78 个场景里
// alpha 有 188 处绑属性，其中 62 处快照值不是 1（含 2 处 alpha=0），被强制成 1 后
// 这些半透明层（水汽、光晕、暗角）画成了完全不透明，甚至该隐藏的层照样显示。
export function parseNum(v, dflt) {
  if (typeof v === 'number') return v
  if (v !== null && typeof v === 'object') {
    const n = Number(v.value)
    return Number.isFinite(n) ? n : dflt
  }
  return dflt
}

export function parseScene(sceneJson, project) {
  const properties = (project && project.general && project.general.properties) || {}
  const objects = sceneJson.objects || []
  // [we-scene patch] 先把所有 {user, value} 包装解成生效值（详见文件头注释），
  // 之后的字段读取一律拿到已解引用的值。
  resolveUserProps(objects, properties, 0)
  resolveUserProps(sceneJson.general || {}, properties, 0)

  // ---- 父子层级：子对象坐标是相对父级的局部坐标，需合并到世界坐标 ----
  // WE 语义：子 origin 相对父原点；父旋转/缩放作用于子。父级无动画时静态合并等价。
  const byId = new Map()
  for (const o of objects) {
    if (o.id !== undefined) byId.set(o.id, o)
  }
  const local = objects.map((o) => ({
    id: o.id,
    parent: o.parent,
    origin: parseVec3(o.origin || '0 0 0'),
    scale: parseVec3(o.scale || '1 1 1'),
    angles: parseVec3(o.angles || '0 0 0'),
  }))
  // 自底向上迭代合并（层级深时循环至收敛）
  for (let pass = 0; pass < 8; pass++) {
    let changed = false
    for (const c of local) {
      if (c.parent === undefined || c.parent === null) continue
      const p = byId.get(c.parent)
      if (!p) continue
      const pIdx = local.findIndex((x) => x.id === c.parent)
      if (pIdx < 0) continue
      const pc = local[pIdx]
      if (pc.parent !== undefined && pc.parent !== null) continue // 父级还未合并完成，下一轮
      // 父级已合并：应用父变换（旋转仅考虑 z；WE 2D 层只用 z 旋转）
      const ca = (pc.angles[2] * Math.PI) / 180
      const cos = Math.cos(ca)
      const sin = Math.sin(ca)
      const ox = c.origin[0] * pc.scale[0]
      const oy = c.origin[1] * pc.scale[1]
      c.origin[0] = pc.origin[0] + ox * cos - oy * sin
      c.origin[1] = pc.origin[1] + ox * sin + oy * cos
      c.origin[2] = pc.origin[2] + c.origin[2]
      c.angles[2] = pc.angles[2] + c.angles[2]
      c.scale[0] = pc.scale[0] * c.scale[0]
      c.scale[1] = pc.scale[1] * c.scale[1]
      c.scale[2] = pc.scale[2] * c.scale[2]
      c.parent = null // 标记已合并
      changed = true
    }
    if (!changed) break
  }

  // [we-scene patch] 可见性沿父链继承：父级隐藏时其所有后代都不渲染。
  // WE 里 visible 常绑用户属性（如 language / clocklocation）来切换整组图层，
  // 只看自身 visible 会把 5 套隐藏的语言变体一起画出来——其中的半透明水面层
  // （ripple1440p）被反复叠加上百次，把画面糊成一片灰，看起来就像蒙了层蒙版。
  const visibleSelf = objects.map((o) => parseBool(o.visible, true))
  const idxById = new Map()
  objects.forEach((o, i) => {
    if (o.id !== undefined) idxById.set(o.id, i)
  })
  const effVisible = objects.map(() => true)
  for (let i = 0; i < objects.length; i++) {
    let vis = visibleSelf[i]
    let p = objects[i].parent
    // 沿父链上溯，任一祖先隐藏则本层隐藏；深度设上限以防数据里存在环
    for (let guard = 0; vis && p !== undefined && p !== null && guard < 64; guard++) {
      const pi = idxById.get(p)
      if (pi === undefined) break
      if (!visibleSelf[pi]) vis = false
      p = objects[pi].parent
    }
    effVisible[i] = vis
  }

  const layers = objects.map((o, i) => {
    const world = local[i]
    return {
      id: o.id !== undefined ? o.id : i,
      name: o.name || '',
      visible: effVisible[i],
      image: typeof o.image === 'string' ? o.image : null,
      particle: typeof o.particle === 'string' ? o.particle : null,
      // [we-scene patch] 粒子参数覆盖 + 声音 + 文字 + 组件
      instanceoverride: o.instanceoverride || null,
      sound: Array.isArray(o.sound) ? o.sound.filter((s) => typeof s === 'string') : [],
      soundprops: {
        volume: parseNum(o.volume, 1),
        playbackmode: o.playbackmode || 'single',
        startsilent: parseBool(o.startsilent, false),
        maxtime: parseNum(o.maxtime, 0),
        mintime: parseNum(o.mintime, 0),
        spatialization: parseBool(o.spatialization, false),
        muteineditor: parseBool(o.muteineditor, false),
      },
      text: typeof o.text === 'string' ? o.text : (o.text && typeof o.text === 'object' ? o.text.value : null),
      textScript: o.text && typeof o.text === 'object' ? (o.text.script || null) : null,
      textScriptProps: o.text && typeof o.text === 'object' ? (o.text.scriptproperties || null) : null,
      component: typeof o.component === 'string' ? o.component : null,
      isText: !!(o.text !== undefined && o.text !== null),
      // 文字布局属性
      textFont: typeof o.font === 'string' ? o.font : null,
      textPointsize: parseNum(o.pointsize, 24) || 24,
      textColor: parseColor(o.color),
      textMaxwidth: parseNum(o.maxwidth, 0),
      textMaxrows: parseNum(o.maxrows, 0),
      textHAlign: o.horizontalalign || 'center',
      textVAlign: o.verticalalign || 'center',
      textSpacing: o.spacing ? parseVec2(o.spacing) : [0, 0],
      isSound: !!(o.sound && o.sound.length),
      isComponent: !!o.component,
      // [we-scene patch] puppet 骨骼动画层：animation 为 MDLA 里的动画 id
      animationLayers: (o.animationlayers || [])
        .filter((a) => a && typeof a.animation === 'number')
        .map((a) => ({
          animation: a.animation,
          visible: parseBool(a.visible, true),
          additive: parseBool(a.additive, false),
          blend: parseNum(a.blend, 1),
          rate: parseNum(a.rate, 1),
        })),
      // WE 的 solid 层：无 image/particle，或 image 指向内置 models/util/*（纯色层，无纹理）
      solid: !!o.solid && typeof o.particle !== 'string' && (typeof o.image !== 'string' || o.image.indexOf('models/util/') === 0),
      // composelayer 是分组容器（子层已合并为世界坐标），容器自身不渲染
      isContainer: typeof o.image === 'string' && o.image.indexOf('models/util/composelayer') === 0,
      // [we-scene patch] projectlayer / fullscreenlayer 是 WE 的**全屏后期处理层**：
      // 层内容 = 当前已渲染的画面（copybackground），套效果后再合成回画布。
      // 渲染器还没有实现「把画布回读为层内容」，此前把它们当普通空层渲染，
      // 于是 blur 效果末尾的 compositecolor "1 1 1" 直接把整屏铺成纯白
      // （3113287126 的 Full Composition Layer / Post-Processing Layer 即此）。
      // 正确的降级是跳过这类层：不加后期远好于糊掉整个画面。
      isPostProcess:
        typeof o.image === 'string' &&
        (o.image.indexOf('models/util/projectlayer') === 0 || o.image.indexOf('models/util/fullscreenlayer') === 0),
      origin: world.origin,
      scale: world.scale,
      angles: world.angles,
      size: parseVec2(o.size || '0 0'),
      alignment: o.alignment || 'center',
      color: parseColor(o.color),
      alpha: parseNum(o.alpha, 1),
      brightness: parseNum(o.brightness, 1),
      copybackground: !!o.copybackground,
      colorBlendMode: o.colorBlendMode || 0,
      // 视差深度（vec2：x/y 方向分量；近景正值位移大、远景负值反向）
      parallaxDepth: o.parallaxDepth !== undefined ? parseVec2(o.parallaxDepth) : null,
      effects: (o.effects || []).map((e) => ({
        file: e.file || '',
        visible: parseBool(e.visible, true),
        passes: (e.passes || []).map((p) => ({
          combos: p.combos || {},
          constantshadervalues: p.constantshadervalues || {},
          textures: p.textures || [],
        })),
      })),
    }
  })
  return {
    camera: sceneJson.camera || null,
    general: sceneJson.general || {},
    layers,
    properties,
  }
}

// 从对象模型解析材质链：object.image → models/x.json → materials/y.json → passes
export function resolveMaterial(modelJson) {
  if (!modelJson || typeof modelJson.material !== 'string') return null
  return {
    materialPath: modelJson.material,
    autosize: !!modelJson.autosize,
    cropoffset: modelJson.cropoffset ? parseVec2(modelJson.cropoffset) : null,
  }
}
