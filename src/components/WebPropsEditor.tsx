// WE 网页壁纸用户属性编辑器：
// 读取 project.json general.properties（经 Rust 线格式转换 + 文案本地化），按类型渲染控件，
// 按 condition 表达式对当前草稿求值决定显隐，
// 保存差量覆盖值（与默认值不同的属性）到 settings，原生侧同时对已应用窗口热更新。
import { useCallback, useEffect, useState } from "react";
import { api, type WebPropDef, type WebPropValues } from "../api/steam";
import { evalCondition } from "../lib/weCondition";

/// WE 线格式 "r g b"（0..1 浮点）→ #rrggbb
function rgbStrToHex(s: string): string {
  const parts = s.trim().split(/\s+/).map(Number);
  if (parts.length !== 3 || parts.some((n) => Number.isNaN(n))) return "#000000";
  const h = (v: number) =>
    Math.max(0, Math.min(255, Math.round(v * 255)))
      .toString(16)
      .padStart(2, "0");
  return `#${h(parts[0])}${h(parts[1])}${h(parts[2])}`;
}

/// #rrggbb → WE 线格式 "r g b"（6 位小数，与 project.json 精度一致）
function hexToRgbStr(hex: string): string | null {
  let m = hex.replace("#", "");
  if (m.length === 3) m = m.split("").map((c) => c + c).join("");
  const n = parseInt(m, 16);
  if (Number.isNaN(n)) return null;
  const f = (v: number) => Number(v.toFixed(6));
  return `${f(((n >> 16) & 255) / 255)} ${f(((n >> 8) & 255) / 255)} ${f((n & 255) / 255)}`;
}

const sameValue = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

export function WebPropsEditor({ itemId }: { itemId: string }) {
  const [defs, setDefs] = useState<WebPropDef[]>([]);
  const [state, setState] = useState<"loading" | "ready" | "hidden">("loading");
  const [draft, setDraft] = useState<WebPropValues>({});
  const [saving, setSaving] = useState(false);
  const [msg, setMsg] = useState("");

  const load = useCallback(async () => {
    try {
      const list = await api.libraryItemProps(itemId);
      setDefs(list);
      setDraft(
        Object.fromEntries(
          list.filter((p) => p.value !== null).map((p) => [p.name, p.value as WebPropValues[string]])
        )
      );
      setState(list.length > 0 ? "ready" : "hidden");
    } catch {
      setState("hidden"); // 非 web 类型/读取失败：不显示
    }
  }, [itemId]);

  useEffect(() => {
    setState("loading");
    load();
  }, [load]);

  if (state === "loading") return null;
  if (state === "hidden") return null;

  // WE 语义：condition 不成立的属性在界面上隐藏（值仍由 Rust 侧照常下发给壁纸）。
  // 依赖当前草稿求值，故拖动开关时被依赖项立刻显隐
  const visible = defs.filter((p) => evalCondition(p.condition, draft));
  const dirty = defs.some((p) => p.value !== null && !sameValue(draft[p.name], p.value));

  const save = async () => {
    setSaving(true);
    setMsg("");
    try {
      // 只提交与默认值不同的属性（即覆盖集）；全部恢复默认 → 空对象 = 清除覆盖。
      // 遍历 defs 而非 visible：条件隐藏的属性已有的覆盖值不能因隐藏而丢失
      const overrides: WebPropValues = {};
      for (const p of defs) {
        if (p.value === null) continue;
        if (!sameValue(draft[p.name], p.default)) {
          overrides[p.name] = draft[p.name];
        }
      }
      await api.librarySetItemProps(itemId, overrides);
      setMsg("✅ 已保存并实时生效");
      await load();
    } catch (e) {
      setMsg(String(e));
    } finally {
      setSaving(false);
    }
  };

  const resetAll = async () => {
    setSaving(true);
    setMsg("");
    try {
      await api.libraryResetItemProps(itemId);
      setMsg("↺ 已恢复默认");
      await load();
    } catch (e) {
      setMsg(String(e));
    } finally {
      setSaving(false);
    }
  };

  // file 类型属性：系统文件选择器 → 拷入壁纸目录 → 立即生效（Rust 侧热更新）
  const onFilePick = async (p: WebPropDef) => {
    setMsg("");
    try {
      const r = await api.librarySetItemPropFile(itemId, p.name);
      if (!r.cancelled && r.value) {
        setMsg(`✅ 已更新文件：${r.value}`);
        await load();
      }
    } catch (e) {
      setMsg(String(e));
    }
  };

  return (
    <div className="card mt-5 p-5">
      <div className="flex items-center justify-between">
        <div className="flex items-baseline gap-2">
          <div className="text-[13px] font-semibold">壁纸属性</div>
          <span className="text-[11.5px] text-[var(--text-2)]">
            {visible.length < defs.length
              ? `${visible.length} 项（${defs.length - visible.length} 项因条件隐藏）`
              : `${defs.length} 项`}
          </span>
        </div>
        <div className="flex items-center gap-2">
          {msg && <span className="text-[12px] text-[var(--text-2)]">{msg}</span>}
          <button className="btn !py-1 !text-[12px]" disabled={saving} onClick={resetAll}>
            ↺ 恢复默认
          </button>
          <button
            className="btn btn-primary !py-1 !text-[12px]"
            disabled={saving || !dirty}
            onClick={save}
          >
            {saving ? "…" : "保存"}
          </button>
        </div>
      </div>
      <div className="mt-3 flex flex-col gap-3">
        {visible.map((p) => (
          <PropRow
            key={p.name}
            p={p}
            draft={draft}
            onChange={(v) => setDraft({ ...draft, [p.name]: v })}
            onFilePick={onFilePick}
          />
        ))}
      </div>
    </div>
  );
}

function PropRow({
  p,
  draft,
  onChange,
  onFilePick,
}: {
  p: WebPropDef;
  draft: WebPropValues;
  onChange: (v: WebPropValues[string]) => void;
  onFilePick?: (p: WebPropDef) => Promise<void>;
}) {
  const cur = draft[p.name];
  const isDefault = sameValue(cur, p.default);
  const controlCls =
    "rounded-lg border border-[var(--separator)] bg-[var(--card)] px-2.5 py-1.5 text-[12.5px] focus:outline-none focus:ring-1 focus:ring-[var(--accent)]";

  // WE 的 text 是静态说明/分节标题（不可编辑，多数连 value 都没有），
  // textinput 才是文本输入框。整行跨栏展示；长段落（真实数据有 234 字符的作者说明）
  // 按正文行高换行，并与上方控件留出分隔感
  if (p.ptype === "text") {
    const label = p.text || p.name;
    return (
      <div className="mt-1 border-t border-[var(--separator)] pt-2.5 text-[12px] leading-relaxed text-[var(--text-2)]">
        {label}
      </div>
    );
  }

  // 条件成立的选项才可选（选项级 condition 与属性级同样按草稿求值）
  const options = p.options.filter((o) => evalCondition(o.condition, draft));

  return (
    <div className="flex items-center gap-3">
      <div className="w-44 shrink-0 truncate text-[12.5px] text-[var(--text-2)]" title={p.name}>
        {p.text || p.name}
        {!isDefault && <span className="ml-1 text-[var(--accent)]" title="已自定义">•</span>}
      </div>
      <div className="flex flex-1 items-center gap-2">
        {p.ptype === "color" && (
          <>
            <input
              type="color"
              className="h-7 w-10 cursor-pointer rounded border border-[var(--separator)] bg-transparent"
              value={rgbStrToHex(String(cur ?? "0 0 0"))}
              onChange={(e) => {
                const v = hexToRgbStr(e.target.value);
                if (v !== null) onChange(v);
              }}
            />
            <span className="text-[11.5px] text-[var(--text-2)]">{rgbStrToHex(String(cur ?? ""))}</span>
          </>
        )}
        {p.ptype === "bool" && (
          <button
            role="switch"
            aria-checked={!!cur}
            className={`relative h-5.5 w-10 rounded-full transition-colors ${
              cur ? "bg-[var(--accent)]" : "bg-[var(--separator)]"
            }`}
            style={{ height: 22 }}
            onClick={() => onChange(!cur)}
          >
            <span
              className="absolute top-0.5 h-[18px] w-[18px] rounded-full bg-white shadow transition-all"
              style={{ left: cur ? 20 : 2 }}
            />
          </button>
        )}
        {p.ptype === "slider" && (
          <>
            <input
              type="range"
              className="flex-1 accent-[var(--accent)]"
              min={p.min ?? 0}
              max={p.max ?? 1}
              step={p.step ?? 0.01}
              value={Number(cur ?? 0)}
              onChange={(e) => onChange(Number(e.target.value))}
            />
            <span className="w-12 text-right text-[11.5px] text-[var(--text-2)]">
              {Number(cur ?? 0).toFixed(2)}
            </span>
          </>
        )}
        {p.ptype === "combo" && (
          <select
            className={controlCls}
            value={String(cur ?? "")}
            onChange={(e) => {
              // 回填 project.json 声明的原始类型：壁纸里的 === / switch 依赖数字/布尔而非字符串
              const hit = options.find((o) => String(o.value) === e.target.value);
              if (hit) onChange(hit.value);
            }}
          >
            {options.map((o) => (
              <option key={String(o.value)} value={String(o.value)}>
                {o.label}
              </option>
            ))}
          </select>
        )}
        {p.ptype === "textinput" && (
          <input
            type="text"
            className={`${controlCls} w-full`}
            value={String(cur ?? "")}
            onChange={(e) => onChange(e.target.value)}
          />
        )}
        {p.ptype === "file" && (
          <>
            <span className="min-w-0 flex-1 truncate text-[11.5px] text-[var(--text-2)]" title={cur ? String(cur) : undefined}>
              {cur ? String(cur) : "未设置"}
            </span>
            <button
              className="btn !py-1 !text-[11.5px]"
              onClick={() => onFilePick?.(p)}
            >
              选择文件…
            </button>
          </>
        )}
        {/* 未支持的类型（directory/group 等）只读展示当前值 */}
        {!["color", "bool", "slider", "combo", "textinput", "file"].includes(p.ptype) && (
          <span className="text-[11.5px] text-[var(--text-2)]">{String(cur ?? "")}</span>
        )}
        {!isDefault && (
          <button
            className="text-[11px] text-[var(--text-2)] hover:text-[var(--accent)]"
            title="恢复该属性默认值"
            onClick={() => p.default !== null && onChange(p.default)}
          >
            ↺
          </button>
        )}
      </div>
    </div>
  );
}
