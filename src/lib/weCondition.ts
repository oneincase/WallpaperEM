// Wallpaper Engine 属性显隐条件（project.json `condition`）求值器。
//
// 真实壁纸里 86% 的属性带 condition，形如：
//   clock_enable.value
//   background_enable.value && background_type.value == 1
//   clockdisplay.value != 'none' && foregroundenabled.value == true
//   (MENU_mode.value == 1 || MENU_mode.value == 3) && !(circle_blur.value)
//
// 只支持一个受限的纯表达式语法（无副作用、不用 eval）：
//   or      := and ( '||' and )*
//   and     := cmp ( '&&' cmp )*
//   cmp     := unary ( ('=='|'!='|'==='|'!=='|'>='|'<='|'>'|'<') unary )?
//   unary   := ('!' | '-') unary | primary
//   primary := '(' or ')' | ident '.value' | ident | number | string | true | false
//
// 解析失败一律「失败即显示」（fail open）。这是刻意的：真实数据里存在把 condition
// 当作带三元和赋值副作用的 JS 语句用的壁纸（往别的属性 .text 上赋值改配色），
// 这类表达式不该也无法在受限语法里支持，而误判隐藏比多显示一项糟糕得多。

/** 属性名 → 当前值（编辑器草稿）。缺失的属性求值为 undefined（falsy） */
export type ConditionValues = Record<string, string | number | boolean | undefined>;

type Node = (v: ConditionValues) => unknown;

// ---------- 词法 ----------

type Token = { k: "id" | "num" | "str" | "op"; v: string };

// 长算符必须排在前缀更短的同族算符之前（=== 先于 ==，!== 先于 !=，!= 先于 !）
const OPS = ["===", "!==", "&&", "||", "==", "!=", ">=", "<=", ">", "<", "!", "(", ")", ".", "-"];

function tokenize(src: string): Token[] {
  const out: Token[] = [];
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    if (c === " " || c === "\t" || c === "\n" || c === "\r") {
      i++;
      continue;
    }
    // 字符串：单/双引号，不支持转义（真实数据里没有）
    if (c === "'" || c === '"') {
      const end = src.indexOf(c, i + 1);
      if (end < 0) throw new Error("未闭合的字符串");
      out.push({ k: "str", v: src.slice(i + 1, end) });
      i = end + 1;
      continue;
    }
    if (c >= "0" && c <= "9") {
      let j = i;
      while (j < src.length && /[0-9.]/.test(src[j])) j++;
      out.push({ k: "num", v: src.slice(i, j) });
      i = j;
      continue;
    }
    if (/[A-Za-z_$]/.test(c)) {
      let j = i;
      while (j < src.length && /[A-Za-z0-9_$]/.test(src[j])) j++;
      out.push({ k: "id", v: src.slice(i, j) });
      i = j;
      continue;
    }
    const op = OPS.find((o) => src.startsWith(o, i));
    if (!op) throw new Error(`无法识别的字符 ${c}`);
    out.push({ k: "op", v: op });
    i += op.length;
  }
  return out;
}

// ---------- 值语义（对齐 JS，因为数据里 "1" 与 1 混用） ----------

/** JS 宽松相等（值域限定为 string | number | boolean | undefined | null） */
function looseEq(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (a === undefined || a === null || b === undefined || b === null) {
    return (a === undefined || a === null) && (b === undefined || b === null);
  }
  if (typeof a === typeof b) return false; // 同类型且 !== → 不等
  const na = Number(a);
  const nb = Number(b);
  return !Number.isNaN(na) && !Number.isNaN(nb) && na === nb;
}

/** 数值比较；任一侧非数值则为 false（与 JS 关系运算遇 NaN 一致） */
function numCmp(a: unknown, b: unknown, op: string): boolean {
  const x = Number(a);
  const y = Number(b);
  if (Number.isNaN(x) || Number.isNaN(y)) return false;
  switch (op) {
    case ">":
      return x > y;
    case "<":
      return x < y;
    case ">=":
      return x >= y;
    default:
      return x <= y;
  }
}

// ---------- 语法（直接编译成闭包，避免每次渲染重新解析） ----------

function parse(tokens: Token[]): Node {
  let pos = 0;
  const peek = () => tokens[pos];
  const eat = (v: string) => {
    const t = peek();
    if (!t || t.k !== "op" || t.v !== v) throw new Error(`期望 ${v}`);
    pos++;
  };

  function or(): Node {
    let left = and();
    while (peek()?.k === "op" && peek().v === "||") {
      pos++;
      const right = and();
      const l = left;
      left = (v) => !!l(v) || !!right(v);
    }
    return left;
  }

  function and(): Node {
    let left = cmp();
    while (peek()?.k === "op" && peek().v === "&&") {
      pos++;
      const right = cmp();
      const l = left;
      left = (v) => !!l(v) && !!right(v);
    }
    return left;
  }

  function cmp(): Node {
    const left = unary();
    const t = peek();
    if (t?.k === "op" && ["==", "!=", "===", "!==", ">", "<", ">=", "<="].includes(t.v)) {
      pos++;
      const right = unary();
      const op = t.v;
      // === / !== 也按宽松语义比。值经 project.json → Rust wire → JSON → JS 这条链路
      // 传递，整型/浮点/字符串表示可能与壁纸自身 JS 里看到的不完全一致；此处严格比较
      // 会把本该显示的属性误判为隐藏，而宽松比较最坏只是多显示一项（fail open 一致）
      if (op === "==" || op === "===") return (v) => looseEq(left(v), right(v));
      if (op === "!=" || op === "!==") return (v) => !looseEq(left(v), right(v));
      return (v) => numCmp(left(v), right(v), op);
    }
    return left;
  }

  function unary(): Node {
    const t = peek();
    if (t?.k === "op" && t.v === "!") {
      pos++;
      const inner = unary();
      return (v) => !inner(v);
    }
    if (t?.k === "op" && t.v === "-") {
      pos++;
      const inner = unary();
      return (v) => -Number(inner(v));
    }
    return primary();
  }

  function primary(): Node {
    const t = peek();
    if (!t) throw new Error("表达式意外结束");
    if (t.k === "op" && t.v === "(") {
      pos++;
      const inner = or();
      eat(")");
      return inner;
    }
    if (t.k === "num") {
      pos++;
      const n = Number(t.v);
      if (Number.isNaN(n)) throw new Error(`非法数字 ${t.v}`);
      return () => n;
    }
    if (t.k === "str") {
      pos++;
      const s = t.v;
      return () => s;
    }
    if (t.k === "id") {
      pos++;
      if (t.v === "true") return () => true;
      if (t.v === "false") return () => false;
      const name = t.v;
      // 只接受 `ident` 与 `ident.value`；`.text` 等成员（赋值语句里出现）走 fail open
      if (peek()?.k === "op" && peek().v === ".") {
        pos++;
        const m = peek();
        if (!m || m.k !== "id" || m.v !== "value") throw new Error("仅支持 .value");
        pos++;
      }
      return (v) => v[name];
    }
    throw new Error(`意外的 ${t.v}`);
  }

  const root = or();
  if (pos !== tokens.length) throw new Error("表达式有多余内容"); // 三元/赋值等落到这里
  return root;
}

// 编译结果缓存：拖动滑块时每帧要重算数百个条件，避免反复分词解析
const cache = new Map<string, ((v: ConditionValues) => boolean) | null>();

function compile(expr: string): ((v: ConditionValues) => boolean) | null {
  const hit = cache.get(expr);
  if (hit !== undefined) return hit;
  let fn: ((v: ConditionValues) => boolean) | null = null;
  try {
    const node = parse(tokenize(expr));
    fn = (v) => !!node(v);
  } catch {
    fn = null; // 不支持的语法 → 恒显示
  }
  cache.set(expr, fn);
  return fn;
}

/**
 * 求值属性显隐条件。无条件、空条件、语法不支持一律视为可见。
 */
export function evalCondition(expr: string | undefined, values: ConditionValues): boolean {
  if (!expr || !expr.trim()) return true;
  const fn = compile(expr);
  if (!fn) return true;
  try {
    return fn(values);
  } catch {
    return true;
  }
}
