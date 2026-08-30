// WE 风格的壁纸自定义属性弹窗（对标 Wallpaper Engine「编辑壁纸属性」窗口）：
// - 打开时读取 project.json general.properties（library_item_props，含本地化文案/wire 值）
//   —— 没有声明可自定义项时明确展示「不可自定义」状态
// - 按类型渲染控件（color/bool/slider/combo/textinput/file，text 为分节标题），
//   按 condition 对当前草稿求值决定显隐（与 WE 一致：隐藏项的值仍下发）
// - 交互复刻 WE：改动即生效 —— 500ms 防抖自动保存，关闭前冲刷未保存草稿
// - 支持属性搜索（真实壁纸常有 100+ 项）、单项恢复默认、全部恢复默认
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, type WebPropDef, type WebPropValues } from "../api/steam";
import { evalCondition } from "../lib/weCondition";
import { refreshItemPropsCache } from "../hooks/useItemProps";
import { ConfirmModal } from "./ConfirmModal";
import { IconSliders } from "./icons";

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

type SaveState = "idle" | "pending" | "saving" | "saved" | "error";

const SAVE_DEBOUNCE_MS = 500;

export function WallpaperPropsModal({
  itemId,
  title,
  onClose,
}: {
  itemId: string;
  title: string;
  onClose: () => void;
}) {
  const [defs, setDefs] = useState<WebPropDef[] | null>(null); // null = 读取中
  const [loadError, setLoadError] = useState("");
  const [draft, setDraft] = useState<WebPropValues>({});
  const [query, setQuery] = useState("");
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [errMsg, setErrMsg] = useState("");
  const [confirmReset, setConfirmReset] = useState(false);

  const draftRef = useRef(draft);
  draftRef.current = draft;
  const defsRef = useRef<WebPropDef[] | null>(null);
  defsRef.current = defs;
  const saveTimer = useRef<number | null>(null);

  // ---- 载入属性定义（每次打开都重新拉，值必须是最新的；成功后刷新可用性缓存） ----
  useEffect(() => {
    let alive = true;
    setDefs(null);
    setLoadError("");
    api
      .libraryItemProps(itemId)
      .then((list) => {
        if (!alive) return;
        setDefs(list);
        setDraft(
          Object.fromEntries(
            list.filter((p) => p.value !== null).map((p) => [p.name, p.value as WebPropValues[string]])
          )
        );
        refreshItemPropsCache(itemId);
      })
      .catch((e) => alive && setLoadError(String(e)));
    return () => {
      alive = false;
    };
  }, [itemId]);

  // ---- 保存：只提交与默认值不同的属性（覆盖集）；全部恢复默认 → 空对象 = 清除覆盖。
  // 遍历 defs 而非可见项：条件隐藏的属性已有的覆盖值不能因隐藏而丢失 ----
  const doSave = useCallback(async () => {
    const list = defsRef.current;
    if (!list) return;
    const d = draftRef.current;
    const overrides: WebPropValues = {};
    for (const p of list) {
      if (p.value === null) continue;
      if (!sameValue(d[p.name], p.default)) overrides[p.name] = d[p.name];
    }
    setSaveState("saving");
    try {
      await api.librarySetItemProps(itemId, overrides);
      setSaveState("saved");
      setErrMsg("");
      refreshItemPropsCache(itemId);
    } catch (e) {
      setSaveState("error");
      setErrMsg(String(e));
    }
  }, [itemId]);

  const scheduleSave = useCallback(() => {
    setSaveState("pending");
    if (saveTimer.current) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => {
      saveTimer.current = null;
      void doSave();
    }, SAVE_DEBOUNCE_MS);
  }, [doSave]);

  /** 立刻落盘防抖中的草稿（file 选择前调用，避免随后的重载丢失未保存更改） */
  const flushNow = useCallback(async () => {
    if (saveTimer.current) {
      window.clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    await doSave();
  }, [doSave]);

  // 卸载时冲刷未保存草稿（兜底；close() 已先行处理）
  useEffect(() => {
    return () => {
      if (saveTimer.current) {
        window.clearTimeout(saveTimer.current);
        saveTimer.current = null;
        void doSave();
      }
    };
  }, [doSave]);

  const change = useCallback(
    (name: string, v: WebPropValues[string]) => {
      setDraft((prev) => ({ ...prev, [name]: v }));
      scheduleSave();
    },
    [scheduleSave]
  );

  /** 重新拉取定义并重建草稿（reset / file 选择后调用） */
  const reload = useCallback(async () => {
    try {
      const list = await api.libraryItemProps(itemId);
      setDefs(list);
      setDraft(
        Object.fromEntries(
          list.filter((p) => p.value !== null).map((p) => [p.name, p.value as WebPropValues[string]])
        )
      );
      refreshItemPropsCache(itemId);
    } catch (e) {
      setSaveState("error");
      setErrMsg(String(e));
    }
  }, [itemId]);

  const resetAll = useCallback(async () => {
    setConfirmReset(false);
    // 丢弃防抖中的草稿，让「恢复默认」完整生效
    if (saveTimer.current) {
      window.clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    try {
      await api.libraryResetItemProps(itemId);
      setSaveState("saved");
      setErrMsg("");
      await reload();
    } catch (e) {
      setSaveState("error");
      setErrMsg(String(e));
    }
  }, [itemId, reload]);

  const pickFile = useCallback(
    async (p: WebPropDef) => {
      setErrMsg("");
      await flushNow();
      try {
        const r = await api.librarySetItemPropFile(itemId, p.name);
        if (!r.cancelled) await reload();
      } catch (e) {
        setSaveState("error");
        setErrMsg(String(e));
      }
    },
    [itemId, flushNow, reload]
  );

  /** 关闭前落盘草稿 */
  const close = useCallback(() => {
    if (saveTimer.current) {
      window.clearTimeout(saveTimer.current);
      saveTimer.current = null;
      void doSave();
    }
    onClose();
  }, [doSave, onClose]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !confirmReset) close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [close, confirmReset]);

  // ---- 派生：condition 显隐（按当前草稿求值，开关联动即时反映）+ 搜索过滤 ----
  const all = defs ?? [];
  const visible = useMemo(() => all.filter((p) => evalCondition(p.condition, draft)), [all, draft]);
  const q = query.trim().toLowerCase();
  const shown = useMemo(
    () =>
      q
        ? visible.filter(
            (p) => p.text.toLowerCase().includes(q) || p.name.toLowerCase().includes(q)
          )
        : visible,
    [visible, q]
  );
  const modifiedCount = useMemo(
    () => all.filter((p) => p.value !== null && !sameValue(draft[p.name], p.default)).length,
    [all, draft]
  );

  const showSearch = all.length > 6;
  const searchHint = q ? `匹配 ${shown.length} 项` : "";
  const infoBits = [
    `共 ${all.length} 项`,
    modifiedCount > 0 ? `已修改 ${modifiedCount} 项` : "",
    visible.length < all.length ? `${all.length - visible.length} 项因条件隐藏` : "",
  ].filter(Boolean);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-8" onClick={close}>
      <div
        className="card flex h-[78vh] w-full max-w-2xl flex-col overflow-hidden"
        onClick={(e) => e.stopPropagation()}
      >
        {/* 标题栏 */}
        <div className="flex shrink-0 items-center justify-between border-b border-[var(--separator)] px-5 py-3">
          <div className="min-w-0">
            <div className="flex items-center gap-2 text-[14px] font-semibold">
              <IconSliders size={15} />
              自定义壁纸属性
            </div>
            <div className="mt-0.5 truncate text-[11.5px] text-[var(--text-2)]" title={title}>
              {title}
            </div>
          </div>
          <button className="btn !py-1" onClick={close}>
            完成
          </button>
        </div>

        {loadError ? (
          <div className="flex flex-1 flex-col items-center justify-center gap-3 px-6 text-center">
            <div className="text-[13px] text-red-500">读取失败：{loadError}</div>
            <button className="btn" onClick={close}>
              关闭
            </button>
          </div>
        ) : defs === null ? (
          <div className="flex flex-1 items-center justify-center text-[13px] text-[var(--text-2)]">
            正在读取 project.json…
          </div>
        ) : all.length === 0 ? (
          <div className="flex flex-1 flex-col items-center justify-center gap-1.5 px-6 text-center">
            <div className="text-[13.5px] font-medium">该壁纸没有可自定义的配置项</div>
            <div className="text-[12px] text-[var(--text-2)]">
              project.json 未声明 general.properties，仅可整体应用
            </div>
          </div>
        ) : (
          <>
            {/* 工具条：搜索 + 统计 */}
            <div className="flex shrink-0 items-center gap-3 border-b border-[var(--separator)] px-5 py-2">
              {showSearch && (
                <input
                  type="text"
                  className="w-52 rounded-lg border border-[var(--separator)] bg-[var(--card)] px-2.5 py-1 text-[12px] placeholder:text-[var(--text-2)]/60 focus:outline-none focus:ring-1 focus:ring-[var(--accent)]"
                  placeholder="搜索属性…"
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                />
              )}
              <div className="ml-auto flex items-center gap-2 text-[11.5px] text-[var(--text-2)]">
                {searchHint && <span>{searchHint} ·</span>}
                <span className="truncate">{infoBits.join(" · ")}</span>
              </div>
            </div>

            {/* 属性列表 */}
            <div className="flex-1 overflow-y-auto px-5 py-3">
              {shown.length === 0 ? (
                <div className="py-10 text-center text-[12.5px] text-[var(--text-2)]">
                  没有匹配「{query}」的属性
                </div>
              ) : (
                <div className="flex flex-col gap-3">
                  {shown.map((p) => (
                    <PropRow
                      key={p.name}
                      p={p}
                      draft={draft}
                      onChange={change}
                      onFilePick={pickFile}
                    />
                  ))}
                </div>
              )}
            </div>

            {/* 底栏：保存状态 + 全部重置 */}
            <div className="flex shrink-0 items-center gap-3 border-t border-[var(--separator)] px-5 py-3">
              <span
                className={`truncate text-[12px] ${
                  saveState === "error" ? "text-red-500" : "text-[var(--text-2)]"
                }`}
              >
                {saveState === "error"
                  ? `保存失败：${errMsg}`
                  : saveState === "saving"
                    ? "正在保存…"
                    : saveState === "pending"
                      ? "更改即将自动生效…"
                      : saveState === "saved"
                        ? "✓ 已保存并实时生效"
                        : ""}
              </span>
              <div className="ml-auto flex items-center gap-2">
                <button
                  className="btn !py-1 !text-[12px]"
                  disabled={modifiedCount === 0}
                  onClick={() => setConfirmReset(true)}
                >
                  ↺ 恢复默认
                </button>
                <button className="btn btn-primary !py-1 !text-[12px]" onClick={close}>
                  完成
                </button>
              </div>
            </div>
          </>
        )}
      </div>

      {confirmReset && (
        <ConfirmModal
          title="恢复默认"
          message={`将清除该壁纸的全部自定义属性（${modifiedCount} 项），恢复 project.json 默认值。`}
          confirmText="恢复"
          onCancel={() => setConfirmReset(false)}
          onConfirm={resetAll}
        />
      )}
    </div>
  );
}

// ---------- 单个属性行 ----------

function PropRow({
  p,
  draft,
  onChange,
  onFilePick,
}: {
  p: WebPropDef;
  draft: WebPropValues;
  onChange: (name: string, v: WebPropValues[string]) => void;
  onFilePick: (p: WebPropDef) => Promise<void>;
}) {
  const cur = draft[p.name];
  const isDefault = sameValue(cur, p.default);
  const controlCls =
    "rounded-lg border border-[var(--separator)] bg-[var(--card)] px-2.5 py-1.5 text-[12.5px] focus:outline-none focus:ring-1 focus:ring-[var(--accent)]";

  // WE 的 text 是静态说明/分节标题（不可编辑）；group 是分组标题（带 text 的
  // 容器，真实数据 13 处）。两者都整行跨栏展示，充当分组分隔
  if (p.ptype === "text" || p.ptype === "group") {
    if (!p.text && p.ptype === "group") return null; // 无标题的空分组不占位
    return (
      <div
        className={`mt-1 border-t border-[var(--separator)] pt-2.5 leading-relaxed text-[var(--text-2)] ${
          p.ptype === "group"
            ? "text-[12.5px] font-semibold text-[var(--text-1)]"
            : "text-[12px] font-medium"
        }`}
      >
        {p.text || p.name}
      </div>
    );
  }

  return (
    <div className="flex items-center gap-3">
      <div className="w-44 shrink-0 truncate text-[12.5px] text-[var(--text-2)]" title={p.name}>
        {p.text || p.name}
        {!isDefault && (
          <span className="ml-1 text-[var(--accent)]" title="已自定义">
            •
          </span>
        )}
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
                if (v !== null) onChange(p.name, v);
              }}
            />
            <span className="text-[11.5px] text-[var(--text-2)]">{rgbStrToHex(String(cur ?? ""))}</span>
          </>
        )}
        {p.ptype === "bool" && (
          <button
            role="switch"
            aria-checked={!!cur}
            className={`relative h-[22px] w-10 rounded-full transition-colors ${
              cur ? "bg-[var(--accent)]" : "bg-[var(--separator)]"
            }`}
            onClick={() => onChange(p.name, !cur)}
          >
            <span
              className="absolute top-0.5 h-[18px] w-[18px] rounded-full bg-white shadow transition-all"
              style={{ left: cur ? 20 : 2 }}
            />
          </button>
        )}
        {p.ptype === "slider" && (
          <SliderControl p={p} value={cur} onChange={(v) => onChange(p.name, v)} />
        )}
        {p.ptype === "combo" && (
          <ComboControl p={p} cur={cur} draft={draft} controlCls={controlCls} onChange={(v) => onChange(p.name, v)} />
        )}
        {p.ptype === "textinput" && (
          <input
            type="text"
            className={`${controlCls} w-full`}
            value={String(cur ?? "")}
            onChange={(e) => onChange(p.name, e.target.value)}
          />
        )}
        {p.ptype === "file" && (
          <>
            <span
              className="min-w-0 flex-1 truncate text-[11.5px] text-[var(--text-2)]"
              title={cur ? String(cur) : undefined}
            >
              {cur ? String(cur) : "未设置"}
            </span>
            <button className="btn !py-1 !text-[11.5px]" onClick={() => void onFilePick(p)}>
              选择文件…
            </button>
          </>
        )}
        {/* directory：选择本地目录，存绝对路径（WE 语义）；不进壁纸站点目录故不补前缀 */}
        {p.ptype === "directory" && (
          <>
            <span
              className="min-w-0 flex-1 truncate text-[11.5px] text-[var(--text-2)]"
              title={cur ? String(cur) : undefined}
            >
              {cur ? String(cur) : "未设置"}
            </span>
            <button className="btn !py-1 !text-[11.5px]" onClick={() => void onFilePick(p)}>
              选择目录…
            </button>
          </>
        )}
        {/* 未支持的类型（scenetexture 等 scene 专属）只读展示当前值 */}
        {!["color", "bool", "slider", "combo", "textinput", "file", "directory"].includes(p.ptype) && (
          <span className="text-[11.5px] text-[var(--text-2)]">{String(cur ?? "")}</span>
        )}
        {!isDefault && p.default !== null && (
          <button
            className="text-[11px] text-[var(--text-2)] hover:text-[var(--accent)]"
            title="恢复该属性默认值"
            onClick={() => onChange(p.name, p.default as WebPropValues[string])}
          >
            ↺
          </button>
        )}
      </div>
    </div>
  );
}

// ---------- slider：拖动 + 可编辑数值（精度随 step） ----------

function SliderControl({
  p,
  value,
  onChange,
}: {
  p: WebPropDef;
  value: WebPropValues[string] | undefined;
  onChange: (v: number) => void;
}) {
  const min = p.min ?? 0;
  const max = p.max ?? 1;
  // 精度优先级：project.json step > precision 推导 > 缺省 0.01；
  // 显示小数位优先取 precision（真实数据 51 处声明，如 min=-100..100 precision=1）
  const precisionStep = p.precision !== undefined ? Math.pow(10, -p.precision) : undefined;
  const step = p.step ?? precisionStep ?? 0.01;
  const decimals = p.precision ?? (String(step).split(".")[1] ?? "").length;
  const clamp = (n: number) => Math.min(max, Math.max(min, n));
  const cur = clamp(Number(value ?? 0));
  // 编辑中的原始文本（null = 未在打字，直接显示格式化值）
  const [text, setText] = useState<string | null>(null);

  const commit = (raw: string) => {
    const n = Number(raw);
    if (raw.trim() !== "" && Number.isFinite(n)) onChange(clamp(n));
    setText(null);
  };

  return (
    <>
      <input
        type="range"
        className="min-w-0 flex-1 accent-[var(--accent)]"
        min={min}
        max={max}
        step={step}
        value={cur}
        onChange={(e) => onChange(Number(e.target.value))}
      />
      <input
        type="text"
        className="w-14 rounded-md border border-[var(--separator)] bg-[var(--card)] px-1.5 py-1 text-right text-[11.5px] focus:outline-none focus:ring-1 focus:ring-[var(--accent)]"
        value={text ?? cur.toFixed(decimals)}
        onChange={(e) => setText(e.target.value)}
        onBlur={(e) => commit(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") (e.target as HTMLInputElement).blur();
          if (e.key === "Escape") setText(null);
        }}
      />
    </>
  );
}

// ---------- combo：下拉（保留声明类型；选项级 condition 过滤） ----------

function ComboControl({
  p,
  cur,
  draft,
  controlCls,
  onChange,
}: {
  p: WebPropDef;
  cur: WebPropValues[string] | undefined;
  draft: WebPropValues;
  controlCls: string;
  onChange: (v: WebPropValues[string]) => void;
}) {
  const options = p.options.filter((o) => evalCondition(o.condition, draft));
  const hit = options.find((o) => sameValue(o.value, cur));
  return (
    <select
      className={controlCls}
      value={hit ? String(hit.value) : String(cur ?? "")}
      onChange={(e) => {
        // 回填 project.json 声明的原始类型：壁纸里的 === / switch 依赖数字/布尔而非字符串
        const opt = options.find((o) => String(o.value) === e.target.value);
        if (opt) onChange(opt.value);
      }}
    >
      {!hit && cur !== undefined && (
        <option value={String(cur)} disabled>
          {String(cur)}（当前值不在可选范围）
        </option>
      )}
      {options.map((o) => (
        <option key={String(o.value)} value={String(o.value)}>
          {o.label}
        </option>
      ))}
    </select>
  );
}
