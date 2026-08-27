// WE shader（HLSL 方言）→ GLSL ES 3.0 转译器
// 覆盖 WE 效果 shader 的实际语法面：预处理（#include/#define/#if combo）+ 方言转换。
// 依据：linux-wallpaperengine 的 GLSLContext 思路 + 本仓库提取的全部效果 shader 实测语法。
// mul 语义：HLSL 行向量约定 → GLSL 列向量约定（transpose 处理）；效果 pass 的 MVP=单位矩阵时二者等价。

// ---------- 预处理 ----------

// 收集宏定义（对象宏 + 函数宏）
function collectMacros(src) {
  const defs = new Map()
  const fns = new Map()
  const re = /^[ \t]*#define[ \t]+([A-Za-z_][A-Za-z0-9_]*)(?:\(([^)]*)\))?[ \t]*(.*)$/gm
  let m
  while ((m = re.exec(src)) !== null) {
    if (m[2] !== undefined) {
      fns.set(m[1], { args: m[2].split(',').map((s) => s.trim()).filter(Boolean), body: m[3].trim() })
    } else {
      defs.set(m[1], m[3].trim())
    }
  }
  return { defs, fns }
}

// 函数宏展开（平衡括号取参，递归深度限制）
function expandFunctionMacro(text, name, info, depth) {
  const out = []
  let i = 0
  while (i < text.length) {
    const idx = text.indexOf(name, i)
    if (idx === -1) {
      out.push(text.slice(i))
      break
    }
    out.push(text.slice(i, idx))
    const p = idx + name.length
    // 必须是函数调用形式：下一个非空白字符是 '('
    let q = p
    while (q < text.length && /\s/.test(text[q])) q++
    if (text[q] !== '(') {
      out.push(text.slice(idx, q))
      i = q
      continue
    }
    // 平衡括号取参数
    let depthCount = 0
    let end = q
    for (; end < text.length; end++) {
      if (text[end] === '(') depthCount++
      else if (text[end] === ')') {
        depthCount--
        if (depthCount === 0) break
      }
    }
    if (end >= text.length) {
      out.push(text.slice(idx))
      break
    }
    const argsStr = text.slice(q + 1, end)
    const args = splitArgs(argsStr)
    let body = info.body
    info.args.forEach((name, k) => {
      const val = args[k] !== undefined ? args[k].trim() : ''
      body = replaceWord(body, name, val)
    })
    if (depth > 0) body = expandMacrosIn(body, depth - 1)
    out.push('(' + body + ')')
    i = end + 1
  }
  return out.join('')
}

function replaceWord(text, word, replacement) {
  return text.replace(new RegExp('\\b' + word.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + '\\b', 'g'), replacement)
}

function splitArgs(s) {
  const out = []
  let depth = 0
  let cur = ''
  for (let i = 0; i < s.length; i++) {
    const c = s[i]
    if (c === '(') depth++
    else if (c === ')') depth--
    if (c === ',' && depth === 0) {
      out.push(cur)
      cur = ''
    } else {
      cur += c
    }
  }
  if (cur.trim() !== '') out.push(cur)
  return out
}

// 展开宏（对象宏 + 函数宏），多轮迭代直到无宏残留（宏可互相引用）。
// 预处理行（# 开头）不展开，避免 #define 行自身被误当作调用。
function expandMacrosIn(text, depth) {
  for (let round = 0; round < 12; round++) {
    const { defs, fns } = collectMacros(text)
    if (defs.size === 0 && fns.size === 0) break
    const lines = text.split('\n')
    let changed = false
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i]
      if (/^[ \t]*#/.test(line)) continue
      let l = line
      for (const [name, val] of defs) {
        const re = new RegExp('\\b' + name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + '\\b')
        if (re.test(l)) {
          l = replaceWord(l, name, val)
          changed = true
        }
      }
      for (const [name, info] of fns) {
        if (l.includes(name)) {
          l = expandFunctionMacro(l, name, info, depth)
          changed = true
        }
      }
      lines[i] = l
    }
    text = lines.join('\n')
    if (!changed) break
  }
  return text
}

// 求值 #if 表达式（安全自写求值器：|| && ! ( ) == != < > <= >= 数字 标识符）
function evalIfExpr(expr, combos, defs) {
  const resolve = (name) => {
    if (combos[name] !== undefined) return String(combos[name])
    if (defs.has(name)) return '(' + defs.get(name) + ')'
    return '0'
  }
  let s = expr.replace(/\b[A-Za-z_][A-Za-z0-9_]*\b/g, (n) => resolve(n))
  // 递归下降
  let i = 0
  function skipWs() {
    while (i < s.length && /\s/.test(s[i])) i++
  }
  function parseOr() {
    let v = parseAnd()
    skipWs()
    while (s.startsWith('||', i)) {
      i += 2
      const r = parseAnd()
      v = v || r
      skipWs()
    }
    return v
  }
  function parseAnd() {
    let v = parseEq()
    skipWs()
    while (s.startsWith('&&', i)) {
      i += 2
      const r = parseEq()
      v = v && r
      skipWs()
    }
    return v
  }
  function parseEq() {
    let v = parseRel()
    skipWs()
    while (s.startsWith('==', i) || s.startsWith('!=', i)) {
      const op = s[i] === '=' ? '==' : '!='
      i += 2
      const r = parseRel()
      v = op === '==' ? v === r : v !== r
      skipWs()
    }
    return v
  }
  function parseRel() {
    let v = parseUnary()
    skipWs()
    while (/^[<>]/.test(s[i] || '')) {
      let op = s[i]
      if (s[i + 1] === '=') {
        op += '='
        i++
      }
      i++
      const r = parseUnary()
      if (op === '<') v = v < r
      else if (op === '>') v = v > r
      else if (op === '<=') v = v <= r
      else v = v >= r
      skipWs()
    }
    return v
  }
  function parseUnary() {
    skipWs()
    if (s[i] === '!') {
      i++
      return !parseUnary()
    }
    return parseAtom()
  }
  function parseAtom() {
    skipWs()
    if (s[i] === '(') {
      i++
      const v = parseOr()
      skipWs()
      i++ // )
      return v
    }
    const m = /^-?\d+(\.\d+)?/.exec(s.slice(i))
    if (m) {
      i += m[0].length
      return Number(m[0])
    }
    return false
  }
  return parseOr()
}

// 行级预处理：展开 #include、按 combo 裁剪 #if 块
function preprocess(src, combos, includeResolver, depth) {
  const lines = src.split('\n')
  const out = []
  const stack = [] // { parent, hit }（不支持 #elif；#else 取反 hit）
  const defs = new Map()
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]
    const t = line.trim()
    if (t.startsWith('#include')) {
      if (allActive(stack)) {
        const m = /^#include[ \t]+"([^"]+)"|^#include[ \t]+<([^>]+)>/.exec(t)
        const file = m && (m[1] || m[2])
        const inc = file && includeResolver ? includeResolver(file) : null
        if (inc !== null && inc !== undefined) {
          out.push(preprocess(inc, combos, includeResolver, depth + 1))
        } else {
          out.push('// [include 缺失: ' + file + ']')
        }
      }
      continue
    }
    if (t.startsWith('#define')) {
      if (allActive(stack)) {
        out.push(line)
        const m = /^#define[ \t]+([A-Za-z_][A-Za-z0-9_]*)(?:\([^)]*\))?[ \t]*(.*)$/.exec(t)
        if (m) defs.set(m[1], m[2].trim())
      }
      continue
    }
    if (t.startsWith('#undef')) {
      if (allActive(stack)) out.push(line)
      continue
    }
    if (t.startsWith('#ifdef') || t.startsWith('#ifndef') || t.startsWith('#if')) {
      const parent = allActive(stack)
      let cond = false
      if (t.startsWith('#ifdef')) {
        const name = t.slice(6).trim().split(/\s+/)[0]
        cond = combos[name] !== undefined || defs.has(name)
      } else if (t.startsWith('#ifndef')) {
        const name = t.slice(7).trim().split(/\s+/)[0]
        cond = !(combos[name] !== undefined || defs.has(name))
      } else {
        try {
          cond = !!evalIfExpr(t.slice(3).trim(), combos, defs)
        } catch (e) {
          cond = false
        }
      }
      stack.push({ parent, hit: cond })
      continue
    }
    if (t.startsWith('#else')) {
      if (stack.length > 0) stack[stack.length - 1].hit = !stack[stack.length - 1].hit
      continue
    }
    if (t.startsWith('#elif')) {
      // 不支持 #elif（WE shader 未使用）：当作终止
      continue
    }
    if (t.startsWith('#endif')) {
      if (stack.length > 0) stack.pop()
      continue
    }
    if (t.startsWith('#')) {
      continue
    }
    if (allActive(stack)) out.push(line)
  }
  return out.join('\n')
}

function allActive(stack) {
  return stack.every((s) => s.parent && s.hit)
}

// ---------- 方言语法转换 ----------

// 平衡括号内内容（返回右括号位置）
function matchParen(text, openIdx) {
  let depth = 0
  for (let i = openIdx; i < text.length; i++) {
    if (text[i] === '(') depth++
    else if (text[i] === ')') {
      depth--
      if (depth === 0) return i
    }
  }
  return -1
}

// 替换 callName(args) 形式（嵌套安全；callName 前必须是词边界，避免误匹配 Desaturate→saturate 之类）
function rewriteCall(text, callName, fn) {
  let out = ''
  let i = 0
  while (i < text.length) {
    const idx = text.indexOf(callName, i)
    if (idx === -1) {
      out += text.slice(i)
      break
    }
    // 词边界：前一个字符不能是标识符字符
    if (idx > 0 && /[A-Za-z0-9_]/.test(text[idx - 1])) {
      out += text.slice(i, idx + 1)
      i = idx + 1
      continue
    }
    out += text.slice(i, idx)
    let q = idx + callName.length
    while (q < text.length && /\s/.test(text[q])) q++
    if (text[q] !== '(') {
      out += text.slice(idx, q)
      i = q
      continue
    }
    const end = matchParen(text, q)
    if (end === -1) {
      out += text.slice(idx)
      break
    }
    const inner = text.slice(q + 1, end)
    out += fn(inner, idx)
    i = end + 1
  }
  return out
}

export { preprocess }

export function hlsl2glsl(src, stage, combos, includeResolver) {
  // combo 默认值：WE 语义 = 未显式提供时用声明里的 default（无声明 → 0）
  // （依据 linux-wallpaperengine ShaderUnit.cpp:442-477 parseComboConfiguration）
  const defaults = {}
  const comboRe = /\[COMBO\][^\n]*?"combo"\s*:\s*"([^"]+)"[^\n]*?"default"\s*:\s*(-?\d+)/g
  let cm
  while ((cm = comboRe.exec(src)) !== null) defaults[cm[1]] = Number(cm[2])
  const effective = { ...defaults, ...(combos || {}) }
  let code = preprocess(src, effective, includeResolver, 0)  // 展开本文件保留的宏（#define 行仍在，GLSL 预处理器会展开；但函数宏在 GLSL ES 也支持，
  // 为稳妥起见用 JS 预展开，然后移除 #define 行）
  code = expandMacrosIn(code, 20)
  code = code.replace(/^[ \t]*#define[^\n]*\n?/gm, '')

  // 代码中作为标识符使用的 combo（如 ApplyBlending(BLENDMODE, ...)）替换为数值；未定义 combo 用声明 default，无声明 = 0
  // 从 [COMBO] 注释提取全部 combo 名（含未提供的）
  {
    const comboNames = new Set()
    const comboRe2 = /\[COMBO\][^\n]*"combo"\s*:\s*"([^"]+)"/g
    let c2
    while ((c2 = comboRe2.exec(code)) !== null) comboNames.add(c2[1])
    for (const name of comboNames) {
      const v = effective[name] !== undefined ? effective[name] : 0
      code = replaceWord(code, name, String(v))
    }
  }

  // GLSL ES 3.0 保留字（WE 变量名与之冲突）
  code = replaceWord(code, 'sample', 'smp')

  // 数值后缀 f/h
  code = code.replace(/(\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)[fh]\b/g, '$1')

  // 类型名
  code = code.replace(/\bfloat4x4\b/g, 'mat4')
    .replace(/\bfloat3x3\b/g, 'mat3')
    .replace(/\bfloat2x2\b/g, 'mat2')
    .replace(/\bfloat4\b/g, 'vec4')
    .replace(/\bfloat3\b/g, 'vec3')
    .replace(/\bfloat2\b/g, 'vec2')
    .replace(/\bhalf4\b/g, 'vec4')
    .replace(/\bhalf3\b/g, 'vec3')
    .replace(/\bhalf2\b/g, 'vec2')
    .replace(/\bhalf\b/g, 'float')

  // 纹理采样与饱和
  code = code.replace(/\btexSample2DLod\b/g, 'textureLod').replace(/\btexSample2D\b/g, 'texture')
  code = rewriteCall(code, 'saturate', (inner) => 'clamp(' + inner + ', 0.0, 1.0)')
  code = rewriteCall(code, 'CAST4', (inner) => 'vec4(' + inner + ')')
  code = rewriteCall(code, 'CAST3', (inner) => 'vec3(' + inner + ')')
  code = rewriteCall(code, 'CAST2', (inner) => 'vec2(' + inner + ')')

  // HLSL 标量广播：max(0, x.rgb) → max(vec3(0), x.rgb)（同样 min）
  code = code.replace(/\b(max|min)\(\s*(-?\d+(?:\.\d+)?)\s*,\s*([A-Za-z_]\w*\.(rgb|xyz|rg|xy|r|x))\s*\)/g, (all, fn, num, expr, sw) => {
    const dim = sw.length
    return fn + '(vec' + dim + '(' + num + '), ' + expr + ')'
  })

  // HLSL 隐式 int→float 转换：乘除两侧的整数字面量补 .0（WE 效果 shader 中此类仅出现在 float 上下文）
  // 左侧字面量需排除标识符尾部数字（如 diffx1 * diffy2 不得改写成 diffx1.0）
  code = code.replace(/(^|[^\w.])(\d+)\s*([*/])\s*([A-Za-z_][A-Za-z0-9_]*)/g, '$1$2.0 $3 $4')
  code = code.replace(/\b([A-Za-z_][A-Za-z0-9_]*)\s*([*/])\s*(\d+)(?![\d.])/g, '$1 $2 $3.0')
  // 字面量 × 字面量（如 3.14159 * 2）
  code = code.replace(/(\d+\.\d+)\s*([*/])\s*(\d+)(?![\d.])/g, '$1 $2 $3.0')
  code = code.replace(/(^|[^\w.])(\d+)\s*([*/])\s*(\d+\.\d+)/g, '$1$2.0 $3 $4')

  // + / - 的隐式 int→float（GLSL 无此隐式转换，WE HLSL 有）：
  // 仅当可证明浮点上下文时转换——左侧为浮点字面量（2.0 - 1）或 swizzle 表达式（x.xyz - 1），
  // 以及左侧整数字面量、右侧为浮点字面量或 swizzle 表达式（1 + 2.0 / 1 + x.xyz）。
  code = code.replace(/(\.\d+)\s*([+-])\s*(\d+)(?![\d.])/g, '$1 $2 $3.0')
  code = code.replace(/([A-Za-z_]\w*\.(?:xyzw|xyz|xy|zw|rgba|rgb|rg|x|y|z|w|r|g|b|a))\s*([+-])\s*(\d+)(?![\d.])/g, '$1 $2 $3.0')
  code = code.replace(/(^|[^\w.])(\d+)\s*([+-])\s*(\d+\.\d+)/g, '$1$2.0 $3 $4')
  code = code.replace(/(^|[^\w.])(\d+)\s*([+-])\s*([A-Za-z_]\w*\.(?:xyzw|xyz|xy|zw|rgba|rgb|rg|x|y|z|w|r|g|b|a))/g, '$1$2.0 $3 $4')
  // 整数字面量 ± 浮点类型变量（如 1 - g_Rough、1 + time）：
  // 收集声明为 float/vec/mat 的 uniform 与局部变量名，仅对这些名字补 .0（int 变量不受影响）
  {
    const floatNames = new Set()
    const declRe = /\b(?:uniform\s+)?(?:highp|mediump|lowp\s+)?(?:float|vec2|vec3|vec4|mat2|mat3|mat4)\s+([A-Za-z_][A-Za-z0-9_]*)/g
    let dm
    while ((dm = declRe.exec(code)) !== null) floatNames.add(dm[1])
    if (floatNames.size > 0) {
      const alt = Array.from(floatNames).sort((a, b) => b.length - a.length).join('|')
      code = code.replace(new RegExp('(^|[^\\w.])(\\d+)\\s*([+-])\\s*(' + alt + ')(?![A-Za-z0-9_])', 'g'), '$1$2.0 $3 $4')
    }
  }

  // GLSL 内置 float 函数的实参中不允许裸 int（无隐式转换）：smoothstep 等调用内的整数字面量补 .0
  code = code.replace(/smoothstep\([^)]*\)/g, (call) => call.replace(/(?<![A-Za-z0-9_.])(\d+)(?![.\d])/g, '$1.0'))

  // mul(a, b)：HLSL 行向量语义
  // 收集矩阵类型 uniform/局部变量名
  const matNames = new Set()
  const matRe = /\b(?:uniform\s+)?(?:mat4|mat3|mat2|float4x4)\s+([A-Za-z_][A-Za-z0-9_]*)/g
  let mm
  while ((mm = matRe.exec(code)) !== null) matNames.add(mm[1])
  code = rewriteCall(code, 'mul', (inner) => {
    const args = splitArgs(inner)
    if (args.length !== 2) return 'mul(' + inner + ')'
    const a = args[0].trim()
    const b = args[1].trim()
    const isMat = (s) => matNames.has(s.split(/[\[.\s]/)[0]) || /^mat[234]\(/.test(s)
    if (isMat(a) && isMat(b)) return 'transpose(' + b + ') * transpose(' + a + ')'
    if (isMat(b)) return 'transpose(' + b + ') * ' + a
    return a + ' * ' + b
  })

  // lerp → mix；frac → fract（HLSL 名）
  code = code.replace(/\blerp\b/g, 'mix').replace(/\bfrac\b/g, 'fract')

  // varying/attribute → in/out
  if (stage === 'vert') {
    code = code.replace(/\battribute\b/g, 'in').replace(/\bvarying\b/g, 'out')
  } else {
    code = code.replace(/\bvarying\b/g, 'in').replace(/\battribute\b/g, 'in')
  }

  // HLSL 属性/修饰符
  code = code.replace(/\[(?:unroll|loop|branch|flatten)\]\s*/g, '')
  code = code.replace(/\bstatic\s+/g, '')

  // 输出：float 精度统一 highp（顶点默认即 highp；片元若用 mediump 会与顶点共享 uniform 精度不一致导致链接失败）
  let prologue = '#version 300 es\n'
  if (stage === 'vert') {
    prologue += 'precision highp float;\n'
  }
  if (stage === 'frag') {
    prologue += 'precision highp float;\n'
    if (/\bgl_FragColor\b/.test(code)) {
      prologue += 'out vec4 fragColor;\n'
      code = code.replace(/\bgl_FragColor\b/g, 'fragColor')
    }
  }
  return prologue + code
}
