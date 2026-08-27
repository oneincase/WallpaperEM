// 场景对象模型：scene.json + project.json → 归一化图层列表
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

export function parseScene(sceneJson, project) {
  const properties = (project && project.general && project.general.properties) || {}
  const objects = sceneJson.objects || []

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

  const layers = objects.map((o, i) => {
    const world = local[i]
    return {
      id: o.id !== undefined ? o.id : i,
      name: o.name || '',
      visible: parseBool(o.visible, true),
      image: typeof o.image === 'string' ? o.image : null,
      particle: typeof o.particle === 'string' ? o.particle : null,
      // [we-scene patch] 粒子参数覆盖 + 声音 + 文字 + 组件
      instanceoverride: o.instanceoverride || null,
      sound: Array.isArray(o.sound) ? o.sound.filter((s) => typeof s === 'string') : [],
      soundprops: {
        volume: typeof o.volume === 'number' ? o.volume : 1,
        playbackmode: o.playbackmode || 'single',
        startsilent: parseBool(o.startsilent, false),
        maxtime: typeof o.maxtime === 'number' ? o.maxtime : 0,
        mintime: typeof o.mintime === 'number' ? o.mintime : 0,
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
      textPointsize: typeof o.pointsize === 'number' ? o.pointsize : (o.pointsize && o.pointsize.value) || 24,
      textColor: parseColor(o.color),
      textMaxwidth: typeof o.maxwidth === 'number' ? o.maxwidth : (o.maxwidth && o.maxwidth.value) || 0,
      textMaxrows: typeof o.maxrows === 'number' ? o.maxrows : (o.maxrows && o.maxrows.value) || 0,
      textHAlign: o.horizontalalign || 'center',
      textVAlign: o.verticalalign || 'center',
      textSpacing: o.spacing ? parseVec2(o.spacing) : [0, 0],
      isSound: !!(o.sound && o.sound.length),
      isComponent: !!o.component,
      // WE 的 solid 层：无 image/particle，或 image 指向内置 models/util/*（纯色层，无纹理）
      solid: !!o.solid && typeof o.particle !== 'string' && (typeof o.image !== 'string' || o.image.indexOf('models/util/') === 0),
      // composelayer 是分组容器（子层已合并为世界坐标），容器自身不渲染
      isContainer: typeof o.image === 'string' && o.image.indexOf('models/util/composelayer') === 0,
      origin: world.origin,
      scale: world.scale,
      angles: world.angles,
      size: parseVec2(o.size || '0 0'),
      alignment: o.alignment || 'center',
      color: parseColor(o.color),
      alpha: typeof o.alpha === 'number' ? o.alpha : 1,
      brightness: typeof o.brightness === 'number' ? o.brightness : 1,
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
