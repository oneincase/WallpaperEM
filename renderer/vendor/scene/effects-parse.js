import { getEntry } from '../pkg/container.js'
// 解析效果的 material 链：effects/<name>/effect.json → materials/effects/*.json 的 passes
// 产出 layer.effects[i] 的 { materialPasses, fbos, binds }，供通用 pass 管线使用。

// pkg: parsePkg 结果；effect: scene.json 的效果条目（file/passes/visible）
export function resolveEffectChain(pkg, effect, readText) {
  const entry = getEntry(pkg, effect.file)
  if (entry === null) return
  let ej
  try {
    ej = JSON.parse(readText(entry))
  } catch (e) {
    return
  }
  effect.fbos = ej.fbos || []
  effect.materialPasses = (ej.passes || []).map((p) => {
    if (!p.material) {
      // 无 material 的 copy 命令 pass（罕见）：构造虚拟 copy pass
      return {
        shader: null,
        copyCommand: true,
        target: p.target || null,
        binds: p.bind || [],
        blending: 'normal',
        textures: [],
        combos: {},
        constants: {},
      }
    }
    const me = getEntry(pkg, p.material)
    if (me === null) {
      return { shader: null, copyCommand: false, target: p.target || null, binds: p.bind || [], blending: 'normal', textures: [], combos: {}, constants: {} }
    }
    const mj = JSON.parse(readText(me))
    const mp = (mj.passes && mj.passes[0]) || {}
    return {
      shader: mp.shader || null,
      copyCommand: false,
      target: p.target || null,
      binds: p.bind || [],
      blending: mp.blending || 'normal',
      textures: mp.textures || [],
      combos: mp.combos || {},
      constants: mp.constantshadervalues || {},
    }
  })
}

// 内置模型（pkg 内没有 models/util/*）：返回内置 material 路径或 null
export const BUILTIN_MODELS = {
  'models/util/solidlayer.json': { material: 'materials/util/solidlayer.json' },
}

// 内置 material（pkg 内没有 materials/util/*）：返回 passes 定义或 null
export const BUILTIN_MATERIALS = {
  'materials/util/solidlayer.json': {
    passes: [{ shader: 'flat', blending: 'translucent', cullmode: 'nocull', depthtest: 'disabled', depthwrite: 'disabled', textures: [], combos: {} }],
  },
}
